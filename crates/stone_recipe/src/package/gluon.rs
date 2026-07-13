// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Gluon evaluation boundary for package declarations.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};
use thiserror::Error;

use super::{
    BuilderSpec, DependencySpec, HooksSpec, MetaSpec, OutputRef, OutputSpec, PackageConversionError, PackageRef,
    PackageSpec, PhaseSpec, ProfileSpec, ScriptsSpec, StepSpec,
};
use crate::{KeyValueSpec, OptionsSpec, PathSpec, ToolchainSpec, TuningSpec, UpstreamSpec};

/// Version of the package-function ABI.
pub const PACKAGE_ABI_VERSION: u32 = 2;

/// Pure Gluon definitions exposed as `boulder.package.v2`.
pub const GLUON_PACKAGE_ABI: &str = include_str!("../../gluon/package.glu");

pub const GLUON_CMAKE_BUILDER_ABI: &str = include_str!("../../gluon/builders/cmake.glu");
pub const GLUON_MESON_BUILDER_ABI: &str = include_str!("../../gluon/builders/meson.glu");
pub const GLUON_CARGO_BUILDER_ABI: &str = include_str!("../../gluon/builders/cargo.glu");
pub const GLUON_AUTOTOOLS_BUILDER_ABI: &str = include_str!("../../gluon/builders/autotools.glu");

const GLUON_PURE_TYPES: &str = r#"type Bool =
    | False
    | True

type Option a =
    | None
    | Some a

type Result e t =
    | Err e
    | Ok t

type Ordering =
    | LT
    | EQ
    | GT

{ Bool, Option, Result, Ordering }
"#;

/// A normalized package-v2 value returned by restricted Gluon evaluation.
#[derive(Debug, Clone)]
pub struct EvaluatedPackage {
    pub package: PackageSpec,
    pub fingerprint: EvaluationFingerprint,
}

