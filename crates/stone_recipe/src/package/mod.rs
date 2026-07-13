// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed package declarations for the `boulder.package.v3` Gluon ABI.
//!
//! A package factory is evaluated completely inside Gluon and produces one
//! concrete [`PackageSpec`]. This module deliberately contains values only:
//! Rust never receives or retains a Gluon closure or a second recipe model.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use crate::{
    NamedTuningSpec, OptionsSpec, PathSpec, UpstreamSpec,
    spec::{UpstreamValidationError, is_safe_artifact_component},
};
use stone::relation::{Dependency, Kind as RelationKind, ParseError, Provider};
use thiserror::Error;

pub use self::gluon::{
    EvaluatedPackage, GLUON_AUTOTOOLS_BUILDER_ABI, GLUON_CARGO_BUILDER_ABI, GLUON_CMAKE_BUILDER_ABI,
    GLUON_MESON_BUILDER_ABI, GLUON_PACKAGE_ABI, PACKAGE_ABI_VERSION, PackageEvaluationError, evaluate_gluon,
    evaluate_gluon_with, evaluate_gluon_with_inputs,
};

mod gluon;

/// One pure, concrete package declaration returned by a Gluon package factory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    pub meta: MetaSpec,
    pub builder: BuilderSpec,
    pub hooks: HooksSpec,
    pub native_build_inputs: Vec<DependencySpec>,
    pub build_inputs: Vec<DependencySpec>,
    pub check_inputs: Vec<DependencySpec>,
    pub outputs: Vec<OutputSpec>,
    pub options: OptionsSpec,
    pub profiles: Vec<ProfileSpec>,
    pub sources: Vec<UpstreamSpec>,
    pub architectures: Vec<String>,
    pub tuning: Vec<NamedTuningSpec>,
    pub emul32: bool,
    pub mold: bool,
}

/// Package identity and user-facing source metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaSpec {
    pub pname: String,
    pub version: String,
    pub release: i64,
    pub homepage: String,
    pub license: Vec<String>,
}

/// One executable, structural build step.
///
/// Standard builders use dedicated variants. [`Self::Run`] executes one
/// declared program directly. [`Self::Shell`] is the only escape hatch for
/// commands which cannot be expressed structurally, and makes both its
/// interpreter and every additional executable capability explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepSpec {
    Run {
        program: ProgramSpec,
        args: Vec<String>,
    },
    Shell {
        interpreter: ProgramSpec,
        declared_programs: Vec<ProgramSpec>,
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
    CargoFetch,
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

/// Ordered steps executed in one build phase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseSpec {
    pub steps: Vec<StepSpec>,
}

impl PhaseSpec {
    pub fn new(steps: impl IntoIterator<Item = StepSpec>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Complete structural phase set selected for one target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhasesSpec {
    pub setup: PhaseSpec,
    pub build: PhaseSpec,
    pub install: PhaseSpec,
    pub check: PhaseSpec,
    pub workload: PhaseSpec,
}

/// Commands inserted around builder-owned phase bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksSpec {
    pub pre_setup: Vec<StepSpec>,
    pub post_setup: Vec<StepSpec>,
    pub pre_build: Vec<StepSpec>,
    pub post_build: Vec<StepSpec>,
    pub pre_check: Vec<StepSpec>,
    pub post_check: Vec<StepSpec>,
    pub pre_install: Vec<StepSpec>,
    pub post_install: Vec<StepSpec>,
    pub pre_workload: Vec<StepSpec>,
    pub post_workload: Vec<StepSpec>,
}

/// One repository-owned environment layer selected by a pure builder module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuilderEnvironmentSpec {
    CMake,
    Meson,
    Cargo,
    Autotools,
}

/// Hook phases accepted by one structural builder contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SupportedHooksSpec {
    pub setup: bool,
    pub build: bool,
    pub check: bool,
    pub install: bool,
    pub workload: bool,
}

impl SupportedHooksSpec {
    pub const fn all() -> Self {
        Self {
            setup: true,
            build: true,
            check: true,
            install: true,
            workload: true,
        }
    }
}

/// A completely structural build contract returned by a pure Gluon module.
///
/// The module owns phase membership, symbolic tool capabilities, environment
/// selection, and the supported hook surface. Repository policy remains the
/// sole owner of the typed commands and bindings selected by these markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderSpec {
    pub required_tools: Vec<DependencySpec>,
    pub environment: Vec<BuilderEnvironmentSpec>,
    pub phases: PhasesSpec,
    pub supported_hooks: SupportedHooksSpec,
}

impl Default for BuilderSpec {
    fn default() -> Self {
        Self {
            required_tools: Vec::new(),
            environment: Vec::new(),
            phases: PhasesSpec::default(),
            supported_hooks: SupportedHooksSpec::all(),
        }
    }
}

/// A target-specific package build profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSpec {
    pub name: String,
    pub builder: BuilderSpec,
    pub hooks: HooksSpec,
    pub native_build_inputs: Vec<DependencySpec>,
    pub build_inputs: Vec<DependencySpec>,
    pub check_inputs: Vec<DependencySpec>,
}

/// One explicit Stone output.
///
/// `out` is the root package. Other local names lower temporarily to
/// `<pname>-<name>` subpackages until Boulder and Stone consume output names
/// directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    pub name: String,
    /// Whether this output participates in build manifests. The Stone itself
    /// is emitted regardless, including when the output is empty.
    pub include_in_manifest: bool,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_inputs: Vec<DependencySpec>,
    pub runtime_exclude: Vec<String>,
    pub paths: Vec<PathSpec>,
    pub conflicts: Vec<DependencySpec>,
}

/// A symbolic package name supplied to a package factory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageRef {
    pub name: String,
}

/// A named output of a symbolic package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutputRef {
    pub package: PackageRef,
    pub output: String,
}

/// A typed package relationship converted through the shared Stone relation
/// model whenever Boulder crosses into package resolution or metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencySpec {
    Package(PackageRef),
    Output(OutputRef),
    Binary(String),
    SystemBinary(String),
    PkgConfig(String),
    PkgConfig32(String),
    Soname(String),
    CMake(String),
    Python(String),
    Interpreter(String),
}

/// One executable path bound to the dependency capability which supplies it.
///
/// The path is guest-visible and absolute. `Binary` and `SystemBinary`
/// requirements are bound to their canonical `/usr/bin` and `/usr/sbin`
/// locations; package and output requirements may expose a program at another
/// normalized absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramSpec {
    pub path: String,
    pub requirement: DependencySpec,
}

