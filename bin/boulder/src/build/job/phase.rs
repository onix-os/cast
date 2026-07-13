// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use moss::util;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    path::Path,
};
use stone_recipe::{
    ToolchainSpec, UpstreamSpec,
    build_policy::{ContextValue, TextSpec},
    derivation::{PhasePlan, StepPlan},
    package::{PhaseSpec, StepSpec},
    script,
};
use tui::Styled;

use crate::build::{context::BuildContext, pgo};
use crate::{BuildPolicy, Macros, Paths, Recipe, architecture::BuildTarget};

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
        target: BuildTarget,
        pgo_stage: Option<pgo::Stage>,
        recipe: &Recipe,
        paths: &Paths,
        macros: &Macros,
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

        let mut parser = script::Parser::new();

        let build_target = target.to_string();
        let build_dir = paths.build().guest.join(&build_target);
        let work_dir = if matches!(self, Phase::Prepare) {
            build_dir.clone()
        } else {
            work_dir(&build_dir, &recipe.declaration.sources)
        };
        for arch in ["base", &build_target] {
            let macros = macros
                .arch
                .get(arch)
                .cloned()
                .ok_or_else(|| Error::MissingArchMacros(arch.to_owned()))?;

            parser.add_macros(macros.clone());
        }

        for macros in macros.actions.clone() {
            parser.add_macros(macros.clone());
        }

        parser.add_definition("name", &recipe.declaration.meta.pname);
        parser.add_definition("version", &recipe.declaration.meta.version);
        parser.add_definition("release", recipe.declaration.meta.release);
        parser.add_definition("jobs", num_jobs);
        parser.add_definition("pkgdir", paths.recipe().guest.join("pkg").display());
        parser.add_definition("sourcedir", paths.upstreams().guest.display());
        parser.add_definition("installroot", paths.install().guest.display());
        parser.add_definition("buildroot", build_dir.display());
        parser.add_definition("workdir", work_dir.display());

        parser.add_definition("compiler_cache", "/mason/ccache");
        parser.add_definition("scompiler_cache", "/mason/sccache");

        parser.add_definition("sourcedateepoch", recipe.build_time.timestamp());

        let path = if ccache {
            "/usr/lib/ccache/bin:/usr/bin:/bin"
        } else {
            "/usr/bin:/bin"
        };

        if ccache {
            parser.add_definition("compiler_go_cache", "/mason/gocache");
            parser.add_definition("compiler_go_mod_cache", "/mason/gomodcache");
            parser.add_definition("compiler_cargo_cache", "/mason/cargocache");
            parser.add_definition("compiler_zig_cache", "/mason/zigcache");
            parser.add_definition("rustc_wrapper", "/usr/bin/sccache");
        } else {
            parser.add_definition("compiler_go_cache", "");
            parser.add_definition("compiler_go_mod_cache", "");
            parser.add_definition("compiler_cargo_cache", "");
            parser.add_definition("compiler_zig_cache", "");
            parser.add_definition("rustc_wrapper", "");
        }

        /* Set the relevant compilers */
        if matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm) {
            parser.add_definition("compiler_c", "clang");
            parser.add_definition("compiler_cxx", "clang++");
            parser.add_definition("compiler_objc", "clang");
            parser.add_definition("compiler_objcxx", "clang++");
            parser.add_definition("compiler_cpp", "clang-cpp");
            parser.add_definition("compiler_objcpp", "clang -E -");
            parser.add_definition("compiler_objcxxcpp", "clang++ -E");
            parser.add_definition("compiler_d", "ldc2");
            parser.add_definition("compiler_ar", "llvm-ar");
            parser.add_definition("compiler_objcopy", "llvm-objcopy");
            parser.add_definition("compiler_nm", "llvm-nm");
            parser.add_definition("compiler_ranlib", "llvm-ranlib");
            parser.add_definition("compiler_strip", "llvm-strip");
        } else {
            parser.add_definition("compiler_c", "gcc");
            parser.add_definition("compiler_cxx", "g++");
            parser.add_definition("compiler_objc", "gcc");
            parser.add_definition("compiler_objcxx", "g++");
            parser.add_definition("compiler_cpp", "gcc -E");
            parser.add_definition("compiler_objcpp", "gcc -E");
            parser.add_definition("compiler_objcxxcpp", "g++ -E");
            parser.add_definition("compiler_d", "ldc2"); // FIXME: GDC
            parser.add_definition("compiler_ar", "gcc-ar");
            parser.add_definition("compiler_objcopy", "objcopy");
            parser.add_definition("compiler_nm", "gcc-nm");
            parser.add_definition("compiler_ranlib", "gcc-ranlib");
            parser.add_definition("compiler_strip", "strip");
        }
        parser.add_definition("compiler_path", path);

        if recipe.declaration.mold {
            parser.add_definition("compiler_ld", "ld.mold");
        } else if matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm) {
            parser.add_definition("compiler_ld", "ld.lld");
        } else {
            parser.add_definition("compiler_ld", "ld.bfd");
        }

        /* Allow packagers to do stage specific actions in a pgo build */
        if matches!(pgo_stage, Some(pgo::Stage::One)) {
            parser.add_definition("pgo_stage", "ONE");
        } else if matches!(pgo_stage, Some(pgo::Stage::Two)) {
            parser.add_definition("pgo_stage", "TWO");
        } else if matches!(pgo_stage, Some(pgo::Stage::Use)) {
            parser.add_definition("pgo_stage", "USE");
        } else {
            parser.add_definition("pgo_stage", "NONE");
        }

        parser.add_definition("pgo_dir", format!("{}-pgo", build_dir.display()));

        add_tuning(target, pgo_stage, recipe, policy, &mut parser, num_jobs)?;

        let context = resolved_context(
            &parser,
            recipe,
            paths,
            build_dir.as_path(),
            work_dir.as_path(),
            pgo_stage,
            ccache,
            num_jobs,
        )?;
        let working_dir = if matches!(self, Phase::Prepare) {
            build_dir.display().to_string()
        } else {
            work_dir.display().to_string()
        };
        if !matches!(self, Phase::Prepare) {
            validate_environment_steps(&typed_phases.environment)?;
        }
        let mut steps = if prepare {
            prepare_steps(&recipe.declaration.sources, paths, &context.environment, &working_dir)
        } else {
            compile_steps(&typed_phase, &context, &working_dir)?
        };
        if matches!(self, Phase::Workload)
            && matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm)
            && let Some(step) = pgo_finish_step(pgo_stage, &context, &working_dir)
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

