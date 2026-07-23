//! Typed package declarations for the `cast.package.v3` Gluon ABI.
//!
//! A package factory is evaluated completely inside Gluon and produces one
//! concrete [`PackageSpec`]. This module deliberately contains values only:
//! Rust never receives or retains a Gluon closure or a second recipe model.

use crate::{NamedTuningSpec, OptionsSpec, PathSpec, UpstreamSpec};
use stone::relation::{Dependency, Kind as RelationKind, ParseError, Provider};

pub use self::gluon::{
    EvaluatedPackage, GluonPackageEvaluator, GLUON_AUTOTOOLS_BUILDER_ABI, GLUON_CARGO_BUILDER_ABI, GLUON_CMAKE_BUILDER_ABI,
    GLUON_MESON_BUILDER_ABI, GLUON_PACKAGE_ABI, PACKAGE_ABI_VERSION, PackageEvaluationError, evaluate_gluon,
    evaluate_gluon_with, evaluate_gluon_with_inputs,
};

mod gluon;
mod validation;

pub(crate) use validation::valid_package_name;
pub use validation::{
    DependencyKind, DependencyRole, PackageConversionError, PackageValidationLimits,
};

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
    /// Execute one exact native executable below the current build working directory.
    ///
    /// Unlike [`Self::Run`], this Linux ELF image is produced or materialized
    /// inside the isolated build tree and therefore has no external
    /// package-provider capability. The authored path is normalized and
    /// relative; freezing binds it to an absolute path below the phase working
    /// directory. Scripts must use [`Self::Shell`]; descriptor-executed
    /// shebangs fail closed.
    RunBuilt {
        program: BuiltProgramSpec,
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
/// `<pname>-<name>` subpackages until Mason and Stone consume output names
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
/// model whenever Mason crosses into package resolution or metadata.
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

/// One native Linux ELF executable produced beneath the phase working directory.
///
/// Scripts use [`StepSpec::Shell`]; a descriptor-executed shebang is rejected
/// without falling back to its mutable public pathname.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltProgramSpec {
    pub path: String,
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

impl PackageSpec {
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
    use crate::spec::UpstreamValidationError;

    use super::*;

    include!("validation/tests/fixtures.rs");
    include!("validation/tests/budgets_relations_profiles.rs");
    include!("validation/tests/dependency_roles.rs");
    include!("validation/tests/metadata_selectors.rs");
    include!("validation/tests/path_rules.rs");
    include!("validation/tests/source_materialization.rs");
    include!("validation/tests/structural_relations.rs");
}