impl DependencySpec {
    /// Convert the authored typed value into the canonical shared dependency.
    pub fn dependency(&self) -> Result<Dependency, ParseError> {
        let (kind, target) = self.kind_and_target()?;
        Dependency::new(kind, target)
    }

    /// Convert an output conflict into the canonical shared provider.
    pub fn provider(&self) -> Result<Provider, ParseError> {
        let (kind, target) = self.kind_and_target()?;
        Provider::new(kind, target)
    }

    fn kind_and_target(&self) -> Result<(RelationKind, String), ParseError> {
        Ok(match self {
            Self::Package(package) => (RelationKind::PackageName, package.name.clone()),
            Self::Output(output) => {
                Dependency::new(RelationKind::PackageName, output.package.name.clone())?;
                if output.output.is_empty() {
                    return Err(ParseError::EmptyTarget {
                        value: output.output.clone(),
                    });
                }
                if output.output == "out" {
                    (RelationKind::PackageName, output.package.name.clone())
                } else {
                    (
                        RelationKind::PackageName,
                        format!("{}-{}", output.package.name, output.output),
                    )
                }
            }
            Self::Binary(target) => (RelationKind::Binary, target.clone()),
            Self::SystemBinary(target) => (RelationKind::SystemBinary, target.clone()),
            Self::PkgConfig(target) => (RelationKind::PkgConfig, target.clone()),
            Self::PkgConfig32(target) => (RelationKind::PkgConfig32, target.clone()),
            Self::Soname(target) => (RelationKind::SharedLibrary, target.clone()),
            Self::CMake(target) => (RelationKind::CMake, target.clone()),
            Self::Python(target) => (RelationKind::Python, target.clone()),
            Self::Interpreter(target) => (RelationKind::Interpreter, target.clone()),
        })
    }

    fn package_and_output(&self) -> Option<(&str, &str)> {
        match self {
            Self::Package(package) => Some((&package.name, "out")),
            Self::Output(output) => Some((&output.package.name, &output.output)),
            _ => None,
        }
    }
}

/// Failure to validate a concrete package-v3 declaration.
#[derive(Debug, Error)]
pub enum PackageConversionError {
    #[error("meta.pname: package name `{name}` must use only ASCII letters, digits, '+', '-', '.', or '_'")]
    InvalidPackageName { name: String },
    #[error("meta.version: version must start with an integer (found `{version}`)")]
    VersionMustStartWithDigit { version: String },
    #[error("meta.version: version `{version}` must be one normalized filename component")]
    InvalidVersionComponent { version: String },
    #[error("meta.release: release must be greater than zero (found `{release}`)")]
    ReleaseMustBePositive { release: i64 },
    #[error(
        "options.networking: frozen builds must declare fetched content as locked sources; network access during execution is unsupported"
    )]
    FrozenBuildNetworkingUnsupported,
    #[error("{field}: {source}")]
    InvalidSource {
        field: String,
        #[source]
        source: UpstreamValidationError,
    },
    #[error("{field}: materialization destination `{value}` duplicates `{first_field}`")]
    DuplicateSourceMaterialization {
        field: String,
        value: String,
        first_field: String,
    },
    #[error("{field}: invalid dependency: {source}")]
    InvalidDependency {
        field: String,
        #[source]
        source: ParseError,
    },
    #[error("{field}: invalid provider: {source}")]
    InvalidProvider {
        field: String,
        #[source]
        source: ParseError,
    },
    #[error("outputs: package must declare exactly one `out` output")]
    MissingRootOutput,
    #[error("outputs[{index}].name: duplicate output name `{name}`")]
    DuplicateOutput { index: usize, name: String },
    #[error("outputs[{index}].name: invalid output name `{name}`")]
    InvalidOutputName { index: usize, name: String },
    #[error("profiles[{index}].name: profile name `{name}` must be a normalized safe relative path")]
    InvalidProfileName { index: usize, name: String },
    #[error(
        "profiles[{duplicate_index}].name: duplicate profile name `{name}`; first declared at profiles[{first_index}].name"
    )]
    DuplicateProfileName {
        first_index: usize,
        duplicate_index: usize,
        name: String,
    },
    #[error("{field}: output `{output}` does not exist in package `{package}`")]
    MissingOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: package output dependency cycle: {cycle}")]
    OutputDependencyCycle { field: String, cycle: String },
    #[error("{field}: duplicate builder environment marker `{environment:?}`")]
    DuplicateBuilderEnvironment {
        field: String,
        environment: BuilderEnvironmentSpec,
    },
    #[error("{field}: hook is not supported by the selected builder")]
    UnsupportedBuilderHook { field: String },
    #[error("{field}: program path `{value}` must be a normalized non-root absolute path")]
    InvalidProgramPath { field: String, value: String },
    #[error("{field}: {requirement:?} is not an executable program capability")]
    UnsupportedProgramRequirement { field: String, requirement: DependencySpec },
    #[error("{field}: {requirement:?} is not a normalized executable capability")]
    InvalidProgramRequirement { field: String, requirement: DependencySpec },
    #[error("{field}: package/output program path `{value}` is ambiguous under the canonical binary directories")]
    AmbiguousPackageProgramPath { field: String, value: String },
    #[error("{field}: program path `{actual}` does not match the canonical path `{expected}` for {requirement:?}")]
    ProgramRequirementPathMismatch {
        field: String,
        requirement: DependencySpec,
        expected: String,
        actual: String,
    },
}

impl PackageConversionError {
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidPackageName { .. } => "meta.pname",
            Self::VersionMustStartWithDigit { .. } => "meta.version",
            Self::InvalidVersionComponent { .. } => "meta.version",
            Self::ReleaseMustBePositive { .. } => "meta.release",
            Self::FrozenBuildNetworkingUnsupported => "options.networking",
            Self::InvalidSource { field, .. }
            | Self::DuplicateSourceMaterialization { field, .. }
            | Self::InvalidDependency { field, .. }
            | Self::InvalidProvider { field, .. }
            | Self::MissingOutputReference { field, .. }
            | Self::OutputDependencyCycle { field, .. }
            | Self::DuplicateBuilderEnvironment { field, .. }
            | Self::UnsupportedBuilderHook { field }
            | Self::InvalidProgramPath { field, .. }
            | Self::UnsupportedProgramRequirement { field, .. }
            | Self::InvalidProgramRequirement { field, .. }
            | Self::AmbiguousPackageProgramPath { field, .. }
            | Self::ProgramRequirementPathMismatch { field, .. } => field,
            Self::MissingRootOutput => "outputs",
            Self::DuplicateOutput { .. } | Self::InvalidOutputName { .. } => "outputs",
            Self::InvalidProfileName { .. } | Self::DuplicateProfileName { .. } => "profiles",
        }
    }
}

