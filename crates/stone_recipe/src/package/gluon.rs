//! Gluon evaluation boundary for package declarations.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, EvaluationDeadline,
    Evaluation as DeclarationEvaluation,
    LanguageSpec, Limits, SourceRoot,
};
use gluon_config::{Diagnostic, EvaluationIdentity, GluonEngine, Source};

use super::{
    AuthoredPackage, BuilderEnvironmentSpec, BuilderRequest, BuilderSpec, BuiltProgramSpec, DependencySpec, HooksSpec,
    MetaSpec, OutputRef, OutputSpec, PackageConversionError, PackageRef, PackageSpec, PhaseSpec, PhasesSpec, ProfileSpec,
    ProgramSpec, StepSpec, SupportedHooksSpec, default_output_set_with_root, lower,
};
use crate::{NamedTuningSpec, OptionsSpec, PathSpec, ToolchainSpec, TuningSpec, UpstreamSpec};

/// Version of the package-function ABI.
pub const PACKAGE_ABI_VERSION: u32 = 3;

/// Pure Gluon definitions exposed as `cast.package.v3`.
pub const GLUON_PACKAGE_ABI: &str = include_str!("../../gluon/package.glu");

pub const GLUON_CMAKE_BUILDER_ABI: &str = include_str!("../../gluon/builders/cmake.glu");
pub const GLUON_MESON_BUILDER_ABI: &str = include_str!("../../gluon/builders/meson.glu");
pub const GLUON_CARGO_BUILDER_ABI: &str = include_str!("../../gluon/builders/cargo.glu");
pub const GLUON_AUTOTOOLS_BUILDER_ABI: &str = include_str!("../../gluon/builders/autotools.glu");

/// The types-only authoring prelude exposed as `cast.authored.v1`.
///
/// This is *not* an ABI: it carries no defaults and no builder logic — only the
/// type + constructor layer (deps, sources, steps, programs, paths, tuning, the
/// `BuilderRequest` kinds, and the `Custom` builder escape hatch) an author needs
/// to write the minimal [`AuthoredPackage`] record that the shared Rust [`lower`]
/// then completes. Because Gluon records are structurally typed (no field may be
/// omitted), a Gluon recipe names every field explicitly and selects a Rust
/// default with `unset`; the shared lowering — not this module — owns defaults.
pub const GLUON_AUTHORED_PRELUDE: &str = include_str!("../../gluon/authored.glu");

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

/// Stateful Gluon package adapter with every package ABI fixed at construction.
///
/// The adapter implements the ordinary declaration role for authored packages
/// without a source lock and the explicit-input role for callers which bind
/// exact generated lock bytes into fingerprint v1.
#[derive(Debug, Clone)]
pub struct GluonPackageEvaluator {
    engine: GluonEngine,
}

impl Default for GluonPackageEvaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl GluonPackageEvaluator {
    pub fn new(limits: Limits) -> Self {
        Self::from_engine(GluonEngine::new(limits))
            .expect("the embedded package and builder ABIs are valid and unique")
    }

    fn from_engine(engine: GluonEngine) -> Result<Self, Diagnostic> {
        let mut import_policy = engine.import_policy().clone();
        import_policy.enable_array_primitives();
        import_policy.enable_string_primitives();
        import_policy.insert_embedded_module("std.types", GLUON_PURE_TYPES)?;
        import_policy.insert_embedded_module("cast.package.v3", GLUON_PACKAGE_ABI)?;
        import_policy.insert_embedded_module("cast.authored.v1", GLUON_AUTHORED_PRELUDE)?;
        import_policy.insert_embedded_module("cast.builders.cmake.v2", GLUON_CMAKE_BUILDER_ABI)?;
        import_policy.insert_embedded_module("cast.builders.meson.v2", GLUON_MESON_BUILDER_ABI)?;
        import_policy.insert_embedded_module("cast.builders.cargo.v2", GLUON_CARGO_BUILDER_ABI)?;
        import_policy.insert_embedded_module("cast.builders.autotools.v2", GLUON_AUTOTOOLS_BUILDER_ABI)?;
        Ok(Self {
            engine: engine.with_import_policy(import_policy),
        })
    }

    fn evaluate_package(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<PackageSpec, EvaluationIdentity>,
        DeclarationEvaluationError<PackageConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs_within::<GluonPackageSpec>(
                source,
                explicit_inputs,
                deadline,
            )
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let package = PackageSpec::from(evaluation.value);
        package
            .validate()
            .map_err(DeclarationEvaluationError::Conversion)?;

        Ok(DeclarationEvaluation {
            value: package,
            identity: evaluation.identity,
        })
    }

