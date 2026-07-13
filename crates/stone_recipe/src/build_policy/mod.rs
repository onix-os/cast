// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed repository policy used to lower package builders into frozen plans.
//!
//! Policy text is deliberately finite: authored values may only contain
//! literals and references to known planning inputs. There is no general
//! string interpolation or action expansion at this boundary.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use thiserror::Error;

pub use self::gluon::{
    BUILD_POLICY_ABI_VERSION, BuildPolicyEvaluationError, EvaluatedBuildPolicy, EvaluatedBuildPolicyPatch,
    GLUON_BUILD_POLICY_ABI, evaluate_gluon, evaluate_gluon_with, evaluate_gluon_with_inputs, evaluate_patch_gluon,
    evaluate_patch_gluon_with, evaluate_patch_gluon_with_inputs,
};

mod gluon;
pub mod layers;

/// A value supplied explicitly by the planner when policy is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextValue {
    PackageName,
    PackageVersion,
    PackageRelease,
    SourceDir,
    InstallRoot,
    BuildRoot,
    WorkDir,
    BuilderDir,
    PgoDir,
    Jobs,
    SourceDateEpoch,
    PgoStage,
    TargetTriple,
    BuildPlatform,
    HostPlatform,
    LibSuffix,
    Prefix,
    BinDir,
    SbinDir,
    IncludeDir,
    LibDir,
    LibexecDir,
    DataDir,
    VendorDir,
    DocDir,
    InfoDir,
    LocaleDir,
    ManDir,
    SysconfDir,
    LocalStateDir,
    SharedStateDir,
    RunStateDir,
    CFlags,
    CxxFlags,
    FFlags,
    DFlags,
    RustFlags,
    ValaFlags,
    GoFlags,
    LdFlags,
    Cc,
    Cxx,
    Objc,
    Objcxx,
    Cpp,
    Objcpp,
    Objcxxcpp,
    D,
    Ar,
    Ld,
    Objcopy,
    Nm,
    Ranlib,
    Strip,
    CompilerPath,
    CcacheDir,
    SccacheDir,
    GoCacheDir,
    GoModCacheDir,
    CargoCacheDir,
    ZigCacheDir,
    RustcWrapper,
    SourcePath,
    SourceDestination,
    SourceStripComponents,
}

/// Text built without an open-ended template language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSpec {
    Literal(String),
    Context(ContextValue),
    Concat(Vec<Self>),
}

/// Filesystem installation policy. Derived paths remain structural text so a
/// package-specific directory such as `libexecdir` can name the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayoutSpec {
    pub prefix: TextSpec,
    pub bindir: TextSpec,
    pub sbindir: TextSpec,
    pub includedir: TextSpec,
    pub libdir: TextSpec,
    pub libexecdir: TextSpec,
    pub datadir: TextSpec,
    pub vendordir: TextSpec,
    pub docdir: TextSpec,
    pub infodir: TextSpec,
    pub localedir: TextSpec,
    pub mandir: TextSpec,
    pub sysconfdir: TextSpec,
    pub localstatedir: TextSpec,
    pub sharedstatedir: TextSpec,
    pub runstatedir: TextSpec,
    pub sysusersdir: TextSpec,
    pub tmpfilesdir: TextSpec,
    pub udevrulesdir: TextSpec,
    pub bash_completions_dir: TextSpec,
    pub fish_completions_dir: TextSpec,
    pub elvish_completions_dir: TextSpec,
    pub zsh_completions_dir: TextSpec,
}

/// Executable names selected by one compiler toolchain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerToolsSpec {
    pub cc: TextSpec,
    pub cxx: TextSpec,
    pub objc: TextSpec,
    pub objcxx: TextSpec,
    pub cpp: TextSpec,
    pub objcpp: TextSpec,
    pub objcxxcpp: TextSpec,
    pub d: TextSpec,
    pub ar: TextSpec,
    pub ld: TextSpec,
    pub mold_ld: TextSpec,
    pub objcopy: TextSpec,
    pub nm: TextSpec,
    pub ranlib: TextSpec,
    pub strip: TextSpec,
}

/// Both supported compiler toolchains are named fields rather than a dynamic
/// map, making an unsupported toolchain impossible to author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainsSpec {
    pub llvm: CompilerToolsSpec,
    pub gnu: CompilerToolsSpec,
}

/// Tokenized compiler flags. Each entry is one flag token after context
/// resolution; policy never relies on shell word splitting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerFlagsSpec {
    pub c: Vec<TextSpec>,
    pub cxx: Vec<TextSpec>,
    pub f: Vec<TextSpec>,
    pub d: Vec<TextSpec>,
    pub rust: Vec<TextSpec>,
    pub vala: Vec<TextSpec>,
    pub go: Vec<TextSpec>,
    pub ld: Vec<TextSpec>,
}

