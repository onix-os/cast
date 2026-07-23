//! Lua declaration DTOs for the package recipe domain (Phase L5, in progress).
//!
//! Like the build policy, the package recipe reaches its domain value through an
//! infallible `From<GluonPackageSpec>`, so it is the neutral shape — pure
//! struct/unit types derive `Deserialize` directly on the domain type, while the
//! tuple/newtype enums (`DependencySpec`, `StepSpec`, …) get struct-variant Lua
//! DTOs with `From` conversions. This module holds that DTO tree; it is
//! assembled bottom-up over several slices toward a full `LuaPackageSpec`.

// The full package adapter is built across several slices; these DTOs are
// exercised by the tests below until the top-level evaluator lands.
#![cfg_attr(not(test), allow(dead_code))]

use lua_config::LuaOption;
use serde::Deserialize;

use crate::{PathSpec, UpstreamSpec};

use super::{
    BuilderEnvironmentSpec, BuilderSpec, BuiltProgramSpec, DependencySpec, HooksSpec, OutputRef,
    OutputSpec, PackageRef, PhaseSpec, PhasesSpec, ProfileSpec, ProgramSpec, StepSpec,
    SupportedHooksSpec,
};

/// Convert an optional Lua DTO into an optional domain value.
fn optional<L, D: From<L>>(value: LuaOption<L>) -> Option<D> {
    Option::<L>::from(value).map(Into::into)
}

/// The Lua encoding of a [`DependencySpec`]. The domain enum's tuple variants
/// become struct variants so the uniform `{ kind = … }` tag applies; the two
/// reference variants reuse the pure [`PackageRef`]/[`OutputRef`] domain types.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaDependencySpec {
    Package { value: PackageRef },
    Output { value: OutputRef },
    Binary { value: String },
    SystemBinary { value: String },
    PkgConfig { value: String },
    PkgConfig32 { value: String },
    Soname { value: String },
    #[serde(rename = "cmake")]
    CMake { value: String },
    Python { value: String },
    Interpreter { value: String },
}

impl From<LuaDependencySpec> for DependencySpec {
    fn from(dependency: LuaDependencySpec) -> Self {
        match dependency {
            LuaDependencySpec::Package { value } => Self::Package(value),
            LuaDependencySpec::Output { value } => Self::Output(value),
            LuaDependencySpec::Binary { value } => Self::Binary(value),
            LuaDependencySpec::SystemBinary { value } => Self::SystemBinary(value),
            LuaDependencySpec::PkgConfig { value } => Self::PkgConfig(value),
            LuaDependencySpec::PkgConfig32 { value } => Self::PkgConfig32(value),
            LuaDependencySpec::Soname { value } => Self::Soname(value),
            LuaDependencySpec::CMake { value } => Self::CMake(value),
            LuaDependencySpec::Python { value } => Self::Python(value),
            LuaDependencySpec::Interpreter { value } => Self::Interpreter(value),
        }
    }
}

/// Map a `Vec` of Lua dependency DTOs to their domain values.
pub(crate) fn dependency_vec(values: Vec<LuaDependencySpec>) -> Vec<DependencySpec> {
    values.into_iter().map(Into::into).collect()
}

/// The Lua encoding of a [`ProgramSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaProgramSpec {
    pub path: String,
    pub requirement: LuaDependencySpec,
}

impl From<LuaProgramSpec> for ProgramSpec {
    fn from(program: LuaProgramSpec) -> Self {
        Self {
            path: program.path,
            requirement: program.requirement.into(),
        }
    }
}

fn program_vec(values: Vec<LuaProgramSpec>) -> Vec<ProgramSpec> {
    values.into_iter().map(Into::into).collect()
}

/// The Lua encoding of a [`StepSpec`]. The builder-specific variants are plain
/// data; `run`/`run_built`/`shell` carry the program DTOs.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaStepSpec {
    Run { program: LuaProgramSpec, args: Vec<String> },
    RunBuilt { program: BuiltProgramSpec, args: Vec<String> },
    Shell { interpreter: LuaProgramSpec, declared_programs: Vec<LuaProgramSpec>, script: String },
    #[serde(rename = "cmake_configure")]
    CMakeConfigure { flags: Vec<String> },
    #[serde(rename = "cmake_build")]
    CMakeBuild,
    #[serde(rename = "cmake_install")]
    CMakeInstall,
    #[serde(rename = "cmake_test")]
    CMakeTest,
    MesonSetup { flags: Vec<String> },
    MesonBuild,
    MesonInstall,
    MesonTest,
    CargoBuild { features: Vec<String> },
    CargoInstall { binaries: Vec<String> },
    CargoTest { features: Vec<String> },
    AutotoolsConfigure { flags: Vec<String> },
    AutotoolsBuild,
    AutotoolsInstall,
    AutotoolsTest,
}