    /// Decode a *minimal-form* authored recipe (a native Gluon record importing
    /// `cast.authored.v1`) into the language-agnostic [`AuthoredPackage`], and
    /// lower it through shared Rust — threading explicit inputs and the
    /// evaluation identity exactly as the legacy path does. No authoring logic
    /// runs in Gluon.
    fn evaluate_authored_package(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<PackageSpec, EvaluationIdentity>,
        DeclarationEvaluationError<PackageConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs_within::<GluonAuthoredPackage>(source, explicit_inputs, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let package = lower(AuthoredPackage::from(evaluation.value));
        package
            .validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: package,
            identity: evaluation.identity,
        })
    }

    /// Dispatch a recipe to the authored decode when it declares the
    /// `cast.authored.v1` ABI, and to the legacy `cast.package.v3` decode
    /// otherwise. This bridge lets the migrated corpus evaluate through the
    /// shared authored layer while any not-yet-migrated legacy recipe still
    /// evaluates, until the legacy ABI is removed.
    fn evaluate_dispatched(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<PackageSpec, EvaluationIdentity>,
        DeclarationEvaluationError<PackageConversionError>,
    > {
        // Try the authored decode first. A migrated recipe decodes as an
        // `AuthoredPackage`; a legacy recipe produces the fully-lowered package
        // shape (builder as a `BuilderSpec` record, not a `BuilderRequest`
        // variant; hooks as a record, not `Optional`), so it fails the authored
        // decode cleanly and falls back to the legacy path. Text sniffing is
        // deliberately avoided: an ABI name can appear in a comment, and a
        // factory entry reaches its ABI only through a transitive import.
        let probe = EvaluationDeadline::start(self.engine.limits().timeout);
        match self.evaluate_authored_package(source, explicit_inputs, probe) {
            Ok(evaluation) => Ok(evaluation),
            Err(_) => self.evaluate_package(source, explicit_inputs, deadline),
        }
    }

    /// Decode a minimal-form authored recipe into its [`PackageSpec`]. Test and
    /// tooling entry point; the production path is [`Self::evaluate_dispatched`].
    pub fn evaluate_authored(
        &self,
        source: &Source,
    ) -> Result<PackageSpec, DeclarationEvaluationError<PackageConversionError>> {
        let deadline = EvaluationDeadline::start(self.engine.limits().timeout);
        Ok(self
            .evaluate_authored_package(source, &[], deadline)?
            .value)
    }
}

impl DeclarationEvaluator<PackageSpec> for GluonPackageEvaluator {
    type Identity = EvaluationIdentity;
    type Error = PackageConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<PackageSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_dispatched(source, &[], deadline)
    }
}

