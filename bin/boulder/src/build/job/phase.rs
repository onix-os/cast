// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use moss::util;
use std::{
    num::NonZeroUsize,
    path::{Component, Path},
};
use stone_recipe::{
    ToolchainSpec, UpstreamSpec,
    build_policy::{CompilerFlagsSpec, ContextValue, TargetPolicySpec, TextSpec},
    derivation::{ExecutablePlan, PhasePlan, RelationPlan, StepPlan},
    package::{BuilderEnvironmentSpec, PhaseSpec, ProgramSpec, StepSpec},
};
use tui::Styled;

use crate::build::{
    context::{BuildContext, PgoContextStage, TextContextOverlay, TypedContextInputs, freeze_policy_program},
    pgo,
};
use crate::{BuildPolicy, Paths, Recipe};

use super::{Error, work_dir};

pub fn list(pgo_stage: Option<pgo::Stage>) -> Vec<Phase> {
    if matches!(pgo_stage, Some(pgo::Stage::One | pgo::Stage::Two)) {
        Phase::WORKLOAD.to_vec()
    } else {
        Phase::NORMAL.to_vec()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, strum::Display)]
pub enum Phase {
    Prepare,
    Setup,
    Build,
    Install,
    Check,
    Workload,
}

pub(super) struct PlanContext<'a> {
    pub target: &'a TargetPolicySpec,
    pub pgo_stage: Option<pgo::Stage>,
    pub recipe: &'a Recipe,
    pub paths: &'a Paths,
    pub policy: &'a BuildPolicy,
    pub compiler_cache: bool,
    pub jobs: NonZeroUsize,
}

impl Phase {
    const NORMAL: &'static [Self] = &[Phase::Prepare, Phase::Setup, Phase::Build, Phase::Install, Phase::Check];
    const WORKLOAD: &'static [Self] = &[Phase::Prepare, Phase::Setup, Phase::Build, Phase::Workload];

    pub fn abbrev(&self) -> &str {
        match self {
            Phase::Prepare => "P",
            Phase::Setup => "S",
            Phase::Build => "B",
            Phase::Install => "I",
            Phase::Check => "C",
            Phase::Workload => "W",
        }
    }

    pub fn styled(&self, s: impl ToString) -> String {
        let s = s.to_string();
        // Taste the rainbow
        // TODO: Ikey plz make pretty
        match self {
            Phase::Prepare => s.grey(),
            Phase::Setup => s.cyan(),
            Phase::Build => s.blue(),
            Phase::Check => s.yellow(),
            Phase::Install => s.green(),
            Phase::Workload => s.magenta(),
        }
        .dim()
        .to_string()
    }