/// One named, toolchain-aware tuning flag from repository policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTuningFlagSpec {
    pub name: String,
    pub value: ToolchainFlagsSpec,
}

/// Flag references activated or suppressed by a tuning group or choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuningOptionSpec {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

/// One named choice within a tuning group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTuningChoiceSpec {
    pub name: String,
    pub value: TuningOptionSpec,
}

/// One tuning group and its optional default choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuningGroupSpec {
    pub base: TuningOptionSpec,
    pub default: Option<String>,
    pub choices: Vec<NamedTuningChoiceSpec>,
}

/// One named tuning group in the repository catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTuningGroupSpec {
    pub name: String,
    pub value: TuningGroupSpec,
}

/// Complete general tuning catalog selected independently of architecture and
/// PGO policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuningPolicySpec {
    pub flags: Vec<NamedTuningFlagSpec>,
    pub groups: Vec<NamedTuningGroupSpec>,
    pub default_groups: Vec<String>,
}

/// One concrete platform identity recorded in a frozen build lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPolicySpec {
    pub architecture: String,
    pub vendor: String,
    pub operating_system: String,
    pub abi: String,
}

/// Whether a target executes natively or through one explicitly named
/// compatibility mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetEmulationSpec {
    Native,
    Emul32 { host_architecture: String },
}

/// One concrete build/host/target policy. Autotools triples are kept separate
/// from lock platform components: neither can be inferred safely from the
/// other (notably for `emul32/x86_64`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPolicySpec {
    pub name: String,
    pub target_triple: String,
    pub build_triple: String,
    pub host_triple: String,
    pub lib_suffix: String,
    pub artifact_architecture: String,
    pub emulation: TargetEmulationSpec,
    pub build_platform: PlatformPolicySpec,
    pub host_platform: PlatformPolicySpec,
    pub target_platform: PlatformPolicySpec,
    pub architecture_flags: ToolchainFlagsSpec,
    pub environment: Vec<EnvironmentBindingSpec>,
}

/// A legacy target which must remain visible to policy consumers without
/// silently remaining selectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredTargetPolicySpec {
    pub name: String,
    pub reason: String,
}

/// Condition under which an environment binding is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvironmentCondition {
    Always,
    CompilerCacheEnabled,
    CompilerCacheDisabled,
}

/// One named environment value produced during planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentBindingSpec {
    pub name: String,
    pub value: TextSpec,
    pub condition: EnvironmentCondition,
}

/// A typed tool dependency owned by repository policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildToolSpec {
    Package(String),
    Binary(String),
    SystemBinary(String),
}

/// Compiler-specific packages installed into a build root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainInputPolicySpec {
    pub llvm: Vec<BuildToolSpec>,
    pub gnu: Vec<BuildToolSpec>,
}

/// Additional packages required by an emulated 32-bit target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emul32InputPolicySpec {
    pub base: Vec<BuildToolSpec>,
    pub toolchains: ToolchainInputPolicySpec,
}

/// Fixed compiler-cache inputs and guest paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCachePolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub default_path: String,
    pub compiler_path: String,
    pub ccache_dir: String,
    pub sccache_dir: String,
    pub go_cache_dir: String,
    pub go_mod_cache_dir: String,
    pub cargo_cache_dir: String,
    pub zig_cache_dir: String,
    pub rustc_wrapper: String,
}

/// Mold is a selectable repository feature, not a package-authored shell
/// fragment. Its linker executable, closure and language flags are all data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoldPolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub linker: TextSpec,
    pub flags: CompilerFlagsSpec,
}

/// Hidden inputs which form the repository-owned base build root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRootPolicySpec {
    pub base: Vec<BuildToolSpec>,
    pub toolchains: ToolchainInputPolicySpec,
    pub emul32: Emul32InputPolicySpec,
    pub compiler_cache: CompilerCachePolicySpec,
    pub mold: MoldPolicySpec,
}

/// Stable guest paths mounted into every sandbox. These paths participate in
/// policy identity instead of being ambient Boulder constants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicySpec {
    pub guest_root: String,
    pub artifacts_dir: String,
    pub build_dir: String,
    pub source_dir: String,
    pub recipe_dir: String,
    pub package_dir: String,
    pub install_dir: String,
    pub verify_dir: String,
}

/// An argv-preserving command template. Package-specific flags and binary
/// lists are added by the typed builder lowering, not hidden in these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderCommandSpec {
    pub program: TextSpec,
    pub args: Vec<TextSpec>,
    pub environment: Vec<EnvironmentBindingSpec>,
    pub working_dir: TextSpec,
}

/// Structural extraction policy for one archive source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePreparationPolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub create_directory: BuilderCommandSpec,
    pub unpack: BuilderCommandSpec,
}