impl PackageSpec {
    /// Validate the concrete package declaration without lowering it through
    /// the transitional recipe model.
    pub fn validate(&self) -> Result<(), PackageConversionError> {
        if !valid_package_name(&self.meta.pname) {
            return Err(PackageConversionError::InvalidPackageName {
                name: self.meta.pname.clone(),
            });
        }
        if !self
            .meta
            .version
            .starts_with(|character: char| character.is_ascii_digit())
        {
            return Err(PackageConversionError::VersionMustStartWithDigit {
                version: self.meta.version.clone(),
            });
        }
        if !is_safe_artifact_component(&self.meta.version) {
            return Err(PackageConversionError::InvalidVersionComponent {
                version: self.meta.version.clone(),
            });
        }
        if self.meta.release <= 0 {
            return Err(PackageConversionError::ReleaseMustBePositive {
                release: self.meta.release,
            });
        }
        if self.options.networking {
            return Err(PackageConversionError::FrozenBuildNetworkingUnsupported);
        }

        let mut source_destinations = BTreeMap::<String, (usize, &'static str)>::new();
        for (index, source) in self.sources.iter().enumerate() {
            source
                .validate()
                .map_err(|source_error| PackageConversionError::InvalidSource {
                    field: format!("sources[{index}].{}", source_error.field()),
                    source: source_error,
                })?;
            let destination =
                source
                    .materialization_name()
                    .map_err(|source_error| PackageConversionError::InvalidSource {
                        field: format!("sources[{index}].{}", source_error.field()),
                        source: source_error,
                    })?;
            let destination_field = source.materialization_field();
            if let Some((first_index, first_destination_field)) =
                source_destinations.insert(destination.clone(), (index, destination_field))
            {
                return Err(PackageConversionError::DuplicateSourceMaterialization {
                    field: format!("sources[{index}].{destination_field}"),
                    value: destination,
                    first_field: format!("sources[{first_index}].{first_destination_field}"),
                });
            }
        }

        let mut profile_names = BTreeMap::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            if !valid_profile_name(&profile.name) {
                return Err(PackageConversionError::InvalidProfileName {
                    index,
                    name: profile.name.clone(),
                });
            }
            if let Some(first_index) = profile_names.insert(profile.name.as_str(), index) {
                return Err(PackageConversionError::DuplicateProfileName {
                    first_index,
                    duplicate_index: index,
                    name: profile.name.clone(),
                });
            }
        }

        Self::validate_builder_contract(&self.builder, &self.hooks, "builder", "hooks")?;
        for (index, profile) in self.profiles.iter().enumerate() {
            Self::validate_builder_contract(
                &profile.builder,
                &profile.hooks,
                &format!("profiles[{index}].builder"),
                &format!("profiles[{index}].hooks"),
            )?;
        }

        self.validate_relations()
    }

    pub fn phases(&self) -> PhasesSpec {
        self.phases_for_profile(None)
    }

    /// Find a target-specific profile by its declarative name.
    pub fn profile(&self, name: &str) -> Option<&ProfileSpec> {
        self.profiles.iter().find(|profile| profile.name == name)
    }

    /// Select a builder for a known profile, falling back to the package's
    /// base builder when no matching profile was requested.
    pub fn builder_for_profile(&self, profile: Option<&str>) -> &BuilderSpec {
        self.selected_profile(profile)
            .map_or(&self.builder, |profile| &profile.builder)
    }

    /// Select hooks for a known profile, falling back to the package's base
    /// hooks when no matching profile was requested.
    pub fn hooks_for_profile(&self, profile: Option<&str>) -> &HooksSpec {
        self.selected_profile(profile)
            .map_or(&self.hooks, |profile| &profile.hooks)
    }

    /// Select structural phases for one optional target profile.
    pub fn phases_for_profile(&self, profile: Option<&str>) -> PhasesSpec {
        self.builder_for_profile(profile)
            .phases(self.hooks_for_profile(profile))
    }

    /// Select target-native build inputs for one optional profile. Builder
    /// capability requirements remain available on the selected
    /// [`BuilderSpec`] and are resolved alongside these package inputs.
    pub fn native_build_inputs_for_profile(&self, profile: Option<&str>) -> &[DependencySpec] {
        self.selected_profile(profile)
            .map_or(&self.native_build_inputs, |profile| &profile.native_build_inputs)
    }

    /// Select target build inputs for one optional profile.
    pub fn build_inputs_for_profile(&self, profile: Option<&str>) -> &[DependencySpec] {
        self.selected_profile(profile)
            .map_or(&self.build_inputs, |profile| &profile.build_inputs)
    }

    /// Select target check inputs for one optional profile.
    pub fn check_inputs_for_profile(&self, profile: Option<&str>) -> &[DependencySpec] {
        self.selected_profile(profile)
            .map_or(&self.check_inputs, |profile| &profile.check_inputs)
    }

    fn selected_profile(&self, profile: Option<&str>) -> Option<&ProfileSpec> {
        profile.and_then(|name| self.profile(name))
    }

    fn validate_builder_contract(
        builder: &BuilderSpec,
        hooks: &HooksSpec,
        builder_field: &str,
        hooks_field: &str,
    ) -> Result<(), PackageConversionError> {
        let mut environments = BTreeSet::new();
        for (index, environment) in builder.environment.iter().copied().enumerate() {
            if !environments.insert(environment) {
                return Err(PackageConversionError::DuplicateBuilderEnvironment {
                    field: format!("{builder_field}.environment[{index}]"),
                    environment,
                });
            }
        }

        for (field, supported, populated) in [
            ("pre_setup", builder.supported_hooks.setup, !hooks.pre_setup.is_empty()),
            (
                "post_setup",
                builder.supported_hooks.setup,
                !hooks.post_setup.is_empty(),
            ),
            ("pre_build", builder.supported_hooks.build, !hooks.pre_build.is_empty()),
            (
                "post_build",
                builder.supported_hooks.build,
                !hooks.post_build.is_empty(),
            ),
            ("pre_check", builder.supported_hooks.check, !hooks.pre_check.is_empty()),
            (
                "post_check",
                builder.supported_hooks.check,
                !hooks.post_check.is_empty(),
            ),
            (
                "pre_install",
                builder.supported_hooks.install,
                !hooks.pre_install.is_empty(),
            ),
            (
                "post_install",
                builder.supported_hooks.install,
                !hooks.post_install.is_empty(),
            ),
            (
                "pre_workload",
                builder.supported_hooks.workload,
                !hooks.pre_workload.is_empty(),
            ),
            (
                "post_workload",
                builder.supported_hooks.workload,
                !hooks.post_workload.is_empty(),
            ),
        ] {
            if populated && !supported {
                return Err(PackageConversionError::UnsupportedBuilderHook {
                    field: format!("{hooks_field}.{field}"),
                });
            }
        }

        Ok(())
    }