impl From<LuaStepSpec> for StepSpec {
    fn from(step: LuaStepSpec) -> Self {
        match step {
            LuaStepSpec::Run { program, args } => Self::Run { program: program.into(), args },
            LuaStepSpec::RunBuilt { program, args } => Self::RunBuilt { program, args },
            LuaStepSpec::Shell { interpreter, declared_programs, script } => Self::Shell {
                interpreter: interpreter.into(),
                declared_programs: program_vec(declared_programs),
                script,
            },
            LuaStepSpec::CMakeConfigure { flags } => Self::CMakeConfigure { flags },
            LuaStepSpec::CMakeBuild => Self::CMakeBuild,
            LuaStepSpec::CMakeInstall => Self::CMakeInstall,
            LuaStepSpec::CMakeTest => Self::CMakeTest,
            LuaStepSpec::MesonSetup { flags } => Self::MesonSetup { flags },
            LuaStepSpec::MesonBuild => Self::MesonBuild,
            LuaStepSpec::MesonInstall => Self::MesonInstall,
            LuaStepSpec::MesonTest => Self::MesonTest,
            LuaStepSpec::CargoBuild { features } => Self::CargoBuild { features },
            LuaStepSpec::CargoInstall { binaries } => Self::CargoInstall { binaries },
            LuaStepSpec::CargoTest { features } => Self::CargoTest { features },
            LuaStepSpec::AutotoolsConfigure { flags } => Self::AutotoolsConfigure { flags },
            LuaStepSpec::AutotoolsBuild => Self::AutotoolsBuild,
            LuaStepSpec::AutotoolsInstall => Self::AutotoolsInstall,
            LuaStepSpec::AutotoolsTest => Self::AutotoolsTest,
        }
    }
}

pub(crate) fn step_vec(values: Vec<LuaStepSpec>) -> Vec<StepSpec> {
    values.into_iter().map(Into::into).collect()
}

/// The Lua encoding of a [`PhaseSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaPhaseSpec {
    pub steps: Vec<LuaStepSpec>,
}

impl From<LuaPhaseSpec> for PhaseSpec {
    fn from(phase: LuaPhaseSpec) -> Self {
        Self {
            steps: step_vec(phase.steps),
        }
    }
}

/// The Lua encoding of a [`PhasesSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaPhasesSpec {
    pub setup: LuaPhaseSpec,
    pub build: LuaPhaseSpec,
    pub install: LuaPhaseSpec,
    pub check: LuaPhaseSpec,
    pub workload: LuaPhaseSpec,
}

impl From<LuaPhasesSpec> for PhasesSpec {
    fn from(phases: LuaPhasesSpec) -> Self {
        Self {
            setup: phases.setup.into(),
            build: phases.build.into(),
            install: phases.install.into(),
            check: phases.check.into(),
            workload: phases.workload.into(),
        }
    }
}

/// The Lua encoding of a [`HooksSpec`] — ten ordered step lists around builder
/// phases.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaHooksSpec {
    pub pre_setup: Vec<LuaStepSpec>,
    pub post_setup: Vec<LuaStepSpec>,
    pub pre_build: Vec<LuaStepSpec>,
    pub post_build: Vec<LuaStepSpec>,
    pub pre_check: Vec<LuaStepSpec>,
    pub post_check: Vec<LuaStepSpec>,
    pub pre_install: Vec<LuaStepSpec>,
    pub post_install: Vec<LuaStepSpec>,
    pub pre_workload: Vec<LuaStepSpec>,
    pub post_workload: Vec<LuaStepSpec>,
}