/// Structural copy policy for one already-fetched git source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPreparationPolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub create_directory: BuilderCommandSpec,
    pub copy: BuilderCommandSpec,
}

/// Repository-owned source preparation commands and their locked inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePreparationPolicySpec {
    pub archive: ArchivePreparationPolicySpec,
    pub git: GitPreparationPolicySpec,
}

/// Default commands and tools for one standard builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardBuilderPolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub environment: Vec<EnvironmentBindingSpec>,
    pub setup: BuilderCommandSpec,
    pub build: BuilderCommandSpec,
    pub install: BuilderCommandSpec,
    pub check: BuilderCommandSpec,
}

/// Closed set of standard builders supported by package-v2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildersPolicySpec {
    pub cmake: StandardBuilderPolicySpec,
    pub meson: StandardBuilderPolicySpec,
    pub cargo: StandardBuilderPolicySpec,
    pub autotools: StandardBuilderPolicySpec,
}

/// Common and compiler-specific flags selected for one PGO mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainFlagsSpec {
    pub common: CompilerFlagsSpec,
    pub gnu: CompilerFlagsSpec,
    pub llvm: CompilerFlagsSpec,
}

/// A structural LLVM profile merge. Input globs are data interpreted by the
/// future PGO lowering, never by a generic shell template parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoFinishSpec {
    pub output: TextSpec,
    pub inputs: Vec<TextSpec>,
    pub copy_to: Option<TextSpec>,
    pub remove_output_first: bool,
}

/// Flags and optional workload completion for one PGO stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoStagePolicySpec {
    pub flags: ToolchainFlagsSpec,
    pub finish: Option<PgoFinishSpec>,
}

/// Complete PGO policy. `sample` augments `use_profile` when requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoPolicySpec {
    pub required_tools: Vec<BuildToolSpec>,
    pub merge_program: TextSpec,
    pub merge_args: Vec<TextSpec>,
    pub copy_program: TextSpec,
    pub remove_program: TextSpec,
    pub sample: ToolchainFlagsSpec,
    pub stage_one: PgoStagePolicySpec,
    pub stage_two: PgoStagePolicySpec,
    pub use_profile: PgoStagePolicySpec,
}

/// Concrete repository build policy returned from restricted Gluon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicySpec {
    pub vendor_id: String,
    pub build_subdir: String,
    pub layout: InstallLayoutSpec,
    pub toolchains: ToolchainsSpec,
    pub targets: Vec<TargetPolicySpec>,
    pub retired_targets: Vec<RetiredTargetPolicySpec>,
    pub sandbox: SandboxPolicySpec,
    pub build_root: BuildRootPolicySpec,
    pub sources: SourcePreparationPolicySpec,
    pub tuning: TuningPolicySpec,
    pub environment: Vec<EnvironmentBindingSpec>,
    pub builders: BuildersPolicySpec,
    pub pgo: PgoPolicySpec,
}

/// A scalar or structured policy field which is either preserved or replaced.
///
/// This is deliberately distinct from [`ArrayPatch`]: replacing an array with
/// an explicitly empty array must not collapse into keeping its current value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ValuePatch<T> {
    #[default]
    Keep,
    Set(T),
}

impl<T> ValuePatch<T> {
    /// Apply this operation to one existing value.
    pub fn apply(self, current: T) -> T {
        match self {
            Self::Keep => current,
            Self::Set(value) => value,
        }
    }
}

/// A total ordered-array operation used by repository policy layers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ArrayPatch<T> {
    #[default]
    Keep,
    Replace(Vec<T>),
    Prepend(Vec<T>),
    Append(Vec<T>),
}

impl<T> ArrayPatch<T> {
    /// Apply this operation without deduplicating, sorting, or otherwise
    /// changing authored order and multiplicity.
    pub fn apply(self, mut current: Vec<T>) -> Vec<T> {
        match self {
            Self::Keep => current,
            Self::Replace(values) => values,
            Self::Prepend(mut values) => {
                values.append(&mut current);
                values
            }
            Self::Append(values) => {
                current.extend(values);
                current
            }
        }
    }
}

/// Total top-level patch for [`BuildPolicySpec`].
///
/// Every policy field is named explicitly so adding a policy field requires a
/// deliberate patch-semantics decision at compile time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildPolicyPatchSpec {
    pub vendor_id: ValuePatch<String>,
    pub build_subdir: ValuePatch<String>,
    pub layout: ValuePatch<InstallLayoutSpec>,
    pub toolchains: ValuePatch<ToolchainsSpec>,
    pub targets: ArrayPatch<TargetPolicySpec>,
    pub retired_targets: ArrayPatch<RetiredTargetPolicySpec>,
    pub sandbox: ValuePatch<SandboxPolicySpec>,
    pub build_root: ValuePatch<BuildRootPolicySpec>,
    pub sources: ValuePatch<SourcePreparationPolicySpec>,
    pub tuning: ValuePatch<TuningPolicySpec>,
    pub environment: ArrayPatch<EnvironmentBindingSpec>,
    pub builders: ValuePatch<BuildersPolicySpec>,
    pub pgo: ValuePatch<PgoPolicySpec>,
}

