// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use moss::util;
use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    path::{Component, Path},
};
use stone_recipe::{
    ToolchainSpec, UpstreamSpec,
    build_policy::{CompilerFlagsSpec, ContextValue, TargetPolicySpec, TextSpec},
    derivation::{PhasePlan, StepPlan},
    package::{PhaseSpec, StepSpec},
};
use tui::Styled;

use crate::build::{
    context::{BuildContext, PgoContextStage, TextContextOverlay, TypedContextInputs},
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

    pub fn plan(
        &self,
        target: &TargetPolicySpec,
        pgo_stage: Option<pgo::Stage>,
        recipe: &Recipe,
        paths: &Paths,
        policy: &BuildPolicy,
        ccache: bool,
        num_jobs: NonZeroUsize,
    ) -> Result<Option<PhasePlan>, Error> {
        let typed_phases = recipe.build_target_phases(target);
        let typed_phase = match self {
            Phase::Prepare => PhaseSpec::default(),
            Phase::Setup => typed_phases.setup.clone(),
            Phase::Build => typed_phases.build.clone(),
            Phase::Install => typed_phases.install.clone(),
            Phase::Check => typed_phases.check.clone(),
            Phase::Workload => typed_phases.workload.clone(),
        };

        let prepare = matches!(self, Phase::Prepare);
        if typed_phase.is_empty() && !prepare {
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
                jobs: u32::try_from(num_jobs.get()).expect("supported jobs fit u32"),
                source_date_epoch: recipe.build_time.timestamp(),
                pgo_stage: pgo_context_stage(pgo_stage),
                toolchain: recipe.declaration.options.toolchain,
                compiler_cache_enabled: ccache,
                mold_enabled: recipe.declaration.mold,
                flags,
            },
        )?;
        validate_environment_steps(&typed_phases.environment)?;
        if typed_phases
            .environment
            .steps
            .iter()
            .any(|step| matches!(step, StepSpec::CargoEnvironment))
        {
            context.extend_environment(&policy.spec.builders.cargo.environment)?;
        }
        let working_dir = if matches!(self, Phase::Prepare) {
            build_dir.display().to_string()
        } else {
            work_dir.display().to_string()
        };
        let mut steps = if prepare {
            prepare_steps(&recipe.declaration.sources, paths, &context, policy)?
        } else {
            compile_steps(&typed_phase, &context, &working_dir)?
        };
        if matches!(self, Phase::Workload)
            && matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm)
            && let Some(step) = pgo_finish_step(pgo_stage, &context, policy, &working_dir)?
        {
            steps.push(step);
        }
        if steps.is_empty() {
            return Ok(None);
        }
        Ok(Some(PhasePlan {
            name: self.to_string(),
            pre: Vec::new(),
            steps,
            post: Vec::new(),
        }))
    }
}

fn validate_environment_steps(phase: &PhaseSpec) -> Result<(), Error> {
    if phase
        .steps
        .iter()
        .all(|step| matches!(step, StepSpec::CargoEnvironment))
    {
        Ok(())
    } else {
        Err(Error::UnsupportedEnvironmentStep)
    }
}

fn compile_steps(phase: &PhaseSpec, context: &BuildContext, working_dir: &str) -> Result<Vec<StepPlan>, Error> {
    let mut steps = Vec::with_capacity(phase.steps.len());
    for step in &phase.steps {
        match step {
            StepSpec::Shell { script } => {
                steps.push(literal_shell(script.clone(), &context.environment, working_dir));
            }
            StepSpec::CargoEnvironment => {}
            _ => {
                if let Some(step) = context.resolve_standard_step(step)? {
                    steps.push(step);
                }
            }
        }
    }
    Ok(steps)
}

fn literal_shell(script: String, environment: &BTreeMap<String, String>, working_dir: &str) -> StepPlan {
    StepPlan::Shell {
        interpreter: "/usr/bin/bash".to_owned(),
        script,
        environment: environment.clone(),
        working_dir: working_dir.to_owned(),
    }
}

