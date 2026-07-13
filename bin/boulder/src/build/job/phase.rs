// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use itertools::Itertools;
use moss::util;
use std::{collections::BTreeSet, num::NonZeroUsize, path::Path};
use stone_recipe::{
    Script, ToolchainSpec, UpstreamSpec,
    package::{PhaseSpec, StepSpec},
    script, tuning,
};
use tui::Styled;

use crate::build::pgo;
use crate::{Macros, Paths, Recipe, architecture::BuildTarget};

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

    pub fn script(
        &self,
        target: BuildTarget,
        pgo_stage: Option<pgo::Stage>,
        recipe: &Recipe,
        paths: &Paths,
        macros: &Macros,
        ccache: bool,
        num_jobs: NonZeroUsize,
    ) -> Result<Option<Script>, Error> {
        let typed_phases = recipe.build_target_phases(target);
        let mut typed_phase = match self {
            Phase::Prepare => PhaseSpec::default(),
            Phase::Setup => typed_phases.setup.clone(),
            Phase::Build => typed_phases.build.clone(),
            Phase::Install => typed_phases.install.clone(),
            Phase::Check => typed_phases.check.clone(),
            Phase::Workload => typed_phases.workload.clone(),
        };

        if !typed_phase.is_empty()
            && matches!(self, Phase::Workload)
            && matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm)
        {
            if matches!(pgo_stage, Some(pgo::Stage::One)) {
                typed_phase.steps.push(StepSpec::Shell {
                    script: "%llvm_merge_s1".to_owned(),
                });
            } else if matches!(pgo_stage, Some(pgo::Stage::Two)) {
                typed_phase.steps.push(StepSpec::Shell {
                    script: "%llvm_merge_s2".to_owned(),
                });
            }
        }

        let content = matches!(self, Phase::Prepare).then(|| prepare_script(&recipe.declaration.sources));
        if typed_phase.is_empty() && content.is_none() {
            return Ok(None);
        }

        if typed_phase.is_empty() && content.as_deref().is_some_and(str::is_empty) {
            return Ok(None);
        }

        let typed_env = if matches!(self, Phase::Prepare) {
            String::new()
        } else {
            typed_phases
                .environment
                .steps
                .iter()
                .map(environment_step)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
        };
        let env = format!("%scriptBase\n{typed_env}\n");

        let mut parser = script::Parser::new().env(env);

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

        add_tuning(target, pgo_stage, recipe, macros, &mut parser)?;

        if typed_phase.is_empty() {
            Ok(Some(parser.parse(content.as_deref().unwrap_or_default())?))
        } else {
            let builder_dir = parser.parse_content("%(builddir)")?;
            Ok(Some(compile_steps(
                &typed_phase,
                &parser,
                StepContext {
                    build_dir: Path::new(&builder_dir),
                    install_root: &paths.install().guest,
                    jobs: num_jobs,
                    package_name: &recipe.declaration.meta.pname,
                },
            )?))
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct StepContext<'a> {
    build_dir: &'a Path,
    install_root: &'a Path,
    jobs: NonZeroUsize,
    package_name: &'a str,
}

fn environment_step(step: &StepSpec) -> Result<String, Error> {
    match step {
        StepSpec::Shell { script } => Ok(script.clone()),
        StepSpec::CargoEnvironment => Ok(
            r#"CARGO_BUILD_DEP_INFO_BASEDIR="%(workdir)"; export CARGO_BUILD_DEP_INFO_BASEDIR;
CARGO_NET_RETRY=5; export CARGO_NET_RETRY;
CARGO_PROFILE_RELEASE_DEBUG="full"; export CARGO_PROFILE_RELEASE_DEBUG;
CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO="off"; export CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO;
CARGO_PROFILE_RELEASE_LTO="off"; export CARGO_PROFILE_RELEASE_LTO;
CARGO_PROFILE_RELEASE_STRIP="none"; export CARGO_PROFILE_RELEASE_STRIP;"#
                .to_owned(),
        ),
        _ => Err(Error::UnsupportedEnvironmentStep),
    }
}