impl From<LuaHooksSpec> for HooksSpec {
    fn from(hooks: LuaHooksSpec) -> Self {
        Self {
            pre_setup: step_vec(hooks.pre_setup),
            post_setup: step_vec(hooks.post_setup),
            pre_build: step_vec(hooks.pre_build),
            post_build: step_vec(hooks.post_build),
            pre_check: step_vec(hooks.pre_check),
            post_check: step_vec(hooks.post_check),
            pre_install: step_vec(hooks.pre_install),
            post_install: step_vec(hooks.post_install),
            pre_workload: step_vec(hooks.pre_workload),
            post_workload: step_vec(hooks.post_workload),
        }
    }
}

/// The Lua encoding of a [`BuilderSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuilderSpec {
    pub required_tools: Vec<LuaDependencySpec>,
    pub environment: Vec<BuilderEnvironmentSpec>,
    pub phases: LuaPhasesSpec,
    pub supported_hooks: SupportedHooksSpec,
}

impl From<LuaBuilderSpec> for BuilderSpec {
    fn from(builder: LuaBuilderSpec) -> Self {
        Self {
            required_tools: dependency_vec(builder.required_tools),
            environment: builder.environment,
            phases: builder.phases.into(),
            supported_hooks: builder.supported_hooks,
        }
    }
}

/// The Lua encoding of an [`OutputSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaOutputSpec {
    pub name: String,
    pub include_in_manifest: bool,
    pub summary: LuaOption<String>,
    pub description: LuaOption<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_inputs: Vec<LuaDependencySpec>,
    pub runtime_exclude: Vec<String>,
    pub paths: Vec<PathSpec>,
    pub conflicts: Vec<LuaDependencySpec>,
}

impl From<LuaOutputSpec> for OutputSpec {
    fn from(output: LuaOutputSpec) -> Self {
        Self {
            name: output.name,
            include_in_manifest: output.include_in_manifest,
            summary: optional(output.summary),
            description: optional(output.description),
            provides_exclude: output.provides_exclude,
            runtime_inputs: dependency_vec(output.runtime_inputs),
            runtime_exclude: output.runtime_exclude,
            paths: output.paths,
            conflicts: dependency_vec(output.conflicts),
        }
    }
}

/// The Lua encoding of a [`ProfileSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaProfileSpec {
    pub name: String,
    pub builder: LuaBuilderSpec,
    pub hooks: LuaHooksSpec,
    pub native_build_inputs: Vec<LuaDependencySpec>,
    pub build_inputs: Vec<LuaDependencySpec>,
    pub check_inputs: Vec<LuaDependencySpec>,
}

impl From<LuaProfileSpec> for ProfileSpec {
    fn from(profile: LuaProfileSpec) -> Self {
        Self {
            name: profile.name,
            builder: profile.builder.into(),
            hooks: profile.hooks.into(),
            native_build_inputs: dependency_vec(profile.native_build_inputs),
            build_inputs: dependency_vec(profile.build_inputs),
            check_inputs: dependency_vec(profile.check_inputs),
        }
    }
}

/// The Lua encoding of an [`UpstreamSpec`]. The optional archive/git fields use
/// the tagged option encoding.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaUpstreamSpec {
    Archive {
        url: String,
        hash: String,
        rename: LuaOption<String>,
        strip_dirs: LuaOption<i64>,
        unpack: bool,
        unpack_dir: LuaOption<String>,
    },
    Git {
        url: String,
        git_ref: String,
        clone_dir: LuaOption<String>,
    },
}