    pub(super) fn plan(&self, request: &PlanContext<'_>) -> Result<Option<PhasePlan>, Error> {
        let PlanContext {
            target,
            pgo_stage,
            recipe,
            paths,
            policy,
            compiler_cache,
            jobs,
        } = *request;
        let typed_phases = &recipe.build_target_builder(target).phases;
        let hooks = recipe.build_target_hooks(target);
        let empty_phase = PhaseSpec::default();
        let no_hooks: &[StepSpec] = &[];
        let (typed_phase, pre_hooks, post_hooks) = match self {
            Phase::Prepare => (&empty_phase, no_hooks, no_hooks),
            Phase::Setup => (
                &typed_phases.setup,
                hooks.pre_setup.as_slice(),
                hooks.post_setup.as_slice(),
            ),
            Phase::Build => (
                &typed_phases.build,
                hooks.pre_build.as_slice(),
                hooks.post_build.as_slice(),
            ),
            Phase::Install => (
                &typed_phases.install,
                hooks.pre_install.as_slice(),
                hooks.post_install.as_slice(),
            ),
            Phase::Check => (
                &typed_phases.check,
                hooks.pre_check.as_slice(),
                hooks.post_check.as_slice(),
            ),
            Phase::Workload => (
                &typed_phases.workload,
                hooks.pre_workload.as_slice(),
                hooks.post_workload.as_slice(),
            ),
        };

        let prepare = matches!(self, Phase::Prepare);
        if typed_phase.is_empty() && pre_hooks.is_empty() && post_hooks.is_empty() && !prepare {
            return Ok(None);
        }

        let build_target = &target.name;
        let build_dir = paths.build().guest.join(build_target);
        let work_dir = if matches!(self, Phase::Prepare) {
            build_dir.clone()
        } else {
            work_dir(&build_dir, &recipe.declaration.sources)
        };
        let flags = select_flags(target, pgo_stage, recipe, policy)?;
        let mut context = BuildContext::resolve(
            &policy.spec,
            target,
            TypedContextInputs {
                package_name: recipe.declaration.meta.pname.clone(),
                package_version: recipe.declaration.meta.version.clone(),
                package_release: recipe.declaration.meta.release,
                source_dir: paths.upstreams().guest.display().to_string(),
                install_root: paths.install().guest.display().to_string(),
                build_root: build_dir.display().to_string(),
                work_dir: work_dir.display().to_string(),
                pgo_dir: format!("{}-pgo", build_dir.display()),
                jobs: u32::try_from(jobs.get()).expect("supported jobs fit u32"),
                source_date_epoch: recipe.build_time.timestamp(),
                pgo_stage: pgo_context_stage(pgo_stage),
                toolchain: recipe.declaration.options.toolchain,
                compiler_cache_enabled: compiler_cache,
                mold_enabled: recipe.declaration.mold,
                flags,
            },
        )?;
        for environment in &recipe.build_target_builder(target).environment {
            let bindings = match environment {
                BuilderEnvironmentSpec::CMake => &policy.spec.builders.cmake.environment,
                BuilderEnvironmentSpec::Meson => &policy.spec.builders.meson.environment,
                BuilderEnvironmentSpec::Cargo => &policy.spec.builders.cargo.environment,
                BuilderEnvironmentSpec::Autotools => &policy.spec.builders.autotools.environment,
            };
            context.extend_environment(bindings)?;
        }
        let working_dir = if matches!(self, Phase::Prepare) {
            build_dir.display().to_string()
        } else {
            work_dir.display().to_string()
        };
        let pre = compile_steps(pre_hooks, &context, &working_dir)?;
        let mut steps = if prepare {
            prepare_steps(&recipe.declaration.sources, paths, &context, policy)?
        } else {
            compile_steps(&typed_phase.steps, &context, &working_dir)?
        };
        if matches!(self, Phase::Workload)
            && matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm)
            && let finish_steps = pgo_finish_steps(pgo_stage, &context, policy, &working_dir)?
            && !finish_steps.is_empty()
        {
            steps.extend(finish_steps);
        }
        let post = compile_steps(post_hooks, &context, &working_dir)?;
        if pre.is_empty() && steps.is_empty() && post.is_empty() {
            return Ok(None);
        }
        Ok(Some(PhasePlan {
            name: self.to_string(),
            pre,
            steps,
            post,
        }))
    }
}

fn compile_steps(typed_steps: &[StepSpec], context: &BuildContext, working_dir: &str) -> Result<Vec<StepPlan>, Error> {
    let mut steps = Vec::with_capacity(typed_steps.len());
    for step in typed_steps {
        match step {
            StepSpec::Run { program, args } => steps.push(StepPlan::Run {
                program: freeze_package_program(program)?,
                args: args.clone(),
                environment: context.environment.clone(),
                working_dir: working_dir.to_owned(),
            }),
            StepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            } => steps.push(StepPlan::Shell {
                interpreter: freeze_package_program(interpreter)?,
                declared_programs: declared_programs
                    .iter()
                    .map(freeze_package_program)
                    .collect::<Result<_, _>>()?,
                script: script.clone(),
                environment: context.environment.clone(),
                working_dir: working_dir.to_owned(),
            }),
            _ => {
                if let Some(step) = context.resolve_standard_step(step)? {
                    steps.push(step);
                }
            }
        }
    }
    Ok(steps)
}