impl BuildPolicyPatchSpec {
    /// Apply every patch operation to a complete policy.
    pub fn apply(self, policy: BuildPolicySpec) -> BuildPolicySpec {
        let Self {
            vendor_id,
            build_subdir,
            layout,
            toolchains,
            targets,
            retired_targets,
            sandbox,
            build_root,
            sources,
            tuning,
            environment,
            builders,
            pgo,
        } = self;
        let BuildPolicySpec {
            vendor_id: current_vendor_id,
            build_subdir: current_build_subdir,
            layout: current_layout,
            toolchains: current_toolchains,
            targets: current_targets,
            retired_targets: current_retired_targets,
            sandbox: current_sandbox,
            build_root: current_build_root,
            sources: current_sources,
            tuning: current_tuning,
            environment: current_environment,
            builders: current_builders,
            pgo: current_pgo,
        } = policy;

        BuildPolicySpec {
            vendor_id: vendor_id.apply(current_vendor_id),
            build_subdir: build_subdir.apply(current_build_subdir),
            layout: layout.apply(current_layout),
            toolchains: toolchains.apply(current_toolchains),
            targets: targets.apply(current_targets),
            retired_targets: retired_targets.apply(current_retired_targets),
            sandbox: sandbox.apply(current_sandbox),
            build_root: build_root.apply(current_build_root),
            sources: sources.apply(current_sources),
            tuning: tuning.apply(current_tuning),
            environment: environment.apply(current_environment),
            builders: builders.apply(current_builders),
            pgo: pgo.apply(current_pgo),
        }
    }

    /// Apply the patch and reject a resulting policy which violates the same
    /// invariants as a directly evaluated policy root.
    pub fn apply_validated(self, policy: BuildPolicySpec) -> Result<BuildPolicySpec, BuildPolicyConversionError> {
        let policy = self.apply(policy);
        policy.validate()?;
        Ok(policy)
    }
}

/// Semantic policy error with a stable field path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildPolicyConversionError {
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("{field}: duplicate value `{value}`")]
    Duplicate { field: String, value: String },
    #[error("{field}: PGO finish must declare at least one input")]
    EmptyPgoInputs { field: String },
    #[error("{field}: unknown reference `{value}`")]
    UnknownReference { field: String, value: String },
    #[error("{field}: default choice `{value}` does not exist")]
    InvalidDefault { field: String, value: String },
    #[error("{field}: flag `{value}` cannot be both enabled and disabled")]
    ConflictingTuningFlag { field: String, value: String },
    #[error("{field}: guest path `{value}` must be absolute and normalized")]
    InvalidGuestPath { field: String, value: String },
    #[error("{field}: target name `{value}` must be a normalized safe relative path")]
    InvalidTargetName { field: String, value: String },
    #[error("{field}: guest path `{value}` is outside `{guest_root}`")]
    GuestPathOutsideRoot {
        field: String,
        value: String,
        guest_root: String,
    },
    #[error("{field}: platform component must be explicit, found `{value}`")]
    InvalidPlatformComponent { field: String, value: String },
    #[error("{field}: architecture `{value}` does not match `{expected}`")]
    ArchitectureMismatch {
        field: String,
        value: String,
        expected: String,
    },
}