impl From<LuaUpstreamSpec> for UpstreamSpec {
    fn from(upstream: LuaUpstreamSpec) -> Self {
        match upstream {
            LuaUpstreamSpec::Archive { url, hash, rename, strip_dirs, unpack, unpack_dir } => {
                Self::Archive {
                    url,
                    hash,
                    rename: Option::from(rename),
                    strip_dirs: Option::from(strip_dirs),
                    unpack,
                    unpack_dir: Option::from(unpack_dir),
                }
            }
            LuaUpstreamSpec::Git { url, git_ref, clone_dir } => Self::Git {
                url,
                git_ref,
                clone_dir: Option::from(clone_dir),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::super::MetaSpec;
    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("package.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn meta_decodes_directly_as_pure_data() {
        let meta: MetaSpec = decode(
            r#"return { pname = "hello", version = "1.0", release = 1, homepage = "https://x", license = { "MIT" } }"#,
        );
        assert_eq!(meta.pname, "hello");
        assert_eq!(meta.release, 1);
        assert_eq!(meta.license, vec!["MIT".to_owned()]);
    }

    #[test]
    fn step_variants_decode_including_programs_and_builder_steps() {
        let run: StepSpec = decode::<LuaStepSpec>(
            r#"return { kind = "run", program = { path = "/bin/cc", requirement = { kind = "binary", value = "cc" } }, args = { "-c" } }"#,
        )
        .into();
        assert!(matches!(run, StepSpec::Run { program, args } if program.path == "/bin/cc" && args == ["-c"]));

        let cmake: StepSpec = decode::<LuaStepSpec>(r#"return { kind = "cmake_build" }"#).into();
        assert_eq!(cmake, StepSpec::CMakeBuild);

        let cargo: StepSpec =
            decode::<LuaStepSpec>(r#"return { kind = "cargo_install", binaries = { "hello" } }"#).into();
        assert_eq!(cargo, StepSpec::CargoInstall { binaries: vec!["hello".to_owned()] });
    }

    #[test]
    fn phases_decode_with_empty_and_populated_step_lists() {
        let source = r#"
return {
    setup = { steps = {} },
    build = { steps = { { kind = "cmake_build" } } },
    install = { steps = {} },
    check = { steps = {} },
    workload = { steps = {} },
}
"#;
        let phases: PhasesSpec = decode::<LuaPhasesSpec>(source).into();
        assert!(phases.setup.steps.is_empty());
        assert_eq!(phases.build.steps, vec![StepSpec::CMakeBuild]);
    }

    #[test]
    fn an_output_decodes_options_paths_and_dependencies() {
        let source = r#"
return {
    name = "out",
    include_in_manifest = true,
    summary = { kind = "some", value = "main output" },
    description = { kind = "none" },
    provides_exclude = {},
    runtime_inputs = { { kind = "soname", value = "libc.so.6" } },
    runtime_exclude = {},
    paths = { { kind = "exe", path = "/usr/bin/hello" } },
    conflicts = {},
}
"#;
        let output: OutputSpec = decode::<LuaOutputSpec>(source).into();
        assert_eq!(output.name, "out");
        assert_eq!(output.summary, Some("main output".to_owned()));
        assert_eq!(output.description, None);
        assert_eq!(output.runtime_inputs, vec![DependencySpec::Soname("libc.so.6".to_owned())]);
        assert_eq!(output.paths, vec![crate::PathSpec::Exe { path: "/usr/bin/hello".to_owned() }]);
    }

    #[test]
    fn upstream_archive_and_git_decode_with_optional_fields() {
        let archive: UpstreamSpec = decode::<LuaUpstreamSpec>(
            r#"return { kind = "archive", url = "https://x/a.tar", hash = "abc", rename = { kind = "some", value = "a" }, strip_dirs = { kind = "none" }, unpack = true, unpack_dir = { kind = "none" } }"#,
        )
        .into();
        assert!(matches!(archive, UpstreamSpec::Archive { rename: Some(ref r), unpack: true, .. } if r == "a"));

        let git: UpstreamSpec = decode::<LuaUpstreamSpec>(
            r#"return { kind = "git", url = "https://x/g.git", git_ref = "main", clone_dir = { kind = "none" } }"#,
        )
        .into();
        assert!(matches!(git, UpstreamSpec::Git { clone_dir: None, .. }));
    }

    #[test]
    fn dependency_variants_decode_including_references() {
        let binary: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "binary", value = "cc" }"#).into();
        assert_eq!(binary, DependencySpec::Binary("cc".to_owned()));

        let cmake: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "cmake", value = "Foo" }"#).into();
        assert_eq!(cmake, DependencySpec::CMake("Foo".to_owned()));

        let package: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "package", value = { name = "glibc" } }"#).into();
        assert_eq!(package, DependencySpec::Package(PackageRef { name: "glibc".to_owned() }));

        let output: DependencySpec = decode::<LuaDependencySpec>(
            r#"return { kind = "output", value = { package = { name = "llvm" }, output = "dev" } }"#,
        )
        .into();
        assert_eq!(
            output,
            DependencySpec::Output(OutputRef {
                package: PackageRef { name: "llvm".to_owned() },
                output: "dev".to_owned(),
            })
        );
    }
}