fn freeze_package_program(program: &ProgramSpec) -> Result<ExecutablePlan, Error> {
    let dependency = program
        .requirement
        .dependency()
        .map_err(|source| Error::InvalidProgramRequirement { source })?;
    Ok(ExecutablePlan {
        path: program.path.clone(),
        requirement: RelationPlan::from(dependency),
    })
}

fn pgo_finish_steps(
    stage: Option<pgo::Stage>,
    context: &BuildContext,
    policy: &BuildPolicy,
    working_dir: &str,
) -> Result<Vec<StepPlan>, Error> {
    let stage = match stage {
        Some(pgo::Stage::One) => &policy.spec.pgo.stage_one,
        Some(pgo::Stage::Two) => &policy.spec.pgo.stage_two,
        Some(pgo::Stage::Use) | None => return Ok(Vec::new()),
    };
    let Some(finish) = &stage.finish else {
        return Ok(Vec::new());
    };

    let pgo_dir = context.resolve_text(&TextSpec::Context(ContextValue::PgoDir))?;
    let output = context.resolve_text(&finish.output)?;
    validate_pgo_path(&output, &pgo_dir)?;
    let inputs = finish
        .inputs
        .iter()
        .map(|input| {
            let input = context.resolve_text(input)?;
            validate_pgo_path(&input, &pgo_dir)?;
            Ok(input)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let copy_to = finish
        .copy_to
        .as_ref()
        .map(|copy_to| {
            let copy_to = context.resolve_text(copy_to)?;
            validate_pgo_path(&copy_to, &pgo_dir)?;
            Ok::<_, Error>(copy_to)
        })
        .transpose()?;

    let resolve = |value: &TextSpec| context.resolve_text(value).map_err(Error::from);
    let merge_args = policy
        .spec
        .pgo
        .merge_args
        .iter()
        .map(resolve)
        .collect::<Result<Vec<_>, _>>()?;
    let mut steps = Vec::new();
    if finish.remove_output_first {
        steps.push(StepPlan::Run {
            program: freeze_policy_program(&policy.spec.pgo.remove_program),
            args: vec![output.clone()],
            environment: context.environment.clone(),
            working_dir: working_dir.to_owned(),
        });
    }
    let mut merge = vec![shell_quote(&policy.spec.pgo.merge_program.path)];
    merge.extend(merge_args.iter().map(|argument| shell_quote(argument)));
    merge.push(shell_quote(&format!("-output={output}")));
    merge.extend(inputs.iter().map(|input| shell_glob(input)));
    steps.push(StepPlan::Shell {
        interpreter: freeze_policy_program(&policy.spec.pgo.shell_interpreter),
        declared_programs: vec![freeze_policy_program(&policy.spec.pgo.merge_program)],
        script: format!("set -euo pipefail\n{}", merge.join(" ")),
        environment: context.environment.clone(),
        working_dir: working_dir.to_owned(),
    });
    if let Some(copy_to) = copy_to {
        steps.push(StepPlan::Run {
            program: freeze_policy_program(&policy.spec.pgo.copy_program),
            args: vec![output, copy_to],
            environment: context.environment.clone(),
            working_dir: working_dir.to_owned(),
        });
    }

    Ok(steps)
}

fn validate_pgo_path(path: &str, pgo_dir: &str) -> Result<(), Error> {
    let relative = Path::new(path)
        .strip_prefix(pgo_dir)
        .map_err(|_| Error::UnsafePgoPath {
            path: path.to_owned(),
            pgo_dir: pgo_dir.to_owned(),
        })?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Error::UnsafePgoPath {
            path: path.to_owned(),
            pgo_dir: pgo_dir.to_owned(),
        });
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_glob(value: &str) -> String {
    let mut rendered = String::new();
    let mut literal = String::new();
    for character in value.chars() {
        if matches!(character, '*' | '?') {
            if !literal.is_empty() {
                rendered.push_str(&shell_quote(&literal));
                literal.clear();
            }
            rendered.push(character);
        } else {
            literal.push(character);
        }
    }
    if !literal.is_empty() || rendered.is_empty() {
        rendered.push_str(&shell_quote(&literal));
    }
    rendered
}

fn prepare_steps(
    sources: &[UpstreamSpec],
    paths: &Paths,
    context: &BuildContext,
    policy: &BuildPolicy,
) -> Result<Vec<StepPlan>, Error> {
    let mut steps = Vec::new();
    for source in sources {
        match source {
            UpstreamSpec::Archive {
                url,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
                ..
            } => {
                if !*unpack {
                    continue;
                }
                let file_name = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let rename = rename.as_deref().unwrap_or(file_name.as_str());
                let unpack_dir = unpack_dir.as_ref().cloned().unwrap_or_else(|| rename.to_owned());
                let strip_dirs = strip_dirs.unwrap_or(1);
                let overlay = TextContextOverlay {
                    source_path: Some(paths.upstreams().guest.join(rename).display().to_string()),
                    source_destination: Some(unpack_dir),
                    source_strip_components: Some(
                        u32::try_from(strip_dirs).expect("validated source strip_dirs fits u32"),
                    ),
                };

                steps.push(context.resolve_command(&policy.spec.sources.archive.create_directory, &overlay)?);
                steps.push(context.resolve_command(&policy.spec.sources.archive.unpack, &overlay)?);
            }
            UpstreamSpec::Git { url, clone_dir, .. } => {
                let source = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let target = clone_dir.as_ref().cloned().unwrap_or_else(|| source.to_owned());
                let overlay = TextContextOverlay {
                    source_path: Some(paths.upstreams().guest.join(source).display().to_string()),
                    source_destination: Some(target),
                    source_strip_components: None,
                };

                steps.push(context.resolve_command(&policy.spec.sources.git.create_directory, &overlay)?);
                steps.push(context.resolve_command(&policy.spec.sources.git.copy, &overlay)?);
            }
        }
    }
    Ok(steps)
}

fn select_flags(
    target: &TargetPolicySpec,
    pgo_stage: Option<pgo::Stage>,
    recipe: &Recipe,
    policy: &BuildPolicy,
) -> Result<CompilerFlagsSpec, Error> {
    let toolchain = recipe.declaration.options.toolchain;
    let mut selection =
        crate::build::tuning::resolve(&policy.spec.tuning, target, toolchain, &recipe.declaration.tuning)?;

    if let Some(stage) = pgo_stage {
        let stage = match stage {
            pgo::Stage::One => &policy.spec.pgo.stage_one,
            pgo::Stage::Two => &policy.spec.pgo.stage_two,
            pgo::Stage::Use => &policy.spec.pgo.use_profile,
        };
        crate::build::tuning::extend_toolchain_flags(&mut selection.flags, &stage.flags, toolchain);
        if matches!(pgo_stage, Some(pgo::Stage::Use)) && recipe.declaration.options.samplepgo {
            crate::build::tuning::extend_toolchain_flags(&mut selection.flags, &policy.spec.pgo.sample, toolchain);
        }
    }
    Ok(selection.flags)
}

fn pgo_context_stage(stage: Option<pgo::Stage>) -> PgoContextStage {
    match stage {
        None => PgoContextStage::None,
        Some(pgo::Stage::One) => PgoContextStage::One,
        Some(pgo::Stage::Two) => PgoContextStage::Two,
        Some(pgo::Stage::Use) => PgoContextStage::Use,
    }
}

#[cfg(test)]
mod direct_tests {
    use chrono::DateTime;
    use std::path::Path;
    use stone_recipe::{
        build_policy::{BuildToolSpec, EnvironmentBindingSpec, EnvironmentCondition},
        derivation::StepPlan,
        package::{
            BuilderEnvironmentSpec, BuilderSpec, DependencySpec, HooksSpec, PhaseSpec, PhasesSpec, ProfileSpec,
            ProgramSpec, StepSpec, SupportedHooksSpec,
        },
    };

    use super::*;
    use crate::{BuildPolicy, Paths, Recipe};

    fn fixture() -> (Recipe, BuildPolicy, tempfile::TempDir) {
        let recipe_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu");
        let recipe = Recipe::load_at(recipe_path, DateTime::from_timestamp(1_700_000_000, 0).unwrap()).unwrap();
        (
            recipe,
            BuildPolicy::repository_for_tests(),
            tempfile::tempdir().unwrap(),
        )
    }

    fn context_for(recipe: &Recipe, paths: &Paths, policy: &BuildPolicy, stage: Option<pgo::Stage>) -> BuildContext {
        let target_policy = policy.target("x86_64").unwrap();
        let target_build = paths.build().guest.join("x86_64");
        BuildContext::resolve(
            &policy.spec,
            target_policy,
            TypedContextInputs {
                package_name: recipe.declaration.meta.pname.clone(),
                package_version: recipe.declaration.meta.version.clone(),
                package_release: recipe.declaration.meta.release,
                source_dir: paths.upstreams().guest.display().to_string(),
                install_root: paths.install().guest.display().to_string(),
                build_root: target_build.display().to_string(),
                work_dir: target_build.display().to_string(),
                pgo_dir: format!("{}-pgo", target_build.display()),
                jobs: 2,
                source_date_epoch: recipe.build_time.timestamp(),
                pgo_stage: pgo_context_stage(stage),
                toolchain: recipe.declaration.options.toolchain,
                compiler_cache_enabled: false,
                mold_enabled: recipe.declaration.mold,
                flags: select_flags(target_policy, stage, recipe, policy).unwrap(),
            },
        )
        .unwrap()
    }

    fn test_paths(recipe: &Recipe, policy: &BuildPolicy, root: &Path) -> Paths {
        let layout = stone_recipe::derivation::BuilderLayout::from_policy(
            &policy.spec.sandbox,
            &policy.spec.build_root.compiler_cache,
        );
        Paths::new(recipe, layout, root, root).unwrap()
    }

    fn shell_script(step: &StepPlan) -> &str {
        match step {
            StepPlan::Shell { script, .. } => script,
            StepPlan::Run { .. } => panic!("expected an authored shell step"),
        }
    }

    fn shell_scripts(steps: &[StepPlan]) -> Vec<&str> {
        steps.iter().map(shell_script).collect()
    }

    fn authored_shell(script: &str) -> StepSpec {
        StepSpec::Shell {
            interpreter: ProgramSpec {
                path: "/usr/bin/bash".to_owned(),
                requirement: DependencySpec::Binary("bash".to_owned()),
            },
            declared_programs: Vec::new(),
            script: script.to_owned(),
        }
    }

    #[test]
    fn standard_steps_freeze_as_run_with_exact_context() {
        let (mut recipe, policy, root) = fixture();
        recipe.declaration.builder = BuilderSpec {
            required_tools: Vec::new(),
            environment: vec![BuilderEnvironmentSpec::CMake],
            phases: PhasesSpec {
                setup: PhaseSpec::new([StepSpec::CMakeConfigure {
                    flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
                }]),
                ..PhasesSpec::default()
            },
            supported_hooks: SupportedHooksSpec::all(),
        };
        let paths = test_paths(&recipe, &policy, root.path());
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Setup
            .plan(&PlanContext {
                target,
                pgo_stage: None,
                recipe: &recipe,
                paths: &paths,
                policy: &policy,
                compiler_cache: false,
                jobs: NonZeroUsize::new(3).unwrap(),
            })
            .unwrap()
            .unwrap();
        let StepPlan::Run {
            program,
            environment,
            working_dir,
            ..
        } = &plan.steps[0]
        else {
            panic!("standard builder step must be Run")
        };
        assert_eq!(program.path, "/usr/bin/cmake");
        assert_eq!(working_dir, "/mason/build/x86_64");
        assert_eq!(environment["BOULDER_PACKAGE_NAME"], "hello");
        assert_eq!(environment["BOULDER_JOBS"], "3");
        assert_eq!(environment["SOURCE_DATE_EPOCH"], "1700000000");
        assert_eq!(environment["CC"], "clang");
    }

    #[test]
    fn selected_profile_hooks_preserve_exact_phase_groups_and_order() {
        let (mut recipe, policy, root) = fixture();
        let shell = authored_shell;
        recipe.declaration.profiles = vec![ProfileSpec {
            name: "x86_64".to_owned(),
            builder: BuilderSpec {
                required_tools: Vec::new(),
                environment: Vec::new(),
                phases: PhasesSpec {
                    build: PhaseSpec::new([shell("body-one"), shell("body-two")]),
                    ..PhasesSpec::default()
                },
                supported_hooks: SupportedHooksSpec::all(),
            },
            hooks: HooksSpec {
                pre_build: vec![shell("pre-one"), shell("pre-two")],
                post_build: vec![shell("post-one"), shell("post-two")],
                ..HooksSpec::default()
            },
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        }];

        let paths = test_paths(&recipe, &policy, root.path());
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Build
            .plan(&PlanContext {
                target,
                pgo_stage: None,
                recipe: &recipe,
                paths: &paths,
                policy: &policy,
                compiler_cache: false,
                jobs: NonZeroUsize::new(2).unwrap(),
            })
            .unwrap()
            .unwrap();

        assert_eq!(shell_scripts(&plan.pre), ["pre-one", "pre-two"]);
        assert_eq!(shell_scripts(&plan.steps), ["body-one", "body-two"]);
        assert_eq!(shell_scripts(&plan.post), ["post-one", "post-two"]);
        let execution_order = plan
            .pre
            .iter()
            .chain(&plan.steps)
            .chain(&plan.post)
            .map(shell_script)
            .collect::<Vec<_>>();
        assert_eq!(
            execution_order,
            ["pre-one", "pre-two", "body-one", "body-two", "post-one", "post-two"]
        );
    }

    #[test]
    fn pgo_finish_stays_in_workload_body_before_post_hook() {
        let (mut recipe, policy, root) = fixture();
        recipe.declaration.builder = BuilderSpec {
            required_tools: Vec::new(),
            environment: Vec::new(),
            phases: PhasesSpec {
                workload: PhaseSpec::new([authored_shell("workload-body")]),
                ..PhasesSpec::default()
            },
            supported_hooks: SupportedHooksSpec::all(),
        };
        recipe.declaration.hooks = HooksSpec {
            pre_workload: vec![authored_shell("pre-workload")],
            post_workload: vec![authored_shell("post-workload")],
            ..HooksSpec::default()
        };

        let paths = test_paths(&recipe, &policy, root.path());
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Workload
            .plan(&PlanContext {
                target,
                pgo_stage: Some(pgo::Stage::One),
                recipe: &recipe,
                paths: &paths,
                policy: &policy,
                compiler_cache: false,
                jobs: NonZeroUsize::new(2).unwrap(),
            })
            .unwrap()
            .unwrap();

        assert_eq!(shell_scripts(&plan.pre), ["pre-workload"]);
        assert_eq!(plan.steps.len(), 3);
        let StepPlan::Shell { script, .. } = &plan.steps[0] else {
            panic!("authored workload must remain a shell step")
        };
        assert_eq!(script, "workload-body");
        let StepPlan::Shell { script, .. } = &plan.steps[1] else {
            panic!("PGO completion must be frozen into the workload body")
        };
        assert!(script.starts_with("set -euo pipefail\n"));
        let StepPlan::Run { program, .. } = &plan.steps[2] else {
            panic!("PGO profile copy must be a structural run step")
        };
        assert_eq!(program.path, "/usr/bin/cp");
        assert_eq!(shell_scripts(&plan.post), ["post-workload"]);
    }

    #[test]
    fn authored_shell_percent_text_is_literal() {
        let (mut recipe, policy, root) = fixture();
        let literal = "%cargo_fetch $BOULDER_INSTALL_ROOT %(jobs)";
        recipe.declaration.builder = BuilderSpec {
            required_tools: Vec::new(),
            environment: Vec::new(),
            phases: PhasesSpec {
                build: PhaseSpec::new([authored_shell(literal)]),
                ..PhasesSpec::default()
            },
            supported_hooks: SupportedHooksSpec::all(),
        };
        let paths = test_paths(&recipe, &policy, root.path());
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Build
            .plan(&PlanContext {
                target,
                pgo_stage: None,
                recipe: &recipe,
                paths: &paths,
                policy: &policy,
                compiler_cache: false,
                jobs: NonZeroUsize::new(2).unwrap(),
            })
            .unwrap()
            .unwrap();
        let StepPlan::Shell { script, .. } = &plan.steps[0] else {
            panic!("explicit shell must stay shell")
        };
        assert_eq!(script, literal);
    }

    #[test]
    fn selected_builder_environment_markers_apply_in_declared_order() {
        let (mut recipe, mut policy, root) = fixture();
        let binding = |name: &str, value: &str| EnvironmentBindingSpec {
            name: name.to_owned(),
            value: TextSpec::Literal(value.to_owned()),
            condition: EnvironmentCondition::Always,
        };
        policy.spec.builders.cmake.environment = vec![
            binding("BUILDER_ENVIRONMENT_ORDER", "cmake"),
            binding("CMAKE_MARKER", "present"),
        ];
        policy.spec.builders.cargo.environment = vec![
            binding("BUILDER_ENVIRONMENT_ORDER", "cargo"),
            binding("CARGO_MARKER", "present"),
        ];
        recipe.declaration.builder = BuilderSpec {
            required_tools: Vec::new(),
            environment: vec![BuilderEnvironmentSpec::CMake, BuilderEnvironmentSpec::Cargo],
            phases: PhasesSpec {
                build: PhaseSpec::new([authored_shell("true")]),
                ..PhasesSpec::default()
            },
            supported_hooks: SupportedHooksSpec::all(),
        };

        let paths = test_paths(&recipe, &policy, root.path());
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Build
            .plan(&PlanContext {
                target,
                pgo_stage: None,
                recipe: &recipe,
                paths: &paths,
                policy: &policy,
                compiler_cache: false,
                jobs: NonZeroUsize::new(2).unwrap(),
            })
            .unwrap()
            .unwrap();
        let StepPlan::Shell { environment, .. } = &plan.steps[0] else {
            panic!("explicit shell must stay shell")
        };

        assert_eq!(environment["BUILDER_ENVIRONMENT_ORDER"], "cargo");
        assert_eq!(environment["CMAKE_MARKER"], "present");
        assert_eq!(environment["CARGO_MARKER"], "present");
    }

    #[test]
    fn source_preparation_is_argv_preserving_and_never_parsed_as_shell() {
        let (recipe, policy, root) = fixture();
        let paths = test_paths(&recipe, &policy, root.path());
        let archive_name = "source archive;echo-not-shell.tar.xz";
        let sources = [
            UpstreamSpec::Archive {
                url: "https://example.invalid/source.tar.xz".to_owned(),
                hash: "a".repeat(64),
                rename: Some(archive_name.to_owned()),
                strip_dirs: Some(2),
                unpack: true,
                unpack_dir: Some("source tree".to_owned()),
            },
            UpstreamSpec::Git {
                url: "https://example.invalid/project.git".to_owned(),
                git_ref: "main".to_owned(),
                clone_dir: Some("git tree".to_owned()),
            },
        ];

        let context = context_for(&recipe, &paths, &policy, None);
        let steps = prepare_steps(&sources, &paths, &context, &policy).unwrap();

        assert_eq!(steps.len(), 4);
        let StepPlan::Run { program, args, .. } = &steps[1] else {
            panic!("archive preparation must be structural")
        };
        assert_eq!(program.path, "/usr/bin/bsdtar-static");
        assert_eq!(args[1], format!("/mason/sourcedir/{archive_name}"));
        assert_eq!(args[3], "source tree");
        assert_eq!(args[4], "--strip-components=2");
        assert!(!steps.iter().any(|step| matches!(step, StepPlan::Shell { .. })));

        let StepPlan::Run { program, args, .. } = &steps[3] else {
            panic!("git preparation must be structural")
        };
        assert_eq!(program.path, "/usr/bin/cp");
        assert_eq!(args[2], "/mason/sourcedir/project.git/.");
        assert_eq!(args[3], "git tree");
    }

    #[test]
    fn pgo_finish_uses_typed_policy_commands_and_controlled_globs() {
        let (recipe, mut policy, root) = fixture();
        policy.spec.pgo.merge_program.path = "/usr/bin/policy-profdata".to_owned();
        policy.spec.pgo.merge_program.requirement = BuildToolSpec::Binary("policy-profdata".to_owned());
        let paths = test_paths(&recipe, &policy, root.path());
        let context = context_for(&recipe, &paths, &policy, Some(pgo::Stage::One));

        let steps = pgo_finish_steps(Some(pgo::Stage::One), &context, &policy, "/mason/build/x86_64").unwrap();
        assert_eq!(steps.len(), 2);
        let StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            environment,
            ..
        } = &steps[0]
        else {
            panic!("PGO glob completion requires a controlled shell step")
        };

        assert_eq!(interpreter.path, "/usr/bin/bash");
        assert_eq!(declared_programs[0].path, "/usr/bin/policy-profdata");
        assert!(script.starts_with("set -euo pipefail\n"));
        assert!(script.contains("'/usr/bin/policy-profdata' 'merge' '--failure-mode=all'"));
        assert!(script.contains("'-output=/mason/build/x86_64-pgo/ir.profdata'"));
        assert!(script.contains("'/mason/build/x86_64-pgo/IR/default'*'.profraw'"));
        assert_eq!(environment["PGO_STAGE"], "ONE");
        let StepPlan::Run { program, args, .. } = &steps[1] else {
            panic!("PGO profile copy must be a structural run step")
        };
        assert_eq!(program.path, "/usr/bin/cp");
        assert_eq!(
            args.join("\n"),
            "/mason/build/x86_64-pgo/ir.profdata\n/mason/build/x86_64-pgo/combined.profdata"
        );
    }

    #[test]
    fn pgo_finish_rejects_paths_outside_the_typed_pgo_directory() {
        let (recipe, mut policy, root) = fixture();
        policy.spec.pgo.stage_one.finish.as_mut().unwrap().inputs =
            vec![TextSpec::Literal("/tmp/default*.profraw".to_owned())];
        let paths = test_paths(&recipe, &policy, root.path());
        let context = context_for(&recipe, &paths, &policy, Some(pgo::Stage::One));

        assert!(matches!(
            pgo_finish_steps(
                Some(pgo::Stage::One),
                &context,
                &policy,
                "/mason/build/x86_64"
            ),
            Err(Error::UnsafePgoPath { path, .. }) if path == "/tmp/default*.profraw"
        ));
        assert_eq!(shell_glob("/tmp/a b*[x]?.raw"), "'/tmp/a b'*'[x]'?'.raw'");
    }
}
