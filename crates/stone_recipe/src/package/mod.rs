// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed package declarations for the `boulder.package.v2` Gluon ABI.
//!
//! A package factory is evaluated completely inside Gluon and produces one
//! concrete [`PackageSpec`]. This module deliberately contains values only:
//! Rust never receives or retains a Gluon closure or a second recipe model.

use std::collections::{BTreeMap, BTreeSet};

use stone::relation::{Dependency, Kind as RelationKind, ParseError, Provider};
use thiserror::Error;
use url::Url;

use crate::{KeyValueSpec, OptionsSpec, PathSpec, TuningSpec, UpstreamSpec};

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
    pub tuning: Vec<KeyValueSpec<TuningSpec>>,
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
/// Standard builders use dedicated variants. [`Self::Shell`] is the only
/// escape hatch for commands which cannot be expressed structurally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepSpec {
    Shell { script: String },
    CMakeConfigure { flags: Vec<String> },
    CMakeBuild,
    CMakeInstall,
    CMakeTest,
    MesonSetup { flags: Vec<String> },
    MesonBuild,
    MesonInstall,
    MesonTest,
    CargoEnvironment,
    CargoFetch,
    CargoBuild { features: Vec<String> },
    CargoInstall { binaries: Vec<String> },
    CargoTest { features: Vec<String> },
    AutotoolsConfigure { flags: Vec<String> },
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
    pub environment: PhaseSpec,
}

/// Explicit custom phase definitions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptsSpec {
    pub setup: PhaseSpec,
    pub build: PhaseSpec,
    pub install: PhaseSpec,
    pub check: PhaseSpec,
    pub workload: PhaseSpec,
    pub environment: PhaseSpec,
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
    pub environment: Vec<StepSpec>,
}

/// A structural build-system selection.
///
/// Standard variants select repository-owned typed builder policy, which owns
/// their commands, environment, and tools. [`Self::Custom`] is the explicit
/// shell escape hatch and therefore carries its package-authored tools here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuilderSpec {
    CMake {
        flags: Vec<String>,
        run_tests: bool,
    },
    Meson {
        flags: Vec<String>,
        run_tests: bool,
    },
    Cargo {
        features: Vec<String>,
        binaries: Vec<String>,
        run_tests: bool,
    },
    Autotools {
        flags: Vec<String>,
        run_tests: bool,
    },
    Custom {
        scripts: ScriptsSpec,
        required_tools: Vec<DependencySpec>,
    },
}