impl BuildPolicySpec {
    /// Validate invariants needed before the policy can participate in a
    /// derivation fingerprint.
    pub fn validate(&self) -> Result<(), BuildPolicyConversionError> {
        require_string("vendor_id", &self.vendor_id)?;
        require_string("build_subdir", &self.build_subdir)?;
        validate_layout(&self.layout)?;
        validate_tools_record("toolchains.llvm", &self.toolchains.llvm)?;
        validate_tools_record("toolchains.gnu", &self.toolchains.gnu)?;

        if self.targets.is_empty() {
            return Err(BuildPolicyConversionError::Empty {
                field: "targets".to_owned(),
            });
        }
        let mut targets = BTreeSet::new();
        for (index, target) in self.targets.iter().enumerate() {
            let field = format!("targets[{index}]");
            validate_target_name(&format!("{field}.name"), &target.name)?;
            require_string(&format!("{field}.target_triple"), &target.target_triple)?;
            require_string(&format!("{field}.build_triple"), &target.build_triple)?;
            require_string(&format!("{field}.host_triple"), &target.host_triple)?;
            require_string(&format!("{field}.artifact_architecture"), &target.artifact_architecture)?;
            if !targets.insert(target.name.as_str()) {
                return Err(BuildPolicyConversionError::Duplicate {
                    field: "targets".to_owned(),
                    value: target.name.clone(),
                });
            }
            validate_target(&field, target)?;
        }
        for (index, target) in self.retired_targets.iter().enumerate() {
            let field = format!("retired_targets[{index}]");
            validate_target_name(&format!("{field}.name"), &target.name)?;
            require_string(&format!("{field}.reason"), &target.reason)?;
            if !targets.insert(target.name.as_str()) {
                return Err(BuildPolicyConversionError::Duplicate {
                    field: "targets".to_owned(),
                    value: target.name.clone(),
                });
            }
        }

        validate_sandbox(&self.sandbox)?;
        validate_build_root(&self.build_root, &self.sandbox)?;
        validate_sources(&self.sources)?;
        validate_tuning(&self.tuning)?;

        validate_bindings("environment", &self.environment)?;
        for (name, builder) in [
            ("cmake", &self.builders.cmake),
            ("meson", &self.builders.meson),
            ("cargo", &self.builders.cargo),
            ("autotools", &self.builders.autotools),
        ] {
            validate_builder(&format!("builders.{name}"), builder)?;
        }

        validate_tools("pgo.required_tools", &self.pgo.required_tools)?;
        require_text("pgo.merge_program", &self.pgo.merge_program)?;
        if self.pgo.merge_args.is_empty() {
            return Err(BuildPolicyConversionError::Empty {
                field: "pgo.merge_args".to_owned(),
            });
        }
        for (index, argument) in self.pgo.merge_args.iter().enumerate() {
            require_text(&format!("pgo.merge_args[{index}]"), argument)?;
        }
        require_text("pgo.copy_program", &self.pgo.copy_program)?;
        require_text("pgo.remove_program", &self.pgo.remove_program)?;
        validate_pgo_stage("pgo.stage_one", &self.pgo.stage_one)?;
        validate_pgo_stage("pgo.stage_two", &self.pgo.stage_two)?;
        validate_pgo_stage("pgo.use_profile", &self.pgo.use_profile)
    }
}