impl DeclarationInputEvaluator<PackageSpec> for GluonPackageEvaluator {
    fn evaluate_with_inputs_within(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
        ) -> Result<
        DeclarationEvaluation<PackageSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_dispatched(source, explicit_inputs, deadline)
    }
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
    tuning: Vec<GluonNamedTuningSpec>,
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
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPhaseSpec {
    steps: Vec<GluonStepSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonStepSpec {
    Run {
        program: GluonProgramSpec,
        args: Vec<String>,
    },
    RunBuilt {
        program: GluonBuiltProgramSpec,
        args: Vec<String>,
    },
    Shell {
        interpreter: GluonProgramSpec,
        declared_programs: Vec<GluonProgramSpec>,
        script: String,
    },
    CMakeConfigure {
        flags: Vec<String>,
    },
    CMakeBuild,
    CMakeInstall,
    CMakeTest,
    MesonSetup {
        flags: Vec<String>,
    },
    MesonBuild,
    MesonInstall,
    MesonTest,
    CargoBuild {
        features: Vec<String>,
    },
    CargoInstall {
        binaries: Vec<String>,
    },
    CargoTest {
        features: Vec<String>,
    },
    AutotoolsConfigure {
        flags: Vec<String>,
    },
    AutotoolsBuild,
    AutotoolsInstall,
    AutotoolsTest,
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
struct GluonProgramSpec {
    path: String,
    requirement: GluonDependencySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuiltProgramSpec {
    path: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
#[allow(clippy::enum_variant_names)] // Gluon constructors share one namespace with dependency variants.
enum GluonBuilderEnvironmentSpec {
    CMakeEnvironment,
    MesonEnvironment,
    CargoEnvironment,
    AutotoolsEnvironment,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSupportedHooksSpec {
    setup: GluonBool,
    build: GluonBool,
    check: GluonBool,
    install: GluonBool,
    workload: GluonBool,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuilderSpec {
    required_tools: Vec<GluonDependencySpec>,
    environment: Vec<GluonBuilderEnvironmentSpec>,
    phases: GluonScriptsSpec,
    supported_hooks: GluonSupportedHooksSpec,
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
    include_in_manifest: GluonBool,
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
struct GluonNamedTuningSpec {
    key: String,
    value: GluonTuningSpec,
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

/// The Gluon encoding of a [`BuilderRequest`] — the minimal builder authoring
/// surface. The standard build systems are typed ADT variants (constructed via
/// the `cast.authored.v1` prelude); the `Custom` data escape hatch is authored
/// as a complete [`BuilderSpec`] and so does not appear here.
#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBuilderRequest {
    Cmake { flags: Vec<String>, run_tests: GluonBool },
    Meson { flags: Vec<String>, run_tests: GluonBool },
    Cargo {
        features: Vec<String>,
        binaries: Vec<String>,
        run_tests: GluonBool,
    },
    Autotools { flags: Vec<String>, run_tests: GluonBool },
    Custom(GluonBuilderSpec),
}

impl From<GluonBuilderRequest> for BuilderRequest {
    fn from(request: GluonBuilderRequest) -> Self {
        match request {
            GluonBuilderRequest::Cmake { flags, run_tests } => BuilderRequest::Cmake {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderRequest::Meson { flags, run_tests } => BuilderRequest::Meson {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderRequest::Cargo {
                features,
                binaries,
                run_tests,
            } => BuilderRequest::Cargo {
                features,
                binaries,
                run_tests: run_tests.into(),
            },
            GluonBuilderRequest::Autotools { flags, run_tests } => BuilderRequest::Autotools {
                flags,
                run_tests: run_tests.into(),
            },
            GluonBuilderRequest::Custom(spec) => BuilderRequest::Custom(Box::new(spec.into())),
        }
    }
}

/// The Gluon encoding of the authored output-set choice. A dedicated type
/// (rather than [`GluonOptional`]) is required because [`GluonOutputSpec`]
/// carries its own optional fields, and nesting `Optional` within `Optional`
/// defeats Gluon's unifier. `DefaultOutputs` selects the shared default set.
#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOutputsChoice {
    DefaultOutputs,
    ExplicitOutputs(Vec<GluonOutputSpec>),
    WithRoot(GluonOutputSpec),
}

impl GluonOutputsChoice {
    /// Resolve the authored output choice into the explicit list, or `None` for
    /// the default split-output set. `WithRoot` overlays an authored root onto
    /// the default set, which requires the package name.
    fn resolve(self, pname: &str) -> Option<Vec<OutputSpec>> {
        match self {
            GluonOutputsChoice::DefaultOutputs => None,
            GluonOutputsChoice::ExplicitOutputs(outputs) => {
                Some(outputs.into_iter().map(Into::into).collect())
            }
            GluonOutputsChoice::WithRoot(root) => {
                Some(default_output_set_with_root(pname, root.into()))
            }
        }
    }
}

/// The Gluon encoding of an [`AuthoredPackage`] — the minimal, language-agnostic
/// authoring surface. Every optional (`outputs`, `options`, `hooks`) is an
/// [`GluonOptional`]: `unset` selects the shared package-ABI default that the
/// Rust [`lower`] fills. This is the Gluon half of the proof that authoring
/// lives in shared Rust rather than in a config language.
#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonAuthoredPackage {
    meta: GluonMetaSpec,
    builder: GluonBuilderRequest,
    sources: Vec<GluonUpstreamSpec>,
    native_build_inputs: Vec<GluonDependencySpec>,
    build_inputs: Vec<GluonDependencySpec>,
    check_inputs: Vec<GluonDependencySpec>,
    outputs: GluonOutputsChoice,
    options: GluonOptional<GluonOptionsSpec>,
    profiles: Vec<GluonProfileSpec>,
    architectures: Vec<String>,
    tuning: Vec<GluonNamedTuningSpec>,
    emul32: GluonBool,
    mold: GluonBool,
    hooks: GluonOptional<GluonHooksSpec>,
}

impl From<GluonAuthoredPackage> for AuthoredPackage {
    fn from(package: GluonAuthoredPackage) -> Self {
        let outputs = package.outputs.resolve(&package.meta.pname);
        Self {
            meta: package.meta.into(),
            builder: package.builder.into(),
            sources: package.sources.into_iter().map(Into::into).collect(),
            native_build_inputs: package.native_build_inputs.into_iter().map(Into::into).collect(),
            build_inputs: package.build_inputs.into_iter().map(Into::into).collect(),
            check_inputs: package.check_inputs.into_iter().map(Into::into).collect(),
            outputs,
            options: Option::<GluonOptionsSpec>::from(package.options).map(Into::into),
            profiles: package.profiles.into_iter().map(Into::into).collect(),
            architectures: package.architectures,
            tuning: package.tuning.into_iter().map(Into::into).collect(),
            emul32: package.emul32.into(),
            mold: package.mold.into(),
            hooks: Option::<GluonHooksSpec>::from(package.hooks)
                .map(Into::into)
                .unwrap_or_default(),
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

impl From<GluonScriptsSpec> for PhasesSpec {
    fn from(spec: GluonScriptsSpec) -> Self {
        Self {
            setup: spec.setup.into(),
            build: spec.build.into(),
            install: spec.install.into(),
            check: spec.check.into(),
            workload: spec.workload.into(),
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
            GluonStepSpec::Run { program, args } => Self::Run {
                program: program.into(),
                args,
            },
            GluonStepSpec::RunBuilt { program, args } => Self::RunBuilt {
                program: program.into(),
                args,
            },
            GluonStepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            } => Self::Shell {
                interpreter: interpreter.into(),
                declared_programs: declared_programs.into_iter().map(Into::into).collect(),
                script,
            },
            GluonStepSpec::CMakeConfigure { flags } => Self::CMakeConfigure { flags },
            GluonStepSpec::CMakeBuild => Self::CMakeBuild,
            GluonStepSpec::CMakeInstall => Self::CMakeInstall,
            GluonStepSpec::CMakeTest => Self::CMakeTest,
            GluonStepSpec::MesonSetup { flags } => Self::MesonSetup { flags },
            GluonStepSpec::MesonBuild => Self::MesonBuild,
            GluonStepSpec::MesonInstall => Self::MesonInstall,
            GluonStepSpec::MesonTest => Self::MesonTest,
            GluonStepSpec::CargoBuild { features } => Self::CargoBuild { features },
            GluonStepSpec::CargoInstall { binaries } => Self::CargoInstall { binaries },
            GluonStepSpec::CargoTest { features } => Self::CargoTest { features },
            GluonStepSpec::AutotoolsConfigure { flags } => Self::AutotoolsConfigure { flags },
            GluonStepSpec::AutotoolsBuild => Self::AutotoolsBuild,
            GluonStepSpec::AutotoolsInstall => Self::AutotoolsInstall,
            GluonStepSpec::AutotoolsTest => Self::AutotoolsTest,
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

impl From<GluonProgramSpec> for ProgramSpec {
    fn from(spec: GluonProgramSpec) -> Self {
        Self {
            path: spec.path,
            requirement: spec.requirement.into(),
        }
    }
}

impl From<GluonBuiltProgramSpec> for BuiltProgramSpec {
    fn from(spec: GluonBuiltProgramSpec) -> Self {
        Self { path: spec.path }
    }
}

impl From<GluonBuilderEnvironmentSpec> for BuilderEnvironmentSpec {
    fn from(spec: GluonBuilderEnvironmentSpec) -> Self {
        match spec {
            GluonBuilderEnvironmentSpec::CMakeEnvironment => Self::CMake,
            GluonBuilderEnvironmentSpec::MesonEnvironment => Self::Meson,
            GluonBuilderEnvironmentSpec::CargoEnvironment => Self::Cargo,
            GluonBuilderEnvironmentSpec::AutotoolsEnvironment => Self::Autotools,
        }
    }
}

impl From<GluonSupportedHooksSpec> for SupportedHooksSpec {
    fn from(spec: GluonSupportedHooksSpec) -> Self {
        Self {
            setup: spec.setup.into(),
            build: spec.build.into(),
            check: spec.check.into(),
            install: spec.install.into(),
            workload: spec.workload.into(),
        }
    }
}

impl From<GluonBuilderSpec> for BuilderSpec {
    fn from(spec: GluonBuilderSpec) -> Self {
        Self {
            required_tools: spec.required_tools.into_iter().map(Into::into).collect(),
            environment: spec.environment.into_iter().map(Into::into).collect(),
            phases: spec.phases.into(),
            supported_hooks: spec.supported_hooks.into(),
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
            include_in_manifest: spec.include_in_manifest.into(),
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

impl From<GluonNamedTuningSpec> for NamedTuningSpec {
    fn from(spec: GluonNamedTuningSpec) -> Self {
        Self {
            key: spec.key,
            value: spec.value.into(),
        }
    }
}