/// Failure to evaluate or validate a package factory result.
#[derive(Debug, Error)]
pub enum PackageEvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] PackageConversionError),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    Unset,
    Set(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBool {
    False,
    True,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPackageSpec {
    meta: GluonMetaSpec,
    builder: GluonBuilderSpec,
    hooks: GluonHooksSpec,
    native_build_inputs: Vec<GluonDependencySpec>,
    build_inputs: Vec<GluonDependencySpec>,
    check_inputs: Vec<GluonDependencySpec>,
    outputs: Vec<GluonOutputSpec>,
    options: GluonOptionsSpec,
    profiles: Vec<GluonProfileSpec>,
    sources: Vec<GluonUpstreamSpec>,
    architectures: Vec<String>,
    tuning: Vec<GluonKeyValueSpec<GluonTuningSpec>>,
    emul32: GluonBool,
    mold: GluonBool,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonMetaSpec {
    pname: String,
    version: String,
    release: i64,
    homepage: String,
    license: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonScriptsSpec {
    setup: GluonPhaseSpec,
    build: GluonPhaseSpec,
    install: GluonPhaseSpec,
    check: GluonPhaseSpec,
    workload: GluonPhaseSpec,
    environment: GluonPhaseSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPhaseSpec {
    steps: Vec<GluonStepSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonStepSpec {
    Shell { script: String },
    CargoFetch,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonHooksSpec {
    pre_setup: Vec<GluonStepSpec>,
    post_setup: Vec<GluonStepSpec>,
    pre_build: Vec<GluonStepSpec>,
    post_build: Vec<GluonStepSpec>,
    pre_check: Vec<GluonStepSpec>,
    post_check: Vec<GluonStepSpec>,
    pre_install: Vec<GluonStepSpec>,
    post_install: Vec<GluonStepSpec>,
    pre_workload: Vec<GluonStepSpec>,
    post_workload: Vec<GluonStepSpec>,
    environment: Vec<GluonStepSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPackageRef {
    name: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonOutputRef {
    package: GluonPackageRef,
    output: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonDependencySpec {
    Package { value: GluonPackageRef },
    Output { value: GluonOutputRef },
    Binary { target: String },
    SystemBinary { target: String },
    PkgConfig { target: String },
    PkgConfig32 { target: String },
    Soname { target: String },
    CMake { target: String },
    Python { target: String },
    Interpreter { target: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBuilderSpec {
    CMakeSystem {
        flags: Vec<String>,
        run_tests: GluonBool,
    },
    MesonSystem {
        flags: Vec<String>,
        run_tests: GluonBool,
    },
    CargoProject {
        features: Vec<String>,
        binaries: Vec<String>,
        run_tests: GluonBool,
    },
    AutotoolsProject {
        flags: Vec<String>,
        run_tests: GluonBool,
    },
    Custom {
        scripts: GluonScriptsSpec,
        required_tools: Vec<GluonDependencySpec>,
    },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonPathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonOutputSpec {
    name: String,
    summary: GluonOptional<String>,
    description: GluonOptional<String>,
    provides_exclude: Vec<String>,
    runtime_inputs: Vec<GluonDependencySpec>,
    runtime_exclude: Vec<String>,
    paths: Vec<GluonPathSpec>,
    conflicts: Vec<GluonDependencySpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonToolchainSpec {
    LlvmToolchain,
    GnuToolchain,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonOptionsSpec {
    toolchain: GluonToolchainSpec,
    cspgo: GluonBool,
    samplepgo: GluonBool,
    debug: GluonBool,
    strip: GluonBool,
    networking: GluonBool,
    compressman: GluonBool,
    lastrip: GluonBool,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonProfileSpec {
    name: String,
    builder: GluonBuilderSpec,
    hooks: GluonHooksSpec,
    native_build_inputs: Vec<GluonDependencySpec>,
    build_inputs: Vec<GluonDependencySpec>,
    check_inputs: Vec<GluonDependencySpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonUpstreamSpec {
    ArchiveSource {
        url: String,
        hash: String,
        rename: GluonOptional<String>,
        strip_dirs: GluonOptional<i64>,
        unpack: GluonBool,
        unpack_dir: GluonOptional<String>,
    },
    GitSource {
        url: String,
        git_ref: String,
        clone_dir: GluonOptional<String>,
    },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonTuningSpec {
    Enable,
    Disable,
    Config { value: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonKeyValueSpec<T> {
    key: String,
    value: T,
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::Unset => None,
            GluonOptional::Set(value) => Some(value),
        }
    }
}

impl From<GluonBool> for bool {
    fn from(value: GluonBool) -> Self {
        matches!(value, GluonBool::True)
    }
}

impl From<GluonPackageSpec> for PackageSpec {
    fn from(spec: GluonPackageSpec) -> Self {
        Self {
            meta: spec.meta.into(),
            builder: spec.builder.into(),
            hooks: spec.hooks.into(),
            native_build_inputs: spec.native_build_inputs.into_iter().map(Into::into).collect(),
            build_inputs: spec.build_inputs.into_iter().map(Into::into).collect(),
            check_inputs: spec.check_inputs.into_iter().map(Into::into).collect(),
            outputs: spec.outputs.into_iter().map(Into::into).collect(),
            options: spec.options.into(),
            profiles: spec.profiles.into_iter().map(Into::into).collect(),
            sources: spec.sources.into_iter().map(Into::into).collect(),
            architectures: spec.architectures,
            tuning: spec.tuning.into_iter().map(Into::into).collect(),
            emul32: spec.emul32.into(),
            mold: spec.mold.into(),
        }
    }
}

impl From<GluonMetaSpec> for MetaSpec {
    fn from(spec: GluonMetaSpec) -> Self {
        Self {
            pname: spec.pname,
            version: spec.version,
            release: spec.release,
            homepage: spec.homepage,
            license: spec.license,
        }
    }
}

impl From<GluonScriptsSpec> for ScriptsSpec {
    fn from(spec: GluonScriptsSpec) -> Self {
        Self {
            setup: spec.setup.into(),
            build: spec.build.into(),
            install: spec.install.into(),
            check: spec.check.into(),
            workload: spec.workload.into(),
            environment: spec.environment.into(),
        }
    }
}

impl From<GluonPhaseSpec> for PhaseSpec {
    fn from(spec: GluonPhaseSpec) -> Self {
        Self {
            steps: spec.steps.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonStepSpec> for StepSpec {
    fn from(spec: GluonStepSpec) -> Self {
        match spec {
            GluonStepSpec::Shell { script } => Self::Shell { script },
            GluonStepSpec::CargoFetch => Self::CargoFetch,
        }
    }
}

impl From<GluonHooksSpec> for HooksSpec {
    fn from(spec: GluonHooksSpec) -> Self {
        Self {
            pre_setup: spec.pre_setup.into_iter().map(Into::into).collect(),
            post_setup: spec.post_setup.into_iter().map(Into::into).collect(),
            pre_build: spec.pre_build.into_iter().map(Into::into).collect(),
            post_build: spec.post_build.into_iter().map(Into::into).collect(),
            pre_check: spec.pre_check.into_iter().map(Into::into).collect(),
            post_check: spec.post_check.into_iter().map(Into::into).collect(),
            pre_install: spec.pre_install.into_iter().map(Into::into).collect(),
            post_install: spec.post_install.into_iter().map(Into::into).collect(),
            pre_workload: spec.pre_workload.into_iter().map(Into::into).collect(),
            post_workload: spec.post_workload.into_iter().map(Into::into).collect(),
            environment: spec.environment.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonPackageRef> for PackageRef {
    fn from(spec: GluonPackageRef) -> Self {
        Self { name: spec.name }
    }
}

impl From<GluonOutputRef> for OutputRef {
    fn from(spec: GluonOutputRef) -> Self {
        Self {
            package: spec.package.into(),
            output: spec.output,
        }
    }
}

impl From<GluonDependencySpec> for DependencySpec {
    fn from(spec: GluonDependencySpec) -> Self {
        match spec {
            GluonDependencySpec::Package { value } => Self::Package(value.into()),
            GluonDependencySpec::Output { value } => Self::Output(value.into()),
            GluonDependencySpec::Binary { target } => Self::Binary(target),
            GluonDependencySpec::SystemBinary { target } => Self::SystemBinary(target),
            GluonDependencySpec::PkgConfig { target } => Self::PkgConfig(target),
            GluonDependencySpec::PkgConfig32 { target } => Self::PkgConfig32(target),
            GluonDependencySpec::Soname { target } => Self::Soname(target),
            GluonDependencySpec::CMake { target } => Self::CMake(target),
            GluonDependencySpec::Python { target } => Self::Python(target),
            GluonDependencySpec::Interpreter { target } => Self::Interpreter(target),
        }
    }
}

impl From<GluonBuilderSpec> for BuilderSpec {
    fn from(spec: GluonBuilderSpec) -> Self {
        match spec {
            GluonBuilderSpec::CMakeSystem { flags, run_tests } => Self::CMake {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderSpec::MesonSystem { flags, run_tests } => Self::Meson {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderSpec::CargoProject {
                features,
                binaries,
                run_tests,
            } => Self::Cargo {
                features,
                binaries,
                run_tests: run_tests.into(),
            },
            GluonBuilderSpec::AutotoolsProject { flags, run_tests } => Self::Autotools {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderSpec::Custom {
                scripts,
                required_tools,
            } => Self::Custom {
                scripts: scripts.into(),
                required_tools: required_tools.into_iter().map(Into::into).collect(),
            },
        }
    }
}

impl From<GluonPathSpec> for PathSpec {
    fn from(spec: GluonPathSpec) -> Self {
        match spec {
            GluonPathSpec::Any { path } => Self::Any { path },
            GluonPathSpec::Exe { path } => Self::Exe { path },
            GluonPathSpec::Symlink { path } => Self::Symlink { path },
            GluonPathSpec::Special { path } => Self::Special { path },
        }
    }
}

impl From<GluonOutputSpec> for OutputSpec {
    fn from(spec: GluonOutputSpec) -> Self {
        Self {
            name: spec.name,
            summary: spec.summary.into(),
            description: spec.description.into(),
            provides_exclude: spec.provides_exclude,
            runtime_inputs: spec.runtime_inputs.into_iter().map(Into::into).collect(),
            runtime_exclude: spec.runtime_exclude,
            paths: spec.paths.into_iter().map(Into::into).collect(),
            conflicts: spec.conflicts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonOptionsSpec> for OptionsSpec {
    fn from(spec: GluonOptionsSpec) -> Self {
        Self {
            toolchain: spec.toolchain.into(),
            cspgo: spec.cspgo.into(),
            samplepgo: spec.samplepgo.into(),
            debug: spec.debug.into(),
            strip: spec.strip.into(),
            networking: spec.networking.into(),
            compressman: spec.compressman.into(),
            lastrip: spec.lastrip.into(),
        }
    }
}

impl From<GluonToolchainSpec> for ToolchainSpec {
    fn from(spec: GluonToolchainSpec) -> Self {
        match spec {
            GluonToolchainSpec::LlvmToolchain => Self::Llvm,
            GluonToolchainSpec::GnuToolchain => Self::Gnu,
        }
    }
}

impl From<GluonProfileSpec> for ProfileSpec {
    fn from(spec: GluonProfileSpec) -> Self {
        Self {
            name: spec.name,
            builder: spec.builder.into(),
            hooks: spec.hooks.into(),
            native_build_inputs: spec.native_build_inputs.into_iter().map(Into::into).collect(),
            build_inputs: spec.build_inputs.into_iter().map(Into::into).collect(),
            check_inputs: spec.check_inputs.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonUpstreamSpec> for UpstreamSpec {
    fn from(spec: GluonUpstreamSpec) -> Self {
        match spec {
            GluonUpstreamSpec::ArchiveSource {
                url,
                hash,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
            } => Self::Archive {
                url,
                hash,
                rename: rename.into(),
                strip_dirs: strip_dirs.into(),
                unpack: unpack.into(),
                unpack_dir: unpack_dir.into(),
            },
            GluonUpstreamSpec::GitSource {
                url,
                git_ref,
                clone_dir,
            } => Self::Git {
                url,
                git_ref,
                clone_dir: clone_dir.into(),
            },
        }
    }
}

impl From<GluonTuningSpec> for TuningSpec {
    fn from(spec: GluonTuningSpec) -> Self {
        match spec {
            GluonTuningSpec::Enable => Self::Enable,
            GluonTuningSpec::Disable => Self::Disable,
            GluonTuningSpec::Config { value } => Self::Config { value },
        }
    }
}

impl<T, U> From<GluonKeyValueSpec<T>> for KeyValueSpec<U>
where
    U: From<T>,
{
    fn from(spec: GluonKeyValueSpec<T>) -> Self {
        Self {
            key: spec.key,
            value: spec.value.into(),
        }
    }
}

/// Evaluate a v2 package with the restricted default evaluator.
pub fn evaluate_gluon(source: &Source) -> Result<EvaluatedPackage, PackageEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

/// Evaluate a v2 package with caller-selected limits and source root.
pub fn evaluate_gluon_with(evaluator: &Evaluator, source: &Source) -> Result<EvaluatedPackage, PackageEvaluationError> {
    evaluate_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate a v2 package and bind lock bytes into its fingerprint.
pub fn evaluate_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedPackage, PackageEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.enable_array_primitives();
    import_policy.enable_string_primitives();
    import_policy.insert_embedded_module("std.types", GLUON_PURE_TYPES)?;
    import_policy.insert_embedded_module("boulder.package.v2", GLUON_PACKAGE_ABI)?;
    import_policy.insert_embedded_module("boulder.builders.cmake.v1", GLUON_CMAKE_BUILDER_ABI)?;
    import_policy.insert_embedded_module("boulder.builders.meson.v1", GLUON_MESON_BUILDER_ABI)?;
    import_policy.insert_embedded_module("boulder.builders.cargo.v1", GLUON_CARGO_BUILDER_ABI)?;
    import_policy.insert_embedded_module("boulder.builders.autotools.v1", GLUON_AUTOTOOLS_BUILDER_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonPackageSpec>(source, explicit_inputs)?;
    let package = PackageSpec::from(evaluation.value);
    package.validate()?;

    Ok(EvaluatedPackage {
        package,
        fingerprint: evaluation.fingerprint,
    })
}