    fn validate_relations(&self) -> Result<(), PackageConversionError> {
        let mut outputs = BTreeMap::new();
        for (index, output) in self.outputs.iter().enumerate() {
            if !valid_output_name(&output.name) {
                return Err(PackageConversionError::InvalidOutputName {
                    index,
                    name: output.name.clone(),
                });
            }
            if outputs.insert(output.name.as_str(), index).is_some() {
                return Err(PackageConversionError::DuplicateOutput {
                    index,
                    name: output.name.clone(),
                });
            }
        }
        if !outputs.contains_key("out") {
            return Err(PackageConversionError::MissingRootOutput);
        }

        self.validate_dependency_list(&self.native_build_inputs, "native_build_inputs", &outputs, false)?;
        self.validate_dependency_list(&self.build_inputs, "build_inputs", &outputs, false)?;
        self.validate_dependency_list(&self.check_inputs, "check_inputs", &outputs, false)?;
        self.validate_dependency_list(&self.builder.required_tools, "builder.required_tools", &outputs, false)?;
        self.validate_builder_programs(&self.builder, &self.hooks, "builder", "hooks", &outputs)?;

        for (index, profile) in self.profiles.iter().enumerate() {
            let parent = format!("profiles[{index}]");
            self.validate_dependency_list(
                &profile.builder.required_tools,
                &format!("{parent}.builder.required_tools"),
                &outputs,
                false,
            )?;
            self.validate_dependency_list(
                &profile.native_build_inputs,
                &format!("{parent}.native_build_inputs"),
                &outputs,
                false,
            )?;
            self.validate_dependency_list(
                &profile.build_inputs,
                &format!("{parent}.build_inputs"),
                &outputs,
                false,
            )?;
            self.validate_dependency_list(
                &profile.check_inputs,
                &format!("{parent}.check_inputs"),
                &outputs,
                false,
            )?;
            self.validate_builder_programs(
                &profile.builder,
                &profile.hooks,
                &format!("{parent}.builder"),
                &format!("{parent}.hooks"),
                &outputs,
            )?;
        }

        for (index, output) in self.outputs.iter().enumerate() {
            self.validate_dependency_list(
                &output.runtime_inputs,
                &format!("outputs[{index}].runtime_inputs"),
                &outputs,
                false,
            )?;
            self.validate_dependency_list(
                &output.conflicts,
                &format!("outputs[{index}].conflicts"),
                &outputs,
                true,
            )?;
        }

        self.validate_output_cycles(&outputs)
    }

    fn validate_dependency_list(
        &self,
        dependencies: &[DependencySpec],
        field: &str,
        outputs: &BTreeMap<&str, usize>,
        provider: bool,
    ) -> Result<(), PackageConversionError> {
        for (index, dependency) in dependencies.iter().enumerate() {
            let field = format!("{field}[{index}]");
            self.validate_dependency(dependency, &field, outputs, provider)?;
        }
        Ok(())
    }

    fn validate_dependency(
        &self,
        dependency: &DependencySpec,
        field: &str,
        outputs: &BTreeMap<&str, usize>,
        provider: bool,
    ) -> Result<(), PackageConversionError> {
        let parsed = if provider {
            dependency.provider().map(|_| ())
        } else {
            dependency.dependency().map(|_| ())
        };
        parsed.map_err(|source| {
            if provider {
                PackageConversionError::InvalidProvider {
                    field: field.to_owned(),
                    source,
                }
            } else {
                PackageConversionError::InvalidDependency {
                    field: field.to_owned(),
                    source,
                }
            }
        })?;
        if let Some((package, output)) = dependency.package_and_output()
            && package == self.meta.pname
            && !outputs.contains_key(output)
        {
            return Err(PackageConversionError::MissingOutputReference {
                field: field.to_owned(),
                package: package.to_owned(),
                output: output.to_owned(),
            });
        }
        Ok(())
    }

    fn validate_builder_programs(
        &self,
        builder: &BuilderSpec,
        hooks: &HooksSpec,
        builder_field: &str,
        hooks_field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        for (name, phase) in [
            ("setup", &builder.phases.setup),
            ("build", &builder.phases.build),
            ("install", &builder.phases.install),
            ("check", &builder.phases.check),
            ("workload", &builder.phases.workload),
        ] {
            self.validate_steps(&phase.steps, &format!("{builder_field}.phases.{name}.steps"), outputs)?;
        }

        for (name, steps) in [
            ("pre_setup", hooks.pre_setup.as_slice()),
            ("post_setup", hooks.post_setup.as_slice()),
            ("pre_build", hooks.pre_build.as_slice()),
            ("post_build", hooks.post_build.as_slice()),
            ("pre_check", hooks.pre_check.as_slice()),
            ("post_check", hooks.post_check.as_slice()),
            ("pre_install", hooks.pre_install.as_slice()),
            ("post_install", hooks.post_install.as_slice()),
            ("pre_workload", hooks.pre_workload.as_slice()),
            ("post_workload", hooks.post_workload.as_slice()),
        ] {
            self.validate_steps(steps, &format!("{hooks_field}.{name}"), outputs)?;
        }

        Ok(())
    }