/// Compile a structural phase into the existing executable script envelope.
///
/// Structural commands are escaped before the transitional parser sees them,
/// so only an explicit [`StepSpec::Shell`] may invoke a legacy `%action`.
fn compile_steps(phase: &PhaseSpec, parser: &script::Parser, context: StepContext<'_>) -> Result<Script, Error> {
    let content = phase
        .steps
        .iter()
        .map(|step| match step {
            StepSpec::Shell { script } => Ok(script.clone()),
            _ => render_step(step, parser, &context).map(|command| command.replace('%', "%%")),
        })
        .collect::<Result<Vec<_>, Error>>()?
        .join("\n");

    parser.parse(&content).map_err(Into::into)
}

fn render_step(step: &StepSpec, parser: &script::Parser, context: &StepContext<'_>) -> Result<String, Error> {
    let build_dir = context.build_dir.display();
    let install_root = context.install_root.display();
    let flags = |values: &[String]| {
        if values.is_empty() {
            String::new()
        } else {
            format!(" {}", values.join(" "))
        }
    };
    let features = |values: &[String]| {
        if values.is_empty() {
            String::new()
        } else {
            format!(" --features {}", values.join(","))
        }
    };

    Ok(match step {
        StepSpec::Shell { .. } | StepSpec::CargoEnvironment => return Err(Error::UnsupportedExecutableStep),
        StepSpec::CMakeConfigure { flags: values } => {
            let options = parser.parse_content("%(options_cmake_ninja)")?;
            format!("cmake {}{}", options.trim_end(), flags(values))
        }
        StepSpec::CMakeBuild => {
            format!(
                r#"cmake --build "${{BUILDDIR:-{build_dir}}}" --verbose --parallel "{}""#,
                context.jobs
            )
        }
        StepSpec::CMakeInstall => {
            format!(r#"DESTDIR="{install_root}" cmake --install "${{BUILDDIR:-{build_dir}}}" --verbose"#)
        }
        StepSpec::CMakeTest => format!(
            r#"ctest --test-dir "${{BUILDDIR:-{build_dir}}}" --verbose --parallel "{}" --output-on-failure --force-new-ctest-process"#,
            context.jobs
        ),
        StepSpec::MesonSetup { flags: values } => {
            let options = parser.parse_content("%(options_meson)")?;
            format!(
                r#"test -e ./meson.build || ( echo "%meson: The ./meson.build script could not be found" ; exit 1 )
meson setup {}{} "{build_dir}""#,
                options.trim_end(),
                flags(values),
            )
        }
        StepSpec::MesonBuild => {
            format!(r#"meson compile --verbose -j "{}" -C "{build_dir}""#, context.jobs)
        }
        StepSpec::MesonInstall => {
            format!(r#"DESTDIR="{install_root}" meson install --no-rebuild -C "{build_dir}""#)
        }
        StepSpec::MesonTest => format!(
            r#"meson test --no-rebuild --print-errorlogs --verbose -j "{}" -C "{build_dir}""#,
            context.jobs
        ),
        StepSpec::CargoFetch => "cargo fetch -v --locked".to_owned(),
        StepSpec::CargoBuild { features: values } => {
            let options = parser.parse_content("%(options_cargo_release)")?;
            format!("cargo build {}{}", options.trim_end(), features(values))
        }
        StepSpec::CargoInstall { binaries } => {
            let target_dir = parser.parse_content("%(cargo_target_dir)")?;
            let bindir = parser.parse_content("%(bindir)")?;
            let binaries = if binaries.is_empty() {
                vec![context.package_name]
            } else {
                binaries.iter().map(String::as_str).collect()
            };
            let sources = binaries
                .iter()
                .map(|binary| format!(r#""{target_dir}/{binary}""#))
                .join(" ");
            format!(r#"install -Dm00755 -t "{install_root}{bindir}" {sources}"#)
        }
        StepSpec::CargoTest { features: values } => {
            let options = parser.parse_content("%(options_cargo_release)")?;
            format!("cargo test {}{} --workspace", options.trim_end(), features(values))
        }
        StepSpec::AutotoolsConfigure { flags: values } => {
            let options = parser.parse_content("%(options_configure)")?;
            format!(
                r#"test -x ./configure || ( echo "%configure: The ./configure script could not be found" ; exit 1 )
# Rewrite any '#!*/bin/sh' shebang to '#!/usr/bin/dash' instead
# '-E' means "Use Extended Regular Expressions" (easier to write and read)
CONFIG_SHELL=/usr/bin/dash; export CONFIG_SHELL
SHELL=/usr/bin/dash; export SHELL
echo "Explicitly using dash to execute ./configure"
/usr/bin/dash ./configure {}{}"#,
                options.trim_end(),
                flags(values),
            )
        }
        StepSpec::AutotoolsBuild => format!(r#"make VERBOSE=1 -j "{}""#, context.jobs),
        StepSpec::AutotoolsInstall => format!(r#"make install DESTDIR="{install_root}""#),
        StepSpec::AutotoolsTest => "make check".to_owned(),
    })
}

fn prepare_script(sources: &[UpstreamSpec]) -> String {
    use std::fmt::Write;

    let mut content = String::default();

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

                let _ = writeln!(&mut content, "mkdir -p {unpack_dir}");
                let _ = writeln!(
                    &mut content,
                    r#"bsdtar-static xf "%(sourcedir)/{rename}" -C "{unpack_dir}" --strip-components={strip_dirs} --no-same-owner || (echo "Failed to extract archive"; exit 1);"#,
                );
            }
            UpstreamSpec::Git { url, clone_dir, .. } => {
                let source = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let target = clone_dir.as_ref().cloned().unwrap_or_else(|| source.to_owned());

                let _ = writeln!(&mut content, "mkdir -p {target}");
                let _ = writeln!(
                    &mut content,
                    r#"cp -Ra --no-preserve=ownership "%(sourcedir)/{source}/." "{target}""#,
                );
            }
        }
    }

    content
}

fn add_tuning(
    target: BuildTarget,
    pgo_stage: Option<pgo::Stage>,
    recipe: &Recipe,
    macros: &Macros,
    parser: &mut script::Parser,
) -> Result<(), Error> {
    let mut tuning = tuning::Builder::new();

    let build_target = target.to_string();

    for arch in ["base", &build_target] {
        let macros = macros
            .arch
            .get(arch)
            .cloned()
            .ok_or_else(|| Error::MissingArchMacros(arch.to_owned()))?;

        tuning.add_macros(macros);
    }

    for macros in macros.actions.clone() {
        tuning.add_macros(macros);
    }

    tuning.enable("architecture", None)?;

    for kv in &recipe.declaration.tuning {
        match &kv.value {
            stone_recipe::TuningSpec::Enable => tuning.enable(&kv.key, None)?,
            stone_recipe::TuningSpec::Disable => tuning.disable(&kv.key)?,
            stone_recipe::TuningSpec::Config { value } => tuning.enable(&kv.key, Some(value.clone()))?,
        }
    }

    // Add defaults that aren't already in recipe
    for group in default_tuning_groups(target, macros) {
        if !recipe.declaration.tuning.iter().any(|kv| &kv.key == group) {
            tuning.enable(group, None)?;
        }
    }

    if let Some(stage) = pgo_stage {
        match stage {
            pgo::Stage::One => tuning.enable("pgostage1", None)?,
            pgo::Stage::Two => tuning.enable("pgostage2", None)?,
            pgo::Stage::Use => {
                tuning.enable("pgouse", None)?;
                if recipe.declaration.options.samplepgo {
                    tuning.enable("pgosample", None)?;
                }
            }
        }
    }

    fn fmt_flags<'a>(flags: impl Iterator<Item = &'a str>) -> String {
        flags
            .map(|s| s.trim())
            .filter(|s| s.len() > 1)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .join(" ")
    }

    let toolchain = recipe.declaration.options.toolchain.into();
    let flags = tuning.build()?;

    let mut cflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::C, toolchain)),
    );
    let mut cxxflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::Cxx, toolchain)),
    );
    let fflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::F, toolchain)),
    );
    let ldflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::Ld, toolchain)),
    );
    let dflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::D, toolchain)),
    );
    let mut rustflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::Rust, toolchain)),
    );
    let valaflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::Vala, toolchain)),
    );
    let goflags = fmt_flags(
        flags
            .iter()
            .filter_map(|flag| flag.get(tuning::CompilerFlag::Go, toolchain)),
    );

    if recipe.declaration.mold {
        cflags.push_str(" -fuse-ld=mold");
        cxxflags.push_str(" -fuse-ld=mold");
        rustflags.push_str(" -Clink-arg=-fuse-ld=mold");
    }

    parser.add_definition("cflags", cflags);
    parser.add_definition("cxxflags", cxxflags);
    parser.add_definition("fflags", fflags);
    parser.add_definition("ldflags", ldflags);
    parser.add_definition("dflags", dflags);
    parser.add_definition("rustflags", rustflags);
    parser.add_definition("valaflags", valaflags);
    parser.add_definition("goflags", goflags);

    Ok(())
}