fn resolved_context(
    parser: &script::Parser,
    recipe: &Recipe,
    paths: &Paths,
    build_dir: &Path,
    work_dir: &Path,
    pgo_stage: Option<pgo::Stage>,
    ccache: bool,
    jobs: NonZeroUsize,
) -> Result<BuildContext, Error> {
    let value = |name: &str| parser.parse_content(&format!("%({name})")).map_err(Error::from);
    let pgo_dir = format!("{}-pgo", build_dir.display());
    let build_subdir = value("builddir")?;
    let mut environment = BTreeMap::from([
        ("BOULDER_PNAME".to_owned(), recipe.declaration.meta.pname.clone()),
        ("BOULDER_PACKAGE_NAME".to_owned(), recipe.declaration.meta.pname.clone()),
        (
            "BOULDER_PACKAGE_VERSION".to_owned(),
            recipe.declaration.meta.version.clone(),
        ),
        (
            "BOULDER_PACKAGE_RELEASE".to_owned(),
            recipe.declaration.meta.release.to_string(),
        ),
        (
            "BOULDER_SOURCE_DIR".to_owned(),
            paths.upstreams().guest.display().to_string(),
        ),
        (
            "BOULDER_INSTALL_ROOT".to_owned(),
            paths.install().guest.display().to_string(),
        ),
        ("BOULDER_BUILD_ROOT".to_owned(), build_dir.display().to_string()),
        ("BOULDER_WORK_DIR".to_owned(), work_dir.display().to_string()),
        ("BOULDER_BUILDER_DIR".to_owned(), build_subdir.clone()),
        ("BOULDER_PGO_DIR".to_owned(), pgo_dir),
        ("BOULDER_JOBS".to_owned(), jobs.to_string()),
        (
            "SOURCE_DATE_EPOCH".to_owned(),
            recipe.build_time.timestamp().to_string(),
        ),
        (
            "PGO_STAGE".to_owned(),
            pgo_stage.map_or_else(|| "NONE".to_owned(), |stage| stage.to_string().to_uppercase()),
        ),
        ("TERM".to_owned(), "dumb".to_owned()),
        ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
        ("LC_ALL".to_owned(), "en_US.UTF-8".to_owned()),
        (
            "PATH".to_owned(),
            if ccache {
                "/usr/lib/ccache/bin:/usr/bin:/bin"
            } else {
                "/usr/bin:/bin"
            }
            .to_owned(),
        ),
        (
            "CARGO_BUILD_DEP_INFO_BASEDIR".to_owned(),
            work_dir.display().to_string(),
        ),
        ("CARGO_NET_RETRY".to_owned(), "5".to_owned()),
        ("CARGO_PROFILE_RELEASE_DEBUG".to_owned(), "full".to_owned()),
        ("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO".to_owned(), "off".to_owned()),
        ("CARGO_PROFILE_RELEASE_LTO".to_owned(), "off".to_owned()),
        ("CARGO_PROFILE_RELEASE_STRIP".to_owned(), "none".to_owned()),
    ]);
    for (name, definition) in [
        ("BOULDER_PREFIX", "prefix"),
        ("BOULDER_BINDIR", "bindir"),
        ("BOULDER_LIBDIR", "libdir"),
        ("BOULDER_LIBEXECDIR", "libexecdir"),
        ("BOULDER_DATADIR", "datadir"),
        ("BOULDER_VENDORDIR", "vendordir"),
        ("PKG_CONFIG_PATH", "pkgconfigpath"),
        ("CFLAGS", "cflags"),
        ("CXXFLAGS", "cxxflags"),
        ("FFLAGS", "fflags"),
        ("LDFLAGS", "ldflags"),
        ("DFLAGS", "dflags"),
        ("RUSTFLAGS", "rustflags"),
        ("VALAFLAGS", "valaflags"),
        ("GOFLAGS", "goflags"),
        ("CC", "cc"),
        ("CXX", "cxx"),
        ("OBJC", "objc"),
        ("OBJCXX", "objcxx"),
        ("CPP", "cpp"),
        ("OBJCPP", "objcpp"),
        ("OBJCXXCPP", "objcxxcpp"),
        ("AR", "ar"),
        ("LD", "ld"),
        ("OBJCOPY", "objcopy"),
        ("NM", "nm"),
        ("RANLIB", "ranlib"),
        ("STRIP", "strip"),
    ] {
        environment.insert(name.to_owned(), value(definition)?);
    }
    if ccache {
        environment.extend([
            ("CCACHE_DIR".to_owned(), "/mason/ccache".to_owned()),
            ("CCACHE_BASEDIR".to_owned(), work_dir.display().to_string()),
            ("SCCACHE_DIR".to_owned(), "/mason/sccache".to_owned()),
            ("RUSTC_WRAPPER".to_owned(), "/usr/bin/sccache".to_owned()),
            ("GOCACHE".to_owned(), "/mason/gocache".to_owned()),
            ("GOMODCACHE".to_owned(), "/mason/gomodcache".to_owned()),
            ("CARGO_HOME".to_owned(), "/mason/cargocache".to_owned()),
            ("ZIG_GLOBAL_CACHE_DIR".to_owned(), "/mason/zigcache".to_owned()),
            ("ZIG_LOCAL_CACHE_DIR".to_owned(), "/mason/zigcache".to_owned()),
        ]);
    }

    Ok(BuildContext {
        package_name: recipe.declaration.meta.pname.clone(),
        work_dir: work_dir.display().to_string(),
        build_subdir,
        install_root: paths.install().guest.display().to_string(),
        target_triple: value("target_triple")?,
        build_platform: value("build_platform")?,
        host_platform: value("host_platform")?,
        jobs: u32::try_from(jobs.get()).expect("supported jobs fit u32"),
        layout: crate::build::context::InstallLayout {
            prefix: value("prefix")?,
            bindir: value("bindir")?,
            sbindir: value("sbindir")?,
            includedir: value("includedir")?,
            libdir: value("libdir")?,
            libexecdir: value("libexecdir")?,
            datadir: value("datadir")?,
            mandir: value("mandir")?,
            infodir: value("infodir")?,
            localedir: value("localedir")?,
            sysconfdir: value("sysconfdir")?,
            localstatedir: value("localstatedir")?,
            sharedstatedir: value("sharedstatedir")?,
        },
        environment,
    })
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
    phase
        .steps
        .iter()
        .filter_map(|step| match step {
            StepSpec::Shell { script } => Some(Ok(literal_shell(script.clone(), &context.environment, working_dir))),
            StepSpec::CargoEnvironment => None,
            _ => context.resolve_standard_step(step).map(Ok),
        })
        .collect()
}