fn validate_target_name(field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
    let path = Path::new(value);
    let normalized = !path.is_absolute()
        && value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if normalized {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::InvalidTargetName {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn validate_target(field: &str, target: &TargetPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_platform(&format!("{field}.build_platform"), &target.build_platform)?;
    validate_platform(&format!("{field}.host_platform"), &target.host_platform)?;
    validate_platform(&format!("{field}.target_platform"), &target.target_platform)?;
    validate_toolchain_flags(&format!("{field}.architecture_flags"), &target.architecture_flags)?;
    validate_bindings(&format!("{field}.environment"), &target.environment)?;

    require_architecture(
        &format!("{field}.host_platform.architecture"),
        &target.host_platform.architecture,
        &target.artifact_architecture,
    )?;
    require_architecture(
        &format!("{field}.target_platform.architecture"),
        &target.target_platform.architecture,
        &target.artifact_architecture,
    )?;
    match &target.emulation {
        TargetEmulationSpec::Native => require_architecture(
            &format!("{field}.build_platform.architecture"),
            &target.build_platform.architecture,
            &target.artifact_architecture,
        ),
        TargetEmulationSpec::Emul32 { host_architecture } => {
            require_string(&format!("{field}.emulation.host_architecture"), host_architecture)?;
            require_architecture(
                &format!("{field}.build_platform.architecture"),
                &target.build_platform.architecture,
                host_architecture,
            )
        }
    }
}

fn validate_platform(field: &str, platform: &PlatformPolicySpec) -> Result<(), BuildPolicyConversionError> {
    for (name, value) in [
        ("architecture", &platform.architecture),
        ("vendor", &platform.vendor),
        ("operating_system", &platform.operating_system),
        ("abi", &platform.abi),
    ] {
        let field = format!("{field}.{name}");
        require_string(&field, value)?;
        if value == "unknown" {
            return Err(BuildPolicyConversionError::InvalidPlatformComponent {
                field,
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn require_architecture(field: &str, value: &str, expected: &str) -> Result<(), BuildPolicyConversionError> {
    if value == expected {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::ArchitectureMismatch {
            field: field.to_owned(),
            value: value.to_owned(),
            expected: expected.to_owned(),
        })
    }
}

fn validate_sandbox(sandbox: &SandboxPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_guest_path("sandbox.guest_root", &sandbox.guest_root)?;
    let mut paths = BTreeSet::new();
    for (name, value) in [
        ("artifacts_dir", &sandbox.artifacts_dir),
        ("build_dir", &sandbox.build_dir),
        ("source_dir", &sandbox.source_dir),
        ("recipe_dir", &sandbox.recipe_dir),
        ("package_dir", &sandbox.package_dir),
        ("install_dir", &sandbox.install_dir),
        ("verify_dir", &sandbox.verify_dir),
    ] {
        let field = format!("sandbox.{name}");
        validate_guest_child(&field, value, &sandbox.guest_root)?;
        if !paths.insert(value.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "sandbox".to_owned(),
                value: value.clone(),
            });
        }
    }
    if !Path::new(&sandbox.package_dir).starts_with(&sandbox.recipe_dir) {
        return Err(BuildPolicyConversionError::GuestPathOutsideRoot {
            field: "sandbox.package_dir".to_owned(),
            value: sandbox.package_dir.clone(),
            guest_root: sandbox.recipe_dir.clone(),
        });
    }
    Ok(())
}

fn validate_build_root(
    build_root: &BuildRootPolicySpec,
    sandbox: &SandboxPolicySpec,
) -> Result<(), BuildPolicyConversionError> {
    validate_tools("build_root.base", &build_root.base)?;
    validate_toolchain_inputs("build_root.toolchains", &build_root.toolchains)?;
    validate_tools("build_root.emul32.base", &build_root.emul32.base)?;
    validate_toolchain_inputs("build_root.emul32.toolchains", &build_root.emul32.toolchains)?;

    let cache = &build_root.compiler_cache;
    validate_tools("build_root.compiler_cache.required_tools", &cache.required_tools)?;
    require_string("build_root.compiler_cache.default_path", &cache.default_path)?;
    require_string("build_root.compiler_cache.compiler_path", &cache.compiler_path)?;
    for (name, value) in [
        ("ccache_dir", &cache.ccache_dir),
        ("sccache_dir", &cache.sccache_dir),
        ("go_cache_dir", &cache.go_cache_dir),
        ("go_mod_cache_dir", &cache.go_mod_cache_dir),
        ("cargo_cache_dir", &cache.cargo_cache_dir),
        ("zig_cache_dir", &cache.zig_cache_dir),
    ] {
        validate_guest_child(&format!("build_root.compiler_cache.{name}"), value, &sandbox.guest_root)?;
    }
    validate_guest_path("build_root.compiler_cache.rustc_wrapper", &cache.rustc_wrapper)?;

    validate_tools("build_root.mold.required_tools", &build_root.mold.required_tools)?;
    require_text("build_root.mold.linker", &build_root.mold.linker)?;
    validate_compiler_flags("build_root.mold.flags", &build_root.mold.flags)
}

fn validate_toolchain_inputs(field: &str, inputs: &ToolchainInputPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_tools(&format!("{field}.llvm"), &inputs.llvm)?;
    validate_tools(&format!("{field}.gnu"), &inputs.gnu)
}

fn validate_sources(sources: &SourcePreparationPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_tools("sources.archive.required_tools", &sources.archive.required_tools)?;
    validate_command("sources.archive.create_directory", &sources.archive.create_directory)?;
    validate_command("sources.archive.unpack", &sources.archive.unpack)?;

    validate_tools("sources.git.required_tools", &sources.git.required_tools)?;
    validate_command("sources.git.create_directory", &sources.git.create_directory)?;
    validate_command("sources.git.copy", &sources.git.copy)
}

fn validate_guest_child(field: &str, value: &str, guest_root: &str) -> Result<(), BuildPolicyConversionError> {
    validate_guest_path(field, value)?;
    if Path::new(value).starts_with(guest_root) && value != guest_root {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::GuestPathOutsideRoot {
            field: field.to_owned(),
            value: value.to_owned(),
            guest_root: guest_root.to_owned(),
        })
    }
}

fn validate_guest_path(field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
    let path = Path::new(value);
    let normalized = path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
    if normalized {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::InvalidGuestPath {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn validate_tuning(tuning: &TuningPolicySpec) -> Result<(), BuildPolicyConversionError> {
    let mut flag_names = BTreeSet::new();
    for (index, flag) in tuning.flags.iter().enumerate() {
        require_string(&format!("tuning.flags[{index}].name"), &flag.name)?;
        if !flag_names.insert(flag.name.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.flags".to_owned(),
                value: flag.name.clone(),
            });
        }
        validate_toolchain_flags(&format!("tuning.flags[{index}].value"), &flag.value)?;
    }

    let mut group_names = BTreeSet::new();
    for (index, group) in tuning.groups.iter().enumerate() {
        let field = format!("tuning.groups[{index}]");
        require_string(&format!("{field}.name"), &group.name)?;
        if !group_names.insert(group.name.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.groups".to_owned(),
                value: group.name.clone(),
            });
        }
        validate_tuning_option(&format!("{field}.value.base"), &group.value.base, &flag_names)?;

        let mut choice_names = BTreeSet::new();
        for (choice_index, choice) in group.value.choices.iter().enumerate() {
            let choice_field = format!("{field}.value.choices[{choice_index}]");
            require_string(&format!("{choice_field}.name"), &choice.name)?;
            if !choice_names.insert(choice.name.as_str()) {
                return Err(BuildPolicyConversionError::Duplicate {
                    field: format!("{field}.value.choices"),
                    value: choice.name.clone(),
                });
            }
            validate_tuning_option(&format!("{choice_field}.value"), &choice.value, &flag_names)?;
        }

        if let Some(default) = &group.value.default {
            require_string(&format!("{field}.value.default"), default)?;
            if !choice_names.contains(default.as_str()) {
                return Err(BuildPolicyConversionError::InvalidDefault {
                    field: format!("{field}.value.default"),
                    value: default.clone(),
                });
            }
        }
    }

    let mut default_groups = BTreeSet::new();
    for (index, group) in tuning.default_groups.iter().enumerate() {
        let field = format!("tuning.default_groups[{index}]");
        require_string(&field, group)?;
        if !default_groups.insert(group.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.default_groups".to_owned(),
                value: group.clone(),
            });
        }
        if !group_names.contains(group.as_str()) {
            return Err(BuildPolicyConversionError::UnknownReference {
                field,
                value: group.clone(),
            });
        }
    }

    Ok(())
}

fn validate_tuning_option(
    field: &str,
    option: &TuningOptionSpec,
    flag_names: &BTreeSet<&str>,
) -> Result<(), BuildPolicyConversionError> {
    let mut enabled = BTreeSet::new();
    for (index, flag) in option.enabled.iter().enumerate() {
        validate_tuning_flag_reference(&format!("{field}.enabled[{index}]"), flag, flag_names)?;
        if !enabled.insert(flag.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: format!("{field}.enabled"),
                value: flag.clone(),
            });
        }
    }

    let mut disabled = BTreeSet::new();
    for (index, flag) in option.disabled.iter().enumerate() {
        validate_tuning_flag_reference(&format!("{field}.disabled[{index}]"), flag, flag_names)?;
        if !disabled.insert(flag.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: format!("{field}.disabled"),
                value: flag.clone(),
            });
        }
        if enabled.contains(flag.as_str()) {
            return Err(BuildPolicyConversionError::ConflictingTuningFlag {
                field: field.to_owned(),
                value: flag.clone(),
            });
        }
    }
    Ok(())
}