    fn validate_steps(
        &self,
        steps: &[StepSpec],
        field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        for (index, step) in steps.iter().enumerate() {
            let field = format!("{field}[{index}]");
            match step {
                StepSpec::Run { program, .. } => {
                    self.validate_program(program, &format!("{field}.program"), outputs)?;
                }
                StepSpec::Shell {
                    interpreter,
                    declared_programs,
                    ..
                } => {
                    self.validate_program(interpreter, &format!("{field}.interpreter"), outputs)?;
                    for (program_index, program) in declared_programs.iter().enumerate() {
                        self.validate_program(
                            program,
                            &format!("{field}.declared_programs[{program_index}]"),
                            outputs,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_program(
        &self,
        program: &ProgramSpec,
        field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        let path_field = format!("{field}.path");
        if !valid_program_path(&program.path) {
            return Err(PackageConversionError::InvalidProgramPath {
                field: path_field,
                value: program.path.clone(),
            });
        }

        let requirement_field = format!("{field}.requirement");
        self.validate_dependency(&program.requirement, &requirement_field, outputs, false)?;

        let expected = match &program.requirement {
            DependencySpec::Package(_) | DependencySpec::Output(_) => {
                if Path::new(&program.path)
                    .parent()
                    .is_some_and(|parent| parent == Path::new("/usr/bin") || parent == Path::new("/usr/sbin"))
                {
                    return Err(PackageConversionError::AmbiguousPackageProgramPath {
                        field: path_field,
                        value: program.path.clone(),
                    });
                }
                return Ok(());
            }
            DependencySpec::Binary(target) => {
                if !is_safe_artifact_component(target) {
                    return Err(PackageConversionError::InvalidProgramRequirement {
                        field: requirement_field,
                        requirement: program.requirement.clone(),
                    });
                }
                format!("/usr/bin/{target}")
            }
            DependencySpec::SystemBinary(target) => {
                if !is_safe_artifact_component(target) {
                    return Err(PackageConversionError::InvalidProgramRequirement {
                        field: requirement_field,
                        requirement: program.requirement.clone(),
                    });
                }
                format!("/usr/sbin/{target}")
            }
            requirement => {
                return Err(PackageConversionError::UnsupportedProgramRequirement {
                    field: requirement_field,
                    requirement: requirement.clone(),
                });
            }
        };
        if program.path != expected {
            return Err(PackageConversionError::ProgramRequirementPathMismatch {
                field: path_field,
                requirement: program.requirement.clone(),
                expected,
                actual: program.path.clone(),
            });
        }

        Ok(())
    }

    fn validate_output_cycles(&self, outputs: &BTreeMap<&str, usize>) -> Result<(), PackageConversionError> {
        let mut edges = BTreeMap::<&str, Vec<(&str, String)>>::new();
        for (index, output) in self.outputs.iter().enumerate() {
            let dependencies = output
                .runtime_inputs
                .iter()
                .enumerate()
                .filter_map(|(dependency_index, dependency)| {
                    let (package, target) = dependency.package_and_output()?;
                    (package == self.meta.pname && outputs.contains_key(target))
                        .then(|| (target, format!("outputs[{index}].runtime_inputs[{dependency_index}]")))
                })
                .collect();
            edges.insert(&output.name, dependencies);
        }

        for output in &self.outputs {
            let mut visiting = BTreeSet::new();
            let mut visited = BTreeSet::new();
            let mut path = Vec::new();
            if let Some((field, cycle)) = find_cycle(&output.name, &edges, &mut visiting, &mut visited, &mut path) {
                return Err(PackageConversionError::OutputDependencyCycle { field, cycle });
            }
        }
        Ok(())
    }
}

fn find_cycle<'a>(
    node: &'a str,
    edges: &BTreeMap<&'a str, Vec<(&'a str, String)>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<(String, String)> {
    if visited.contains(node) {
        return None;
    }
    if !visiting.insert(node) {
        let start = path.iter().position(|entry| *entry == node).unwrap_or(0);
        let mut cycle = path[start..].to_vec();
        cycle.push(node);
        return Some(("outputs".to_owned(), cycle.join(" -> ")));
    }

    path.push(node);
    for (target, field) in edges.get(node).into_iter().flatten() {
        if visiting.contains(target) {
            let start = path.iter().position(|entry| entry == target).unwrap_or(0);
            let mut cycle = path[start..].to_vec();
            cycle.push(target);
            return Some((field.clone(), cycle.join(" -> ")));
        }
        if let Some(cycle) = find_cycle(target, edges, visiting, visited, path) {
            return Some(cycle);
        }
    }
    path.pop();
    visiting.remove(node);
    visited.insert(node);
    None
}

pub(crate) fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_'))
}

fn valid_output_name(name: &str) -> bool {
    valid_package_name(name)
}

fn valid_profile_name(name: &str) -> bool {
    let path = Path::new(name);
    !path.is_absolute()
        && name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_program_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path[1..]
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

impl BuilderSpec {
    pub fn required_tools(&self) -> &[DependencySpec] {
        &self.required_tools
    }

    pub fn phases(&self, hooks: &HooksSpec) -> PhasesSpec {
        hooks.clone().apply(self.phases.clone())
    }
}

impl HooksSpec {
    fn apply(self, phases: PhasesSpec) -> PhasesSpec {
        PhasesSpec {
            setup: phase_with_hooks(self.pre_setup, phases.setup, self.post_setup),
            build: phase_with_hooks(self.pre_build, phases.build, self.post_build),
            check: phase_with_hooks(self.pre_check, phases.check, self.post_check),
            install: phase_with_hooks(self.pre_install, phases.install, self.post_install),
            workload: phase_with_hooks(self.pre_workload, phases.workload, self.post_workload),
        }
    }
}

fn phase_with_hooks(pre: Vec<StepSpec>, body: PhaseSpec, post: Vec<StepSpec>) -> PhaseSpec {
    PhaseSpec::new(pre.into_iter().chain(body.steps).chain(post))
}

impl ProfileSpec {
    pub fn phases(&self) -> PhasesSpec {
        self.builder.phases(&self.hooks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(name: &str) -> DependencySpec {
        DependencySpec::Package(PackageRef { name: name.to_owned() })
    }

    fn binary_program(name: &str) -> ProgramSpec {
        ProgramSpec {
            path: format!("/usr/bin/{name}"),
            requirement: DependencySpec::Binary(name.to_owned()),
        }
    }

    fn shell(script: &str) -> StepSpec {
        StepSpec::Shell {
            interpreter: binary_program("bash"),
            declared_programs: Vec::new(),
            script: script.to_owned(),
        }
    }

    fn structural_builder(
        environment: BuilderEnvironmentSpec,
        required_tools: Vec<DependencySpec>,
        phases: PhasesSpec,
    ) -> BuilderSpec {
        BuilderSpec {
            required_tools,
            environment: vec![environment],
            phases,
            supported_hooks: SupportedHooksSpec::all(),
        }
    }

    fn profile(name: &str) -> ProfileSpec {
        ProfileSpec {
            name: name.to_owned(),
            builder: BuilderSpec::default(),
            hooks: HooksSpec::default(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        }
    }

    fn archive_source() -> UpstreamSpec {
        UpstreamSpec::Archive {
            url: "https://example.com/source.tar.xz".to_owned(),
            hash: "a".repeat(64),
            rename: None,
            strip_dirs: None,
            unpack: true,
            unpack_dir: None,
        }
    }

    fn git_source() -> UpstreamSpec {
        UpstreamSpec::Git {
            url: "https://example.com/source.git".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: None,
        }
    }

    fn package() -> PackageSpec {
        PackageSpec {
            meta: MetaSpec {
                pname: "example".to_owned(),
                version: "1.0.0".to_owned(),
                release: 1,
                homepage: "https://example.com".to_owned(),
                license: vec!["MPL-2.0".to_owned()],
            },
            builder: BuilderSpec::default(),
            hooks: HooksSpec::default(),
            native_build_inputs: vec![DependencySpec::Binary("cmake".to_owned())],
            build_inputs: vec![dependency("zlib")],
            check_inputs: Vec::new(),
            outputs: vec![OutputSpec {
                name: "out".to_owned(),
                include_in_manifest: true,
                summary: None,
                description: None,
                provides_exclude: Vec::new(),
                runtime_inputs: Vec::new(),
                runtime_exclude: Vec::new(),
                paths: Vec::new(),
                conflicts: Vec::new(),
            }],
            options: OptionsSpec::default(),
            profiles: Vec::new(),
            sources: Vec::new(),
            architectures: Vec::new(),
            tuning: Vec::new(),
            emul32: false,
            mold: false,
        }
    }

    #[test]
    fn typed_dependencies_use_the_shared_relation_model() {
        let package = package();
        package.validate().unwrap();
        assert_eq!(
            package.native_build_inputs[0].dependency().unwrap().to_name(),
            "binary(cmake)"
        );
        assert_eq!(package.build_inputs[0].dependency().unwrap().to_name(), "zlib");
        assert_eq!(
            DependencySpec::Soname("libz.so.1".to_owned())
                .dependency()
                .unwrap()
                .kind,
            RelationKind::SharedLibrary
        );
    }

    #[test]
    fn canonical_relation_errors_keep_the_typed_package_field() {
        let mut dependency = package();
        dependency.native_build_inputs = vec![DependencySpec::Binary(String::new())];
        let error = dependency.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidDependency { .. }));
        assert_eq!(error.field(), "native_build_inputs[0]");

        let mut provider = package();
        provider.outputs[0].conflicts = vec![DependencySpec::PkgConfig(String::new())];
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidProvider { .. }));
        assert_eq!(error.field(), "outputs[0].conflicts[0]");

        let mut output = package();
        output.build_inputs = vec![DependencySpec::Output(OutputRef {
            package: PackageRef {
                name: "zlib".to_owned(),
            },
            output: String::new(),
        })];
        let error = output.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidDependency { .. }));
        assert_eq!(error.field(), "build_inputs[0]");
    }

    #[test]
    fn executable_steps_bind_normalized_paths_to_capabilities() {
        let mut valid = package();
        valid.builder.phases.build = PhaseSpec::new([StepSpec::Run {
            program: ProgramSpec {
                path: "/opt/tools/bin/codegen".to_owned(),
                requirement: dependency("codegen-tools"),
            },
            args: vec!["--frozen".to_owned()],
        }]);
        valid.validate().unwrap();

        let mut relative = package();
        relative.builder.phases.build = PhaseSpec::new([StepSpec::Run {
            program: ProgramSpec {
                path: "usr/bin/tool".to_owned(),
                requirement: DependencySpec::Binary("tool".to_owned()),
            },
            args: Vec::new(),
        }]);
        let error = relative.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidProgramPath { .. }));
        assert_eq!(error.field(), "builder.phases.build.steps[0].program.path");

        let mut mismatch = package();
        mismatch.builder.phases.build = PhaseSpec::new([StepSpec::Run {
            program: ProgramSpec {
                path: "/usr/bin/other".to_owned(),
                requirement: DependencySpec::Binary("tool".to_owned()),
            },
            args: Vec::new(),
        }]);
        let error = mismatch.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::ProgramRequirementPathMismatch { .. }
        ));
        assert_eq!(error.field(), "builder.phases.build.steps[0].program.path");

        let mut unsupported = package();
        unsupported.hooks.pre_install = vec![StepSpec::Shell {
            interpreter: binary_program("bash"),
            declared_programs: vec![ProgramSpec {
                path: "/usr/bin/pkg-config".to_owned(),
                requirement: DependencySpec::PkgConfig("libexample".to_owned()),
            }],
            script: "pkg-config --exists libexample".to_owned(),
        }];
        let error = unsupported.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::UnsupportedProgramRequirement { .. }
        ));
        assert_eq!(error.field(), "hooks.pre_install[0].declared_programs[0].requirement");
    }

    #[test]
    fn base_and_selected_profile_semantics_stay_structural() {
        let mut package = package();
        package.builder = structural_builder(
            BuilderEnvironmentSpec::CMake,
            vec![DependencySpec::Binary("cmake".to_owned())],
            PhasesSpec {
                setup: PhaseSpec::new([StepSpec::CMakeConfigure {
                    flags: vec!["-DBASE=ON".to_owned()],
                }]),
                build: PhaseSpec::new([StepSpec::CMakeBuild]),
                install: PhaseSpec::new([StepSpec::CMakeInstall]),
                ..PhasesSpec::default()
            },
        );
        package.native_build_inputs = vec![dependency("base-native")];
        package.build_inputs = vec![dependency("base-build")];
        package.check_inputs = vec![dependency("base-check")];
        package.profiles.push(ProfileSpec {
            name: "emul32/x86_64".to_owned(),
            builder: structural_builder(
                BuilderEnvironmentSpec::Cargo,
                vec![DependencySpec::Binary("cargo".to_owned())],
                PhasesSpec {
                    build: PhaseSpec::new([StepSpec::CargoBuild {
                        features: vec!["profile".to_owned()],
                    }]),
                    install: PhaseSpec::new([StepSpec::CargoInstall {
                        binaries: vec!["example".to_owned()],
                    }]),
                    check: PhaseSpec::new([StepSpec::CargoTest {
                        features: vec!["profile".to_owned()],
                    }]),
                    ..PhasesSpec::default()
                },
            ),
            hooks: HooksSpec {
                pre_build: vec![shell("prepare-profile")],
                ..HooksSpec::default()
            },
            native_build_inputs: vec![dependency("profile-native")],
            build_inputs: vec![dependency("profile-build")],
            check_inputs: vec![dependency("profile-check")],
        });

        assert_eq!(
            package.builder_for_profile(None).environment,
            [BuilderEnvironmentSpec::CMake]
        );
        assert_eq!(
            package.builder_for_profile(None).required_tools(),
            [DependencySpec::Binary("cmake".to_owned())]
        );
        assert_eq!(
            package.builder_for_profile(Some("emul32/x86_64")).environment,
            [BuilderEnvironmentSpec::Cargo]
        );
        assert_eq!(
            package.builder_for_profile(Some("emul32/x86_64")).required_tools(),
            [DependencySpec::Binary("cargo".to_owned())]
        );
        assert_eq!(
            package.phases_for_profile(Some("emul32/x86_64")).build.steps,
            [
                shell("prepare-profile"),
                StepSpec::CargoBuild {
                    features: vec!["profile".to_owned()]
                }
            ]
        );
        assert_eq!(
            package.native_build_inputs_for_profile(None),
            [dependency("base-native")]
        );
        assert_eq!(
            package.build_inputs_for_profile(Some("emul32/x86_64")),
            [dependency("profile-build")]
        );
        assert_eq!(
            package.check_inputs_for_profile(Some("emul32/x86_64")),
            [dependency("profile-check")]
        );
        assert_eq!(
            package.builder_for_profile(Some("missing")).environment,
            [BuilderEnvironmentSpec::CMake]
        );
    }

    #[test]
    fn profile_names_are_unique_normalized_target_keys() {
        for name in ["x86_64", "x86_64-v3x", "emul32/x86_64", "tier/.hidden"] {
            let mut spec = package();
            spec.profiles.push(profile(name));

            spec.validate()
                .unwrap_or_else(|error| panic!("profile name `{name}` was rejected: {error}"));
        }

        for name in [
            "",
            "/x86_64",
            "//x86_64",
            "emul32//x86_64",
            "emul32/",
            "./x86_64",
            "emul32/./x86_64",
            ".",
            "../x86_64",
            "emul32/../x86_64",
            "..",
        ] {
            let mut spec = package();
            spec.profiles.push(profile(name));

            let error = spec.validate().unwrap_err();
            assert!(
                matches!(
                    error,
                    PackageConversionError::InvalidProfileName {
                        index: 0,
                        name: ref found,
                    }
                        if found == name
                ),
                "profile name `{name}` was not rejected as an invalid target key: {error}"
            );
            assert_eq!(error.field(), "profiles");
            assert!(error.to_string().starts_with("profiles[0].name:"));
        }

        let mut duplicate = package();
        duplicate.profiles = vec![profile("native"), profile("emul32/x86_64"), profile("native")];

        let error = duplicate.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::DuplicateProfileName {
                first_index: 0,
                duplicate_index: 2,
                ref name,
            } if name == "native"
        ));
        assert_eq!(error.field(), "profiles");
        assert_eq!(
            error.to_string(),
            "profiles[2].name: duplicate profile name `native`; first declared at profiles[0].name"
        );
    }

    #[test]
    fn direct_metadata_and_source_validation_keep_package_field_paths() {
        for name in [
            "",
            ".",
            "..",
            "/tmp/escape",
            "../../escape",
            "name/child",
            "name\\child",
        ] {
            let mut invalid = package();
            invalid.meta.pname = name.to_owned();
            let error = invalid.validate().unwrap_err();
            assert!(matches!(error, PackageConversionError::InvalidPackageName { .. }));
            assert_eq!(error.field(), "meta.pname");
        }

        let mut invalid = package();
        invalid.meta.version = "v1.0".to_owned();
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::VersionMustStartWithDigit { .. }
        ));
        assert_eq!(error.field(), "meta.version");

        for version in ["1/../../escape", "1\\escape", "1\ninvalid"] {
            let mut invalid = package();
            invalid.meta.version = version.to_owned();
            let error = invalid.validate().unwrap_err();
            assert!(matches!(error, PackageConversionError::InvalidVersionComponent { .. }));
            assert_eq!(error.field(), "meta.version");
        }

        let mut invalid = package();
        invalid.meta.release = 0;
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::ReleaseMustBePositive { release: 0 }
        ));
        assert_eq!(error.field(), "meta.release");

        let mut invalid_source = git_source();
        let UpstreamSpec::Git { url, .. } = &mut invalid_source else {
            unreachable!()
        };
        *url = "not a URL".to_owned();
        let mut invalid = package();
        invalid.sources.push(invalid_source);
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidUrl { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].url");

        for clone_dir in ["", ".", "..", "nested/source", "nested\\source", "source\nname"] {
            let mut invalid = package();
            invalid.sources.push(UpstreamSpec::Git {
                url: "https://example.com/source.git".to_owned(),
                git_ref: "main".to_owned(),
                clone_dir: Some(clone_dir.to_owned()),
            });
            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidMaterializationComponent { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].clone_dir");
        }

        let mut valid = package();
        valid.sources.push(UpstreamSpec::Git {
            url: "https://example.com/source.git".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: Some("custom-source.git".to_owned()),
        });
        valid.validate().unwrap();

        let mut invalid = package();
        invalid.sources.push(UpstreamSpec::Archive {
            url: "https://example.com/source.tar.xz".to_owned(),
            hash: "a".repeat(64),
            rename: None,
            strip_dirs: Some(256),
            unpack: true,
            unpack_dir: None,
        });
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::InvalidSource {
                source: UpstreamValidationError::InvalidStripDirs { .. },
                ..
            }
        ));
        assert_eq!(error.field(), "sources[0].strip_dirs");
    }

    #[test]
    fn every_authored_source_field_is_validated_before_lowering() {
        for hash in ["short".to_owned(), "A".repeat(64), format!("{}g", "a".repeat(63))] {
            let mut source = archive_source();
            let UpstreamSpec::Archive { hash: value, .. } = &mut source else {
                unreachable!()
            };
            *value = hash;
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidArchiveSha256 { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].hash");
            assert!(error.to_string().contains("64 lowercase ASCII hexadecimal"));
        }

        for rename in [
            "",
            ".",
            "..",
            "/escape",
            "nested/source",
            "nested\\source",
            "source\nname",
        ] {
            let mut source = archive_source();
            let UpstreamSpec::Archive { rename: value, .. } = &mut source else {
                unreachable!()
            };
            *value = Some(rename.to_owned());
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidMaterializationComponent { field: "rename", .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].rename");
        }

        for unpack_dir in [
            "",
            ".",
            "..",
            "/escape",
            "nested//source",
            "nested/./source",
            "nested/../escape",
            "nested\\source",
            "source\nname",
        ] {
            let mut source = archive_source();
            let UpstreamSpec::Archive { unpack_dir: value, .. } = &mut source else {
                unreachable!()
            };
            *value = Some(unpack_dir.to_owned());
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidUnpackDir { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].unpack_dir");
            assert!(error.to_string().contains("normalized, non-empty relative path"));
        }

        for field in ["strip_dirs", "unpack_dir"] {
            let mut source = archive_source();
            let UpstreamSpec::Archive {
                strip_dirs,
                unpack,
                unpack_dir,
                ..
            } = &mut source
            else {
                unreachable!()
            };
            *unpack = false;
            if field == "strip_dirs" {
                *strip_dirs = Some(1);
            } else {
                *unpack_dir = Some("source".to_owned());
            }
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::OptionRequiresUnpack { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), format!("sources[0].{field}"));
            assert!(error.to_string().contains("unless `unpack` is true"));
        }

        for git_ref in ["", "main\nother"] {
            let mut source = git_source();
            let UpstreamSpec::Git { git_ref: value, .. } = &mut source else {
                unreachable!()
            };
            *value = git_ref.to_owned();
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidGitRef { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].git_ref");
        }

        let mut archive_without_name = archive_source();
        let UpstreamSpec::Archive { url, .. } = &mut archive_without_name else {
            unreachable!()
        };
        *url = "https://example.com/".to_owned();
        let mut git_without_name = git_source();
        let UpstreamSpec::Git { url, .. } = &mut git_without_name else {
            unreachable!()
        };
        *url = "https://example.com/".to_owned();
        for source in [archive_without_name, git_without_name] {
            let mut invalid = package();
            invalid.sources.push(source);

            let error = invalid.validate().unwrap_err();
            assert!(matches!(
                error,
                PackageConversionError::InvalidSource {
                    source: UpstreamValidationError::InvalidDefaultMaterializationName { .. },
                    ..
                }
            ));
            assert_eq!(error.field(), "sources[0].url");
            assert!(error.to_string().contains("set `"));
        }
    }

    #[test]
    fn source_validation_accepts_normalized_explicit_destinations() {
        let mut archive = archive_source();
        let UpstreamSpec::Archive {
            rename,
            strip_dirs,
            unpack_dir,
            ..
        } = &mut archive
        else {
            unreachable!()
        };
        *rename = Some("source archive;literal.tar.xz".to_owned());
        *strip_dirs = Some(0);
        *unpack_dir = Some("vendor/source tree".to_owned());

        let mut git = git_source();
        let UpstreamSpec::Git { git_ref, clone_dir, .. } = &mut git else {
            unreachable!()
        };
        *git_ref = "refs/tags/v1.0.0^{}".to_owned();
        *clone_dir = Some("git source".to_owned());

        let mut valid = package();
        valid.sources = vec![archive, git];

        valid.validate().unwrap();
    }

    #[test]
    fn duplicate_source_materialization_destinations_are_rejected_before_resolution() {
        let mut archive = archive_source();
        let UpstreamSpec::Archive { rename, .. } = &mut archive else {
            unreachable!()
        };
        *rename = Some("same-source".to_owned());
        let mut git = git_source();
        let UpstreamSpec::Git { clone_dir, .. } = &mut git else {
            unreachable!()
        };
        *clone_dir = Some("same-source".to_owned());

        let mut invalid = package();
        invalid.sources = vec![archive, git];

        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::DuplicateSourceMaterialization {
                ref field,
                ref first_field,
                ref value,
            } if field == "sources[1].clone_dir"
                && first_field == "sources[0].rename"
                && value == "same-source"
        ));
        assert_eq!(error.field(), "sources[1].clone_dir");
        assert_eq!(
            error.to_string(),
            "sources[1].clone_dir: materialization destination `same-source` duplicates `sources[0].rename`"
        );
    }

    #[test]
    fn frozen_packages_require_network_content_to_be_locked_sources() {
        let mut invalid = package();
        invalid.options.networking = true;

        let error = invalid.validate().unwrap_err();

        assert!(matches!(
            error,
            PackageConversionError::FrozenBuildNetworkingUnsupported
        ));
        assert_eq!(error.field(), "options.networking");
        assert!(error.to_string().contains("locked sources"));
    }

    #[test]
    fn every_direct_relation_group_reports_its_package_field() {
        let invalid = DependencySpec::Binary(String::new());

        let mut spec = package();
        spec.check_inputs = vec![invalid.clone()];
        assert_eq!(spec.validate().unwrap_err().field(), "check_inputs[0]");

        let mut spec = package();
        spec.outputs[0].runtime_inputs = vec![invalid.clone()];
        assert_eq!(spec.validate().unwrap_err().field(), "outputs[0].runtime_inputs[0]");

        let mut spec = package();
        spec.profiles.push(ProfileSpec {
            name: "native".to_owned(),
            builder: BuilderSpec {
                required_tools: vec![invalid.clone()],
                ..BuilderSpec::default()
            },
            hooks: HooksSpec::default(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        });
        assert_eq!(
            spec.validate().unwrap_err().field(),
            "profiles[0].builder.required_tools[0]"
        );
    }

    #[test]
    fn builder_contract_rejects_duplicate_environments_and_unsupported_hooks() {
        let mut duplicate = package();
        duplicate.builder.environment = vec![BuilderEnvironmentSpec::Cargo, BuilderEnvironmentSpec::Cargo];
        let error = duplicate.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::DuplicateBuilderEnvironment { .. }
        ));
        assert_eq!(error.field(), "builder.environment[1]");

        let mut unsupported = package();
        unsupported.builder.supported_hooks.build = false;
        unsupported.hooks.pre_build = vec![shell("prepare")];
        let error = unsupported.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::UnsupportedBuilderHook { .. }));
        assert_eq!(error.field(), "hooks.pre_build");
    }

    #[test]
    fn duplicate_and_missing_outputs_are_rejected() {
        let mut missing = package();
        missing.outputs[0].name = "dev".to_owned();
        assert!(matches!(
            missing.validate(),
            Err(PackageConversionError::MissingRootOutput)
        ));

        let mut duplicate = package();
        duplicate.outputs.push(duplicate.outputs[0].clone());
        assert!(matches!(
            duplicate.validate(),
            Err(PackageConversionError::DuplicateOutput { .. })
        ));
    }

    #[test]
    fn local_output_references_are_checked_for_missing_values_and_cycles() {
        let mut missing = package();
        missing.outputs[0]
            .runtime_inputs
            .push(DependencySpec::Output(OutputRef {
                package: PackageRef {
                    name: "example".to_owned(),
                },
                output: "dev".to_owned(),
            }));
        assert!(matches!(
            missing.validate(),
            Err(PackageConversionError::MissingOutputReference { .. })
        ));

        let mut cyclic = package();
        cyclic.outputs[0].runtime_inputs.push(DependencySpec::Output(OutputRef {
            package: PackageRef {
                name: "example".to_owned(),
            },
            output: "out".to_owned(),
        }));
        assert!(matches!(
            cyclic.validate(),
            Err(PackageConversionError::OutputDependencyCycle { .. })
        ));
    }
}