fn literal_shell(script: String, environment: &BTreeMap<String, String>, working_dir: &str) -> StepPlan {
    StepPlan::Shell {
        interpreter: "/usr/bin/bash".to_owned(),
        script,
        environment: environment.clone(),
        working_dir: working_dir.to_owned(),
    }
}

fn pgo_finish_step(stage: Option<pgo::Stage>, context: &BuildContext, working_dir: &str) -> Option<StepPlan> {
    let script = match stage? {
        pgo::Stage::One => {
            r#"llvm-profdata merge --failure-mode=all -output="$BOULDER_PGO_DIR/ir.profdata" "$BOULDER_PGO_DIR"/IR/default*.profraw
cp "$BOULDER_PGO_DIR/ir.profdata" "$BOULDER_PGO_DIR/combined.profdata""#
        }
        pgo::Stage::Two => {
            r#"rm "$BOULDER_PGO_DIR/combined.profdata"
llvm-profdata merge --failure-mode=all -output="$BOULDER_PGO_DIR/combined.profdata" "$BOULDER_PGO_DIR/ir.profdata" "$BOULDER_PGO_DIR"/CS/default*.profraw"#
        }
        pgo::Stage::Use => return None,
    };
    Some(literal_shell(script.to_owned(), &context.environment, working_dir))
}