fn default_tuning_groups(target: BuildTarget, macros: &Macros) -> &[String] {
    let build_target = target.to_string();

    for arch in [&build_target, "base"] {
        let Some(arch_macros) = macros.arch.get(arch) else {
            continue;
        };

        if arch_macros.default_tuning_groups.is_empty() {
            continue;
        }

        return &arch_macros.default_tuning_groups;
    }

    &[]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::DateTime;
    use stone_recipe::script::Command;
    use stone_recipe::tuning::{Builder, CompilerFlag, Toolchain};

    use super::*;
    use crate::Architecture;

    fn structural_parser() -> script::Parser {
        let macros = Macros::repository_for_tests();
        let mut parser = script::Parser::new();
        parser.add_macros(macros.arch["base"].clone());
        parser.add_macros(macros.arch["x86_64"].clone());
        for actions in macros.actions {
            parser.add_macros(actions);
        }
        parser.add_definition("name", "example");
        parser.add_definition("jobs", 8);
        parser.add_definition("installroot", "/mason/install");
        parser.add_definition("workdir", "/mason/build/x86_64/example");
        for (name, value) in [
            ("version", "1.0.0"),
            ("release", "1"),
            ("pkgdir", "/mason/recipe/pkg"),
            ("sourcedir", "/mason/sources"),
            ("buildroot", "/mason/build/x86_64"),
            ("compiler_cache", "/mason/ccache"),
            ("scompiler_cache", "/mason/sccache"),
            ("sourcedateepoch", "0"),
            ("compiler_go_cache", ""),
            ("compiler_go_mod_cache", ""),
            ("compiler_cargo_cache", ""),
            ("compiler_zig_cache", ""),
            ("rustc_wrapper", ""),
            ("compiler_c", "clang"),
            ("compiler_cxx", "clang++"),
            ("compiler_objc", "clang"),
            ("compiler_objcxx", "clang++"),
            ("compiler_cpp", "clang-cpp"),
            ("compiler_objcpp", "clang -E -"),
            ("compiler_objcxxcpp", "clang++ -E"),
            ("compiler_d", "ldc2"),
            ("compiler_ar", "llvm-ar"),
            ("compiler_objcopy", "llvm-objcopy"),
            ("compiler_nm", "llvm-nm"),
            ("compiler_ranlib", "llvm-ranlib"),
            ("compiler_strip", "llvm-strip"),
            ("compiler_path", "/usr/bin:/bin"),
            ("compiler_ld", "ld.lld"),
            ("pgo_stage", "NONE"),
            ("pgo_dir", "/mason/build/x86_64-pgo"),
            ("cflags", ""),
            ("cxxflags", ""),
            ("fflags", ""),
            ("ldflags", ""),
            ("dflags", ""),
            ("rustflags", ""),
            ("valaflags", ""),
            ("goflags", ""),
        ] {
            parser.add_definition(name, value);
        }
        parser
    }

    fn step_context() -> StepContext<'static> {
        StepContext {
            build_dir: Path::new("aerynos-builddir"),
            install_root: Path::new("/mason/install"),
            jobs: NonZeroUsize::new(8).unwrap(),
            package_name: "example",
        }
    }

    #[test]
    fn repository_architecture_tuning_overrides_base_and_selects_base_defaults() {
        let macros = Macros::repository_for_tests();
        let target = BuildTarget::Native(Architecture::X86_64);

        let mut tuning = Builder::new();
        tuning.add_macros(macros.arch["base"].clone());
        tuning.add_macros(macros.arch["x86_64"].clone());
        tuning.enable("architecture", None).unwrap();
        let flags = tuning.build().unwrap();

        // These are the target override and base defaults from the YAML policy
        // at 80d7ac5, asserted through the production merge order.
        assert_eq!(flags.len(), 1);
        assert_eq!(
            flags[0].get(CompilerFlag::C, Toolchain::Llvm),
            Some("-march=x86-64-v2 -mtune=ivybridge")
        );
        assert_eq!(
            flags[0].get(CompilerFlag::Rust, Toolchain::Llvm),
            Some("-C target-cpu=x86-64-v2")
        );
        assert_eq!(
            default_tuning_groups(target, &macros),
            [
                "asneeded",
                "avxwidth",
                "base",
                "bindnow",
                "build-id",
                "compress-debug",
                "control-flow",
                "debug",
                "fat-lto",
                "fortify",
                "frame-pointer",
                "golang-ldflags",
                "golang-modflags",
                "harden",
                "icf",
                "libstdc-assertions",
                "lto",
                "lto-errors",
                "optimize",
                "relr",
                "symbolic",
                "thread-exceptions",
                "tls-gnu",
                "version-allow-undefined",
            ]
        );
    }

    #[test]
    fn explicit_source_timestamp_is_rendered_into_phase_scripts() {
        let recipe_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu");
        let timestamp = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut recipe = Recipe::load_at(recipe_path, timestamp).unwrap();
        recipe.declaration.builder = stone_recipe::package::BuilderSpec::Custom {
            scripts: stone_recipe::package::ScriptsSpec {
                build: PhaseSpec::new([StepSpec::Shell {
                    script: "echo %(sourcedateepoch) %(jobs)".to_owned(),
                }]),
                ..Default::default()
            },
            required_tools: Vec::new(),
        };
        let macros = Macros::repository_for_tests();
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::new(&recipe, None, root.path(), "/mason", root.path()).unwrap();
        let script = Phase::Build
            .script(
                BuildTarget::Native(Architecture::X86_64),
                None,
                &recipe,
                &paths,
                &macros,
                false,
                NonZeroUsize::new(3).unwrap(),
            )
            .unwrap()
            .unwrap();
        let content = script
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Content(content) => Some(content.as_str()),
                Command::Break(_) => None,
            })
            .join("\n");

        assert!(content.contains("1700000000"));
        assert!(content.contains("3"));
    }

    #[test]
    fn structural_steps_preserve_covered_legacy_action_expansions() {
        let parser = structural_parser();
        let context = step_context();
        let cases = [
            (StepSpec::CMakeConfigure { flags: vec![] }, "%cmake"),
            (StepSpec::CMakeBuild, "%cmake_build"),
            (StepSpec::CMakeInstall, "%cmake_install"),
            (StepSpec::CMakeTest, "%cmake_test"),
            (StepSpec::MesonSetup { flags: vec![] }, "%meson"),
            (StepSpec::MesonBuild, "%meson_build"),
            (StepSpec::MesonInstall, "%meson_install"),
            (StepSpec::MesonTest, "%meson_test"),
            (StepSpec::CargoFetch, "%cargo_fetch"),
            (StepSpec::CargoBuild { features: vec![] }, "%cargo_build"),
            (StepSpec::CargoTest { features: vec![] }, "%cargo_test"),
            (StepSpec::AutotoolsConfigure { flags: vec![] }, "%configure"),
            (StepSpec::AutotoolsBuild, "%make"),
        ];

        for (step, legacy) in cases {
            assert_eq!(
                render_step(&step, &parser, &context).unwrap().trim(),
                parser.parse_content(legacy).unwrap().trim(),
                "structural rendering diverged for {step:?}"
            );
        }
        assert_eq!(
            parser
                .parse_content(&environment_step(&StepSpec::CargoEnvironment).unwrap())
                .unwrap()
                .trim(),
            parser.parse_content("%cargo_set_environment").unwrap().trim()
        );
    }

    #[test]
    fn structural_steps_do_not_reenter_legacy_action_parser() {
        let parser = structural_parser();
        let phase = PhaseSpec::new([
            StepSpec::MesonSetup { flags: vec![] },
            StepSpec::Shell {
                script: "%cargo_fetch".to_owned(),
            },
        ]);

        let script = compile_steps(&phase, &parser, step_context()).unwrap();
        let Command::Content(content) = &script.commands[0] else {
            panic!("typed phase should compile to one shell command");
        };

        assert!(content.contains(r#"echo "%meson: The ./meson.build script could not be found""#));
        assert_eq!(content.matches("meson setup").count(), 1);
        assert!(content.contains("cargo fetch -v --locked"));
        assert_eq!(script.dependencies, ["binary(cargo)"]);
    }
}