fn validate_tuning_flag_reference(
    field: &str,
    flag: &str,
    flag_names: &BTreeSet<&str>,
) -> Result<(), BuildPolicyConversionError> {
    require_string(field, flag)?;
    if flag_names.contains(flag) {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::UnknownReference {
            field: field.to_owned(),
            value: flag.to_owned(),
        })
    }
}

fn validate_toolchain_flags(field: &str, flags: &ToolchainFlagsSpec) -> Result<(), BuildPolicyConversionError> {
    validate_compiler_flags(&format!("{field}.common"), &flags.common)?;
    validate_compiler_flags(&format!("{field}.gnu"), &flags.gnu)?;
    validate_compiler_flags(&format!("{field}.llvm"), &flags.llvm)
}

fn validate_compiler_flags(field: &str, flags: &CompilerFlagsSpec) -> Result<(), BuildPolicyConversionError> {
    for (language, values) in [
        ("c", &flags.c),
        ("cxx", &flags.cxx),
        ("f", &flags.f),
        ("d", &flags.d),
        ("rust", &flags.rust),
        ("vala", &flags.vala),
        ("go", &flags.go),
        ("ld", &flags.ld),
    ] {
        for (index, value) in values.iter().enumerate() {
            require_text(&format!("{field}.{language}[{index}]"), value)?;
        }
    }
    Ok(())
}

fn validate_layout(layout: &InstallLayoutSpec) -> Result<(), BuildPolicyConversionError> {
    for (name, value) in [
        ("prefix", &layout.prefix),
        ("bindir", &layout.bindir),
        ("sbindir", &layout.sbindir),
        ("includedir", &layout.includedir),
        ("libdir", &layout.libdir),
        ("libexecdir", &layout.libexecdir),
        ("datadir", &layout.datadir),
        ("vendordir", &layout.vendordir),
        ("docdir", &layout.docdir),
        ("infodir", &layout.infodir),
        ("localedir", &layout.localedir),
        ("mandir", &layout.mandir),
        ("sysconfdir", &layout.sysconfdir),
        ("localstatedir", &layout.localstatedir),
        ("sharedstatedir", &layout.sharedstatedir),
        ("runstatedir", &layout.runstatedir),
        ("sysusersdir", &layout.sysusersdir),
        ("tmpfilesdir", &layout.tmpfilesdir),
        ("udevrulesdir", &layout.udevrulesdir),
        ("bash_completions_dir", &layout.bash_completions_dir),
        ("fish_completions_dir", &layout.fish_completions_dir),
        ("elvish_completions_dir", &layout.elvish_completions_dir),
        ("zsh_completions_dir", &layout.zsh_completions_dir),
    ] {
        require_text(&format!("layout.{name}"), value)?;
    }
    Ok(())
}