impl Default for BuilderSpec {
    fn default() -> Self {
        Self::Custom {
            scripts: ScriptsSpec::default(),
            required_tools: Vec::new(),
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

/// Failure to validate a concrete package-v2 declaration.
#[derive(Debug, Error)]
pub enum PackageConversionError {
    #[error("meta.version: version must start with an integer (found `{version}`)")]
    VersionMustStartWithDigit { version: String },
    #[error("meta.release: release must be greater than zero (found `{release}`)")]
    ReleaseMustBePositive { release: i64 },
    #[error("{field}: invalid URL `{value}`")]
    InvalidUrl {
        field: String,
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("{field}: `{value}` is outside the valid {expected} range")]
    IntegerOutOfRange {
        field: String,
        value: i64,
        expected: &'static str,
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
    #[error("{field}: output `{output}` does not exist in package `{package}`")]
    MissingOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: package output dependency cycle: {cycle}")]
    OutputDependencyCycle { field: String, cycle: String },
}

impl PackageConversionError {
    pub fn field(&self) -> &str {
        match self {
            Self::VersionMustStartWithDigit { .. } => "meta.version",
            Self::ReleaseMustBePositive { .. } => "meta.release",
            Self::InvalidUrl { field, .. }
            | Self::IntegerOutOfRange { field, .. }
            | Self::InvalidDependency { field, .. }
            | Self::InvalidProvider { field, .. }
            | Self::MissingOutputReference { field, .. }
            | Self::OutputDependencyCycle { field, .. } => field,
            Self::MissingRootOutput => "outputs",
            Self::DuplicateOutput { .. } | Self::InvalidOutputName { .. } => "outputs",
        }
    }
}

impl PackageSpec {
    /// Validate the concrete package declaration without lowering it through
    /// the transitional recipe model.
    pub fn validate(&self) -> Result<(), PackageConversionError> {
        if !self
            .meta
            .version
            .starts_with(|character: char| character.is_ascii_digit())
        {
            return Err(PackageConversionError::VersionMustStartWithDigit {
                version: self.meta.version.clone(),
            });
        }
        if self.meta.release <= 0 {
            return Err(PackageConversionError::ReleaseMustBePositive {
                release: self.meta.release,
            });
        }

        for (index, source) in self.sources.iter().enumerate() {
            let (url, strip_dirs) = match source {
                UpstreamSpec::Archive { url, strip_dirs, .. } => (url, *strip_dirs),
                UpstreamSpec::Git { url, .. } => (url, None),
            };
            Url::parse(url).map_err(|source| PackageConversionError::InvalidUrl {
                field: format!("sources[{index}].url"),
                value: url.clone(),
                source,
            })?;
            if let Some(value) = strip_dirs {
                u8::try_from(value).map_err(|_| PackageConversionError::IntegerOutOfRange {
                    field: format!("sources[{index}].strip_dirs"),
                    value,
                    expected: "0..=255 integer",
                })?;
            }
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

    /// Select target-native build inputs for one optional profile. Standard
    /// builder tools live in repository policy; custom tools remain available
    /// through [`BuilderSpec::authored_required_tools`].
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
        self.validate_dependency_list(
            self.builder.authored_required_tools(),
            "builder.required_tools",
            &outputs,
            false,
        )?;

        for (index, profile) in self.profiles.iter().enumerate() {
            let parent = format!("profiles[{index}]");
            self.validate_dependency_list(
                profile.builder.authored_required_tools(),
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
            let parsed = if provider {
                dependency.provider().map(|_| ())
            } else {
                dependency.dependency().map(|_| ())
            };
            parsed.map_err(|source| {
                if provider {
                    PackageConversionError::InvalidProvider {
                        field: field.clone(),
                        source,
                    }
                } else {
                    PackageConversionError::InvalidDependency {
                        field: field.clone(),
                        source,
                    }
                }
            })?;
            if let Some((package, output)) = dependency.package_and_output()
                && package == self.meta.pname
                && !outputs.contains_key(output)
            {
                return Err(PackageConversionError::MissingOutputReference {
                    field,
                    package: package.to_owned(),
                    output: output.to_owned(),
                });
            }
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

fn valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_'))
}

impl BuilderSpec {
    /// Package-authored tools for the explicit custom-builder escape hatch.
    ///
    /// Standard builder tools are repository policy and deliberately have no
    /// duplicate Rust defaults in the package domain.
    pub fn authored_required_tools(&self) -> &[DependencySpec] {
        match self {
            Self::Custom { required_tools, .. } => required_tools,
            Self::CMake { .. } | Self::Meson { .. } | Self::Cargo { .. } | Self::Autotools { .. } => &[],
        }
    }

    pub fn phases(&self, hooks: &HooksSpec) -> PhasesSpec {
        let phases = match self.clone() {
            Self::CMake { flags, run_tests } => PhasesSpec {
                setup: PhaseSpec::new([StepSpec::CMakeConfigure { flags }]),
                build: PhaseSpec::new([StepSpec::CMakeBuild]),
                install: PhaseSpec::new([StepSpec::CMakeInstall]),
                check: PhaseSpec::new(run_tests.then_some(StepSpec::CMakeTest)),
                ..PhasesSpec::default()
            },
            Self::Meson { flags, run_tests } => PhasesSpec {
                setup: PhaseSpec::new([StepSpec::MesonSetup { flags }]),
                build: PhaseSpec::new([StepSpec::MesonBuild]),
                install: PhaseSpec::new([StepSpec::MesonInstall]),
                check: PhaseSpec::new(run_tests.then_some(StepSpec::MesonTest)),
                ..PhasesSpec::default()
            },
            Self::Cargo {
                features,
                binaries,
                run_tests,
            } => PhasesSpec {
                build: PhaseSpec::new([StepSpec::CargoBuild {
                    features: features.clone(),
                }]),
                install: PhaseSpec::new([StepSpec::CargoInstall { binaries }]),
                check: PhaseSpec::new(run_tests.then_some(StepSpec::CargoTest { features })),
                environment: PhaseSpec::new([StepSpec::CargoEnvironment]),
                ..PhasesSpec::default()
            },
            Self::Autotools { flags, run_tests } => PhasesSpec {
                setup: PhaseSpec::new([StepSpec::AutotoolsConfigure { flags }]),
                build: PhaseSpec::new([StepSpec::AutotoolsBuild]),
                install: PhaseSpec::new([StepSpec::AutotoolsInstall]),
                check: PhaseSpec::new(run_tests.then_some(StepSpec::AutotoolsTest)),
                ..PhasesSpec::default()
            },
            Self::Custom { scripts, .. } => scripts.into(),
        };
        hooks.clone().apply(phases)
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
            environment: phase_with_hooks(Vec::new(), phases.environment, self.environment),
        }
    }
}

fn phase_with_hooks(pre: Vec<StepSpec>, body: PhaseSpec, post: Vec<StepSpec>) -> PhaseSpec {
    PhaseSpec::new(pre.into_iter().chain(body.steps).chain(post))
}

impl From<ScriptsSpec> for PhasesSpec {
    fn from(scripts: ScriptsSpec) -> Self {
        Self {
            setup: scripts.setup,
            build: scripts.build,
            install: scripts.install,
            check: scripts.check,
            workload: scripts.workload,
            environment: scripts.environment,
        }
    }
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
    fn base_and_selected_profile_semantics_stay_structural() {
        let mut package = package();
        package.builder = BuilderSpec::CMake {
            flags: vec!["-DBASE=ON".to_owned()],
            run_tests: false,
        };
        package.native_build_inputs = vec![dependency("base-native")];
        package.build_inputs = vec![dependency("base-build")];
        package.check_inputs = vec![dependency("base-check")];
        package.profiles.push(ProfileSpec {
            name: "emul32/x86_64".to_owned(),
            builder: BuilderSpec::Cargo {
                features: vec!["profile".to_owned()],
                binaries: vec!["example".to_owned()],
                run_tests: true,
            },
            hooks: HooksSpec {
                pre_build: vec![StepSpec::Shell {
                    script: "prepare-profile".to_owned(),
                }],
                ..HooksSpec::default()
            },
            native_build_inputs: vec![dependency("profile-native")],
            build_inputs: vec![dependency("profile-build")],
            check_inputs: vec![dependency("profile-check")],
        });

        assert!(matches!(package.builder_for_profile(None), BuilderSpec::CMake { .. }));
        assert!(package.builder_for_profile(None).authored_required_tools().is_empty());
        assert!(matches!(
            package.builder_for_profile(Some("emul32/x86_64")),
            BuilderSpec::Cargo { .. }
        ));
        assert!(
            package
                .builder_for_profile(Some("emul32/x86_64"))
                .authored_required_tools()
                .is_empty()
        );
        assert_eq!(
            package.phases_for_profile(Some("emul32/x86_64")).build.steps,
            [
                StepSpec::Shell {
                    script: "prepare-profile".to_owned()
                },
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
        assert!(matches!(
            package.builder_for_profile(Some("missing")),
            BuilderSpec::CMake { .. }
        ));
    }

    #[test]
    fn direct_metadata_and_source_validation_keep_package_field_paths() {
        let mut invalid = package();
        invalid.meta.version = "v1.0".to_owned();
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::VersionMustStartWithDigit { .. }
        ));
        assert_eq!(error.field(), "meta.version");

        let mut invalid = package();
        invalid.meta.release = 0;
        let error = invalid.validate().unwrap_err();
        assert!(matches!(
            error,
            PackageConversionError::ReleaseMustBePositive { release: 0 }
        ));
        assert_eq!(error.field(), "meta.release");

        let mut invalid = package();
        invalid.sources.push(UpstreamSpec::Git {
            url: "not a URL".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: None,
        });
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::InvalidUrl { .. }));
        assert_eq!(error.field(), "sources[0].url");

        let mut invalid = package();
        invalid.sources.push(UpstreamSpec::Archive {
            url: "https://example.com/source.tar.xz".to_owned(),
            hash: "hash".to_owned(),
            rename: None,
            strip_dirs: Some(256),
            unpack: true,
            unpack_dir: None,
        });
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, PackageConversionError::IntegerOutOfRange { .. }));
        assert_eq!(error.field(), "sources[0].strip_dirs");
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
            builder: BuilderSpec::Custom {
                scripts: ScriptsSpec::default(),
                required_tools: vec![invalid.clone()],
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