fn prepare_steps(
    sources: &[UpstreamSpec],
    paths: &Paths,
    environment: &BTreeMap<String, String>,
    working_dir: &str,
) -> Vec<StepPlan> {
    let run = |program: &str, args: Vec<String>| StepPlan::Run {
        program: program.to_owned(),
        args,
        environment: environment.clone(),
        working_dir: working_dir.to_owned(),
    };
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

                steps.push(run("mkdir", vec!["-p".to_owned(), unpack_dir.clone()]));
                steps.push(run(
                    "bsdtar-static",
                    vec![
                        "xf".to_owned(),
                        paths.upstreams().guest.join(rename).display().to_string(),
                        "-C".to_owned(),
                        unpack_dir,
                        format!("--strip-components={strip_dirs}"),
                        "--no-same-owner".to_owned(),
                    ],
                ));
            }
            UpstreamSpec::Git { url, clone_dir, .. } => {
                let source = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let target = clone_dir.as_ref().cloned().unwrap_or_else(|| source.to_owned());

                steps.push(run("mkdir", vec!["-p".to_owned(), target.clone()]));
                steps.push(run(
                    "cp",
                    vec![
                        "-Ra".to_owned(),
                        "--no-preserve=ownership".to_owned(),
                        paths.upstreams().guest.join(source).join(".").display().to_string(),
                        target,
                    ],
                ));
            }
        }
    }
    steps
}