fn pgo_finish_step(
    stage: Option<pgo::Stage>,
    context: &BuildContext,
    policy: &BuildPolicy,
    working_dir: &str,
) -> Result<Option<StepPlan>, Error> {
    let stage = match stage {
        Some(pgo::Stage::One) => &policy.spec.pgo.stage_one,
        Some(pgo::Stage::Two) => &policy.spec.pgo.stage_two,
        Some(pgo::Stage::Use) | None => return Ok(None),
    };
    let Some(finish) = &stage.finish else {
        return Ok(None);
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
    let merge_program = resolve(&policy.spec.pgo.merge_program)?;
    let merge_args = policy
        .spec
        .pgo
        .merge_args
        .iter()
        .map(resolve)
        .collect::<Result<Vec<_>, _>>()?;
    let mut lines = vec!["set -euo pipefail".to_owned()];
    if finish.remove_output_first {
        lines.push(format!(
            "{} {}",
            shell_quote(&resolve(&policy.spec.pgo.remove_program)?),
            shell_quote(&output)
        ));
    }
    let mut merge = vec![shell_quote(&merge_program)];
    merge.extend(merge_args.iter().map(|argument| shell_quote(argument)));
    merge.push(shell_quote(&format!("-output={output}")));
    merge.extend(inputs.iter().map(|input| shell_glob(input)));
    lines.push(merge.join(" "));
    if let Some(copy_to) = copy_to {
        lines.push(format!(
            "{} {} {}",
            shell_quote(&resolve(&policy.spec.pgo.copy_program)?),
            shell_quote(&output),
            shell_quote(&copy_to)
        ));
    }

    Ok(Some(literal_shell(lines.join("\n"), &context.environment, working_dir)))
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
        derivation::StepPlan,
        package::{BuilderSpec, PhaseSpec, ScriptsSpec, StepSpec},
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
        BuildContext::resolve(
            &policy.spec,
            target_policy,
            TypedContextInputs {
                package_name: recipe.declaration.meta.pname.clone(),
                package_version: recipe.declaration.meta.version.clone(),
                package_release: recipe.declaration.meta.release,
                source_dir: paths.upstreams().guest.display().to_string(),
                install_root: paths.install().guest.display().to_string(),
                build_root: "/mason/build/x86_64".to_owned(),
                work_dir: "/mason/build/x86_64".to_owned(),
                pgo_dir: "/mason/build/x86_64-pgo".to_owned(),
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

    #[test]
    fn standard_steps_freeze_as_run_with_exact_context() {
        let (mut recipe, policy, root) = fixture();
        recipe.declaration.builder = BuilderSpec::CMake {
            flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
            run_tests: false,
        };
        let paths = Paths::new(&recipe, root.path(), "/mason", root.path()).unwrap();
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Setup
            .plan(
                target,
                None,
                &recipe,
                &paths,
                &policy,
                false,
                NonZeroUsize::new(3).unwrap(),
            )
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
        assert_eq!(program, "cmake");
        assert_eq!(working_dir, "/mason/build/x86_64");
        assert_eq!(environment["BOULDER_PACKAGE_NAME"], "hello");
        assert_eq!(environment["BOULDER_JOBS"], "3");
        assert_eq!(environment["SOURCE_DATE_EPOCH"], "1700000000");
        assert_eq!(environment["CC"], "clang");
    }

    #[test]
    fn authored_shell_percent_text_is_literal() {
        let (mut recipe, policy, root) = fixture();
        let literal = "%cargo_fetch $BOULDER_INSTALL_ROOT %(jobs)";
        recipe.declaration.builder = BuilderSpec::Custom {
            scripts: ScriptsSpec {
                build: PhaseSpec::new([StepSpec::Shell {
                    script: literal.to_owned(),
                }]),
                ..ScriptsSpec::default()
            },
            required_tools: Vec::new(),
        };
        let paths = Paths::new(&recipe, root.path(), "/mason", root.path()).unwrap();
        let target = policy.target("x86_64").unwrap();
        let plan = Phase::Build
            .plan(
                target,
                None,
                &recipe,
                &paths,
                &policy,
                false,
                NonZeroUsize::new(2).unwrap(),
            )
            .unwrap()
            .unwrap();
        let StepPlan::Shell { script, .. } = &plan.steps[0] else {
            panic!("explicit shell must stay shell")
        };
        assert_eq!(script, literal);
    }

    #[test]
    fn source_preparation_is_argv_preserving_and_never_parsed_as_shell() {
        let (recipe, policy, root) = fixture();
        let paths = Paths::new(&recipe, root.path(), "/mason", root.path()).unwrap();
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
        assert_eq!(program, "bsdtar-static");
        assert_eq!(args[1], format!("/mason/sourcedir/{archive_name}"));
        assert_eq!(args[3], "source tree");
        assert_eq!(args[4], "--strip-components=2");
        assert!(!steps.iter().any(|step| matches!(step, StepPlan::Shell { .. })));

        let StepPlan::Run { program, args, .. } = &steps[3] else {
            panic!("git preparation must be structural")
        };
        assert_eq!(program, "cp");
        assert_eq!(args[2], "/mason/sourcedir/project.git/.");
        assert_eq!(args[3], "git tree");
    }

    #[test]
    fn pgo_finish_uses_typed_policy_commands_and_controlled_globs() {
        let (recipe, mut policy, root) = fixture();
        policy.spec.pgo.merge_program = TextSpec::Literal("policy-profdata".to_owned());
        let paths = Paths::new(&recipe, root.path(), "/mason", root.path()).unwrap();
        let context = context_for(&recipe, &paths, &policy, Some(pgo::Stage::One));

        let StepPlan::Shell {
            script, environment, ..
        } = pgo_finish_step(Some(pgo::Stage::One), &context, &policy, "/mason/build/x86_64")
            .unwrap()
            .unwrap()
        else {
            panic!("PGO glob completion requires a controlled shell step")
        };

        assert!(script.starts_with("set -euo pipefail\n"));
        assert!(script.contains("'policy-profdata' 'merge' '--failure-mode=all'"));
        assert!(script.contains("'-output=/mason/build/x86_64-pgo/ir.profdata'"));
        assert!(script.contains("'/mason/build/x86_64-pgo/IR/default'*'.profraw'"));
        assert!(
            script.contains("'cp' '/mason/build/x86_64-pgo/ir.profdata' '/mason/build/x86_64-pgo/combined.profdata'")
        );
        assert_eq!(environment["PGO_STAGE"], "ONE");
    }

    #[test]
    fn pgo_finish_rejects_paths_outside_the_typed_pgo_directory() {
        let (recipe, mut policy, root) = fixture();
        policy.spec.pgo.stage_one.finish.as_mut().unwrap().inputs =
            vec![TextSpec::Literal("/tmp/default*.profraw".to_owned())];
        let paths = Paths::new(&recipe, root.path(), "/mason", root.path()).unwrap();
        let context = context_for(&recipe, &paths, &policy, Some(pgo::Stage::One));

        assert!(matches!(
            pgo_finish_step(
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