fn validate_tools_record(field: &str, tools: &CompilerToolsSpec) -> Result<(), BuildPolicyConversionError> {
    for (name, value) in [
        ("cc", &tools.cc),
        ("cxx", &tools.cxx),
        ("objc", &tools.objc),
        ("objcxx", &tools.objcxx),
        ("cpp", &tools.cpp),
        ("objcpp", &tools.objcpp),
        ("objcxxcpp", &tools.objcxxcpp),
        ("d", &tools.d),
        ("ar", &tools.ar),
        ("ld", &tools.ld),
        ("mold_ld", &tools.mold_ld),
        ("objcopy", &tools.objcopy),
        ("nm", &tools.nm),
        ("ranlib", &tools.ranlib),
        ("strip", &tools.strip),
    ] {
        require_text(&format!("{field}.{name}"), value)?;
    }
    Ok(())
}

fn validate_builder(field: &str, builder: &StandardBuilderPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_tools(&format!("{field}.required_tools"), &builder.required_tools)?;
    validate_bindings(&format!("{field}.environment"), &builder.environment)?;
    for (name, command) in [
        ("setup", &builder.setup),
        ("build", &builder.build),
        ("install", &builder.install),
        ("check", &builder.check),
    ] {
        validate_command(&format!("{field}.{name}"), command)?;
    }
    Ok(())
}

fn validate_command(field: &str, command: &BuilderCommandSpec) -> Result<(), BuildPolicyConversionError> {
    require_text(&format!("{field}.program"), &command.program)?;
    require_text(&format!("{field}.working_dir"), &command.working_dir)?;
    for (index, argument) in command.args.iter().enumerate() {
        require_text(&format!("{field}.args[{index}]"), argument)?;
    }
    validate_bindings(&format!("{field}.environment"), &command.environment)
}

fn validate_bindings(field: &str, bindings: &[EnvironmentBindingSpec]) -> Result<(), BuildPolicyConversionError> {
    let mut names = BTreeSet::new();
    for (index, binding) in bindings.iter().enumerate() {
        require_string(&format!("{field}[{index}].name"), &binding.name)?;
        require_text(&format!("{field}[{index}].value"), &binding.value)?;
        if !names.insert((binding.condition, binding.name.as_str())) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: field.to_owned(),
                value: binding.name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_tools(field: &str, tools: &[BuildToolSpec]) -> Result<(), BuildPolicyConversionError> {
    let mut values = BTreeSet::new();
    for (index, tool) in tools.iter().enumerate() {
        let target = match tool {
            BuildToolSpec::Package(target) | BuildToolSpec::Binary(target) | BuildToolSpec::SystemBinary(target) => {
                target
            }
        };
        require_string(&format!("{field}[{index}]"), target)?;
        if !values.insert(tool) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: field.to_owned(),
                value: target.clone(),
            });
        }
    }
    Ok(())
}

fn validate_pgo_stage(field: &str, stage: &PgoStagePolicySpec) -> Result<(), BuildPolicyConversionError> {
    let Some(finish) = &stage.finish else {
        return Ok(());
    };
    require_text(&format!("{field}.finish.output"), &finish.output)?;
    if finish.inputs.is_empty() {
        return Err(BuildPolicyConversionError::EmptyPgoInputs {
            field: format!("{field}.finish.inputs"),
        });
    }
    for (index, input) in finish.inputs.iter().enumerate() {
        require_text(&format!("{field}.finish.inputs[{index}]"), input)?;
    }
    if let Some(copy_to) = &finish.copy_to {
        require_text(&format!("{field}.finish.copy_to"), copy_to)?;
    }
    Ok(())
}

fn require_string(field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
    if value.is_empty() {
        Err(BuildPolicyConversionError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn require_text(field: &str, value: &TextSpec) -> Result<(), BuildPolicyConversionError> {
    let empty = match value {
        TextSpec::Literal(value) => value.is_empty(),
        TextSpec::Context(_) => false,
        TextSpec::Concat(parts) => parts.is_empty() || parts.iter().all(text_is_statically_empty),
    };
    if empty {
        Err(BuildPolicyConversionError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn text_is_statically_empty(value: &TextSpec) -> bool {
    match value {
        TextSpec::Literal(value) => value.is_empty(),
        TextSpec::Context(_) => false,
        TextSpec::Concat(parts) => parts.iter().all(text_is_statically_empty),
    }
}