fn add_tuning(
    target: BuildTarget,
    pgo_stage: Option<pgo::Stage>,
    recipe: &Recipe,
    policy: &BuildPolicy,
    parser: &mut script::Parser,
    jobs: NonZeroUsize,
) -> Result<(), Error> {
    let target = policy.target(&target.to_string())?;
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

    // Mold becomes policy-owned in the next policy-data slice. Until then this
    // explicit typed overlay preserves current package semantics without macro
    // interpolation.
    if recipe.declaration.mold {
        selection.flags.c.push(TextSpec::Literal("-fuse-ld=mold".to_owned()));
        selection.flags.cxx.push(TextSpec::Literal("-fuse-ld=mold".to_owned()));
        selection
            .flags
            .rust
            .push(TextSpec::Literal("-Clink-arg=-fuse-ld=mold".to_owned()));
    }

    let pgo_dir = format!("{}-pgo", parser.parse_content("%(buildroot)")?);
    let resolve = |values: &[TextSpec]| resolve_flags(values, jobs, &pgo_dir);
    parser.add_definition("cflags", resolve(&selection.flags.c)?);
    parser.add_definition("cxxflags", resolve(&selection.flags.cxx)?);
    parser.add_definition("fflags", resolve(&selection.flags.f)?);
    parser.add_definition("ldflags", resolve(&selection.flags.ld)?);
    parser.add_definition("dflags", resolve(&selection.flags.d)?);
    parser.add_definition("rustflags", resolve(&selection.flags.rust)?);
    parser.add_definition("valaflags", resolve(&selection.flags.vala)?);
    parser.add_definition("goflags", resolve(&selection.flags.go)?);

    Ok(())
}

fn resolve_flags(values: &[TextSpec], jobs: NonZeroUsize, pgo_dir: &str) -> Result<String, Error> {
    fn resolve(value: &TextSpec, jobs: NonZeroUsize, pgo_dir: &str) -> Result<String, Error> {
        match value {
            TextSpec::Literal(value) => Ok(value.clone()),
            TextSpec::Context(ContextValue::Jobs) => Ok(jobs.to_string()),
            TextSpec::Context(ContextValue::PgoDir) => Ok(pgo_dir.to_owned()),
            TextSpec::Context(context) => Err(Error::UnsupportedTuningContext(*context)),
            TextSpec::Concat(parts) => parts
                .iter()
                .map(|part| resolve(part, jobs, pgo_dir))
                .collect::<Result<Vec<_>, _>>()
                .map(|parts| parts.concat()),
        }
    }

    values
        .iter()
        .map(|value| resolve(value, jobs, pgo_dir))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|values| {
            values
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
}

#[cfg(test)]
mod direct_tests {
    use chrono::DateTime;
    use stone_recipe::{
        derivation::StepPlan,
        package::{BuilderSpec, PhaseSpec, ScriptsSpec, StepSpec},
    };

    use super::*;
    use crate::{Architecture, BuildPolicy, Macros, Paths, Recipe};

    fn fixture() -> (Recipe, Macros, BuildPolicy, tempfile::TempDir) {
        let recipe_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu");
        let recipe = Recipe::load_at(recipe_path, DateTime::from_timestamp(1_700_000_000, 0).unwrap()).unwrap();
        (
            recipe,
            Macros::repository_for_tests(),
            BuildPolicy::repository_for_tests(),
            tempfile::tempdir().unwrap(),
        )
    }

    #[test]
    fn standard_steps_freeze_as_run_with_exact_context() {
        let (mut recipe, macros, policy, root) = fixture();
        recipe.declaration.builder = BuilderSpec::CMake {
            flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
            run_tests: false,
        };
        let paths = Paths::new(&recipe, None, root.path(), "/mason", root.path()).unwrap();
        let plan = Phase::Setup
            .plan(
                BuildTarget::Native(Architecture::X86_64),
                None,
                &recipe,
                &paths,
                &macros,
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
        let (mut recipe, macros, policy, root) = fixture();
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
        let paths = Paths::new(&recipe, None, root.path(), "/mason", root.path()).unwrap();
        let plan = Phase::Build
            .plan(
                BuildTarget::Native(Architecture::X86_64),
                None,
                &recipe,
                &paths,
                &macros,
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
        let (recipe, _, _, root) = fixture();
        let paths = Paths::new(&recipe, None, root.path(), "/mason", root.path()).unwrap();
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

        let steps = prepare_steps(&sources, &paths, &BTreeMap::new(), "/mason/build/x86_64");

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
}
