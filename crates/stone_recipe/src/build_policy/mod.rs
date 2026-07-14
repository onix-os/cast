//! Typed repository policy used to lower package builders into frozen plans.
//!
//! Policy text is deliberately finite: authored values may only contain
//! literals and references to known planning inputs. There is no general
//! string interpolation or action expansion at this boundary.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use stone::relation::{Dependency, Kind as RelationKind, ParseError};
use thiserror::Error;

pub use self::gluon::{
    BUILD_POLICY_ABI_VERSION, BuildPolicyEvaluationError, EvaluatedBuildPolicy, EvaluatedBuildPolicyPatch,
    GLUON_BUILD_POLICY_ABI, evaluate_gluon, evaluate_gluon_with, evaluate_gluon_with_inputs, evaluate_patch_gluon,
    evaluate_patch_gluon_with, evaluate_patch_gluon_with_inputs,
};

mod gluon;
pub mod layers;

/// Artifact architecture values supported by Stone emission in this ABI.
pub const SUPPORTED_ARTIFACT_ARCHITECTURES: &[&str] = &["x86_64", "x86", "aarch64", "riscv64"];

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
    Ar,
    Ld,
    Objcopy,
    Nm,
    Ranlib,
    Strip,
    CcacheDir,
    SccacheDir,
    GoCacheDir,
    GoModCacheDir,
    CargoCacheDir,
    ZigCacheDir,
    RustcWrapper,
    SourcePath,
    SourceDestination,
}

/// Text built without an open-ended template language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSpec {
    Literal(String),
    Context(ContextValue),
    Concat(Vec<Self>),
}

impl Drop for TextSpec {
    fn drop(&mut self) {
        let Self::Concat(parts) = self else {
            return;
        };

        // Rust's generated drop glue follows recursive enum fields recursively.
        // Policy validation is iterative, so an over-depth value must also be
        // safe to reject and destroy without consuming one call frame per node.
        // Move each child vector onto an explicit stack and empty every
        // `Concat` before that node's own `Drop` implementation runs.
        let mut stack = Vec::new();
        stack.push(std::mem::take(parts));
        while let Some(children) = stack.last_mut() {
            let Some(mut child) = children.pop() else {
                stack.pop();
                continue;
            };
            if let Self::Concat(grandchildren) = &mut child
                && !grandchildren.is_empty()
            {
                stack.push(std::mem::take(grandchildren));
            }
        }
    }
}

/// Finite resource ceilings applied while accepting repository build policy.
///
/// The defaults are intentionally generous for a repository policy, while
/// still preventing a decoded value from driving unbounded allocation or
/// recursive traversal. Callers which accept policy from a less trusted
/// boundary can select tighter limits and pass the same value to Mason's
/// resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildPolicyValidationLimits {
    pub max_targets: usize,
    pub max_retired_targets: usize,
    pub max_environment_bindings: usize,
    pub max_tuning_flags: usize,
    pub max_tuning_groups: usize,
    pub max_tuning_choices: usize,
    pub max_tuning_default_groups: usize,
    pub max_tuning_option_flags: usize,
    pub max_build_root_tools: usize,
    pub max_compiler_flags: usize,
    pub max_builder_arguments: usize,
    pub max_analyzers: usize,
    pub max_pgo_arguments: usize,
    pub max_pgo_inputs: usize,
    pub max_string_bytes: usize,
    pub max_total_collection_items: usize,
    pub max_total_string_bytes: usize,
    pub max_text_nodes: usize,
    pub max_text_depth: usize,
    pub max_text_literal_bytes: usize,
    pub max_text_total_literal_bytes: usize,
    pub max_total_text_nodes: usize,
    pub max_total_text_literal_bytes: usize,
    pub max_resolved_text_bytes: usize,
    pub max_resolved_items: usize,
    pub max_total_resolved_text_nodes: usize,
    pub max_total_resolved_text_bytes: usize,
    pub max_resolver_steps: usize,
}

impl Default for BuildPolicyValidationLimits {
    fn default() -> Self {
        Self {
            max_targets: 64,
            max_retired_targets: 256,
            max_environment_bindings: 1_024,
            max_tuning_flags: 1_024,
            max_tuning_groups: 1_024,
            max_tuning_choices: 1_024,
            max_tuning_default_groups: 1_024,
            max_tuning_option_flags: 4_096,
            max_build_root_tools: 4_096,
            max_compiler_flags: 8_192,
            max_builder_arguments: 4_096,
            max_analyzers: 64,
            max_pgo_arguments: 4_096,
            max_pgo_inputs: 4_096,
            max_string_bytes: 256 * 1024,
            max_total_collection_items: 131_072,
            max_total_string_bytes: 64 * 1024 * 1024,
            max_text_nodes: 65_536,
            max_text_depth: 512,
            max_text_literal_bytes: 256 * 1024,
            max_text_total_literal_bytes: 8 * 1024 * 1024,
            max_total_text_nodes: 1_000_000,
            max_total_text_literal_bytes: 64 * 1024 * 1024,
            max_resolved_text_bytes: 8 * 1024 * 1024,
            max_resolved_items: 131_072,
            max_total_resolved_text_nodes: 1_000_000,
            max_total_resolved_text_bytes: 64 * 1024 * 1024,
            max_resolver_steps: 2_000_000,
        }
    }
}

impl TextSpec {
    /// Validate one standalone text tree without recursive Rust calls.
    pub fn validate_with_limits(&self, limits: BuildPolicyValidationLimits) -> Result<(), BuildPolicyConversionError> {
        let mut validator = ResourceValidator::new(limits);
        validator.text("text", self)?;
        require_text("text", self)
    }
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
    pub cc: BuildCommandSpec,
    pub cxx: BuildCommandSpec,
    pub objc: BuildCommandSpec,
    pub objcxx: BuildCommandSpec,
    pub cpp: BuildCommandSpec,
    pub objcpp: BuildCommandSpec,
    pub objcxxcpp: BuildCommandSpec,
    pub ar: BuildCommandSpec,
    pub ld: BuildCommandSpec,
    pub objcopy: BuildCommandSpec,
    pub nm: BuildCommandSpec,
    pub ranlib: BuildCommandSpec,
    pub strip: BuildCommandSpec,
}

/// One executable and its exact, already-tokenized command arguments.
///
/// Unlike [`BuilderCommandSpec`], compiler commands are static repository
/// policy: they have no working directory, environment overlay, or context
/// expansion. This makes every executable capability visible to dependency
/// locking while still allowing tools such as preprocessors to carry fixed
/// arguments without embedding shell syntax in a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCommandSpec {
    pub program: BuildProgramSpec,
    pub args: Vec<String>,
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

impl BuildToolSpec {
    /// Lower a policy-owned tool capability through the shared Stone relation
    /// model. This is the sole provider request used by build-root planning.
    pub fn dependency(&self) -> Result<Dependency, ParseError> {
        let (kind, target) = match self {
            Self::Package(target) => (RelationKind::PackageName, target),
            Self::Binary(target) => (RelationKind::Binary, target),
            Self::SystemBinary(target) => (RelationKind::SystemBinary, target),
        };
        Dependency::new(kind, target.clone())
    }

    /// Return the canonical guest program named by an executable capability.
    /// Package-name requirements deliberately have no executable path.
    pub fn executable_program(&self) -> Option<String> {
        match self {
            Self::Package(_) => None,
            Self::Binary(target) => Some(format!("/usr/bin/{target}")),
            Self::SystemBinary(target) => Some(format!("/usr/sbin/{target}")),
        }
    }
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

/// Analyzer programs selected for one compiler-toolchain family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzerToolchainPolicySpec {
    pub objcopy: BuildToolSpec,
    pub strip: BuildToolSpec,
}

/// Every external program an analyzer handler can invoke.
///
/// These capabilities are separate from the ordinary compiler and base
/// package lists so planning can request only the tools reachable from the
/// frozen handler/options combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzerToolsPolicySpec {
    pub pkg_config: BuildToolSpec,
    pub python: BuildToolSpec,
    pub llvm: AnalyzerToolchainPolicySpec,
    pub gnu: AnalyzerToolchainPolicySpec,
}

/// Fixed compiler-cache executables and guest cache paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCachePolicySpec {
    pub ccache: BuildProgramSpec,
    pub sccache: BuildProgramSpec,
    pub ccache_dir: String,
    pub sccache_dir: String,
    pub go_cache_dir: String,
    pub go_mod_cache_dir: String,
    pub cargo_cache_dir: String,
    pub zig_cache_dir: String,
}

/// Mold is a selectable repository feature, not a package-authored shell
/// fragment. Its linker executable, closure and language flags are all data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoldPolicySpec {
    pub linker: BuildCommandSpec,
    pub flags: CompilerFlagsSpec,
}

/// Hidden inputs which form the repository-owned base build root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRootPolicySpec {
    pub base: Vec<BuildToolSpec>,
    pub toolchains: ToolchainInputPolicySpec,
    pub emul32: Emul32InputPolicySpec,
    pub analyzer_tools: AnalyzerToolsPolicySpec,
    pub compiler_cache: CompilerCachePolicySpec,
    pub mold: MoldPolicySpec,
}

/// Stable guest paths mounted into every sandbox. These paths participate in
/// policy identity instead of being ambient Mason constants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicySpec {
    pub hostname: String,
    pub credentials: SandboxCredentialPolicySpec,
    pub filesystems: SandboxFilesystemPolicySpec,
    pub guest_root: String,
    pub artifacts_dir: String,
    pub build_dir: String,
    pub source_dir: String,
    pub recipe_dir: String,
    pub package_dir: String,
    pub install_dir: String,
}

/// Fixed credentials visible inside a frozen sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxCredentialPolicySpec {
    /// Namespace user/group ID zero maps only to the invoking caller.
    IsolatedRoot,
}

/// Repository-authorized pseudo-filesystems available to a build sandbox.
///
/// Proc is unconditionally absent from frozen builds and is therefore not an
/// authored policy value. The finite modes also cannot express any `/sys`
/// mount or a full host `/dev` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxFilesystemPolicySpec {
    pub tmp: SandboxTmpPolicySpec,
    pub sys: SandboxSysPolicySpec,
    pub dev: SandboxDevPolicySpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxTmpPolicySpec {
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxSysPolicySpec {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDevPolicySpec {
    None,
    Minimal,
}

/// An argv-preserving command template. Package-specific flags and binary
/// lists are added by the typed builder lowering, not hidden in these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderCommandSpec {
    pub program: BuildProgramSpec,
    pub args: Vec<TextSpec>,
    pub environment: Vec<EnvironmentBindingSpec>,
    pub working_dir: TextSpec,
}

/// One exact executable capability used by repository-owned structural policy.
///
/// The path is static data rather than context-expanded text. Its typed
/// provider request is carried beside it so planning can lock the executable
/// without basename inference, `PATH` lookup, or host filesystem discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildProgramSpec {
    pub path: String,
    pub requirement: BuildToolSpec,
}

/// Structural copy policy for one already-fetched git source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPreparationPolicySpec {
    pub create_directory: BuilderCommandSpec,
    pub copy: BuilderCommandSpec,
}

/// Repository-owned source preparation commands and their locked inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePreparationPolicySpec {
    pub git: GitPreparationPolicySpec,
}

/// Default commands and environment for one standard builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardBuilderPolicySpec {
    pub environment: Vec<EnvironmentBindingSpec>,
    pub setup: BuilderCommandSpec,
    pub build: BuilderCommandSpec,
    pub install: BuilderCommandSpec,
    pub check: BuilderCommandSpec,
}

/// Closed set of standard builders supported by package-v3.
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
    pub shell_interpreter: BuildProgramSpec,
    pub merge_program: BuildProgramSpec,
    pub merge_args: Vec<TextSpec>,
    pub copy_program: BuildProgramSpec,
    pub remove_program: BuildProgramSpec,
    pub sample: ToolchainFlagsSpec,
    pub stage_one: PgoStagePolicySpec,
    pub stage_two: PgoStagePolicySpec,
    pub use_profile: PgoStagePolicySpec,
}

/// One repository-authorized package analyzer in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalyzerKind {
    IgnoreBlocked,
    Binary,
    Elf,
    PkgConfig,
    Python,
    CMake,
    CompressMan,
    IncludeAny,
}

impl AnalyzerKind {
    /// Stable authored name used in diagnostics and canonical identities.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IgnoreBlocked => "IgnoreBlocked",
            Self::Binary => "Binary",
            Self::Elf => "Elf",
            Self::PkgConfig => "PkgConfig",
            Self::Python => "Python",
            Self::CMake => "CMake",
            Self::CompressMan => "CompressMan",
            Self::IncludeAny => "IncludeAny",
        }
    }
}

/// Concrete repository build policy returned from restricted Gluon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicySpec {
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
    pub analyzers: Vec<AnalyzerKind>,
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
    /// Apply one ordered-array operation after checking its final length and
    /// before reserving or combining either vector.
    pub fn apply_validated_with_limits(
        self,
        mut current: Vec<T>,
        field: &str,
        max_items: usize,
    ) -> Result<Vec<T>, BuildPolicyConversionError> {
        let patch_len = match &self {
            Self::Keep => 0,
            Self::Replace(values) | Self::Prepend(values) | Self::Append(values) => values.len(),
        };
        let count = match &self {
            Self::Keep => current.len(),
            Self::Replace(_) => patch_len,
            Self::Prepend(_) | Self::Append(_) => current.len().saturating_add(patch_len),
        };
        if count > max_items {
            return Err(BuildPolicyConversionError::CollectionLimit {
                field: field.to_owned(),
                count,
                limit: max_items,
            });
        }

        match self {
            Self::Keep => Ok(current),
            Self::Replace(values) => Ok(values),
            Self::Prepend(mut values) => {
                values
                    .try_reserve_exact(current.len())
                    .map_err(|_| BuildPolicyConversionError::Capacity {
                        field: field.to_owned(),
                        count,
                    })?;
                values.append(&mut current);
                Ok(values)
            }
            Self::Append(values) => {
                current
                    .try_reserve_exact(values.len())
                    .map_err(|_| BuildPolicyConversionError::Capacity {
                        field: field.to_owned(),
                        count,
                    })?;
                current.extend(values);
                Ok(current)
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
    pub analyzers: ArrayPatch<AnalyzerKind>,
    pub pgo: ValuePatch<PgoPolicySpec>,
}

impl BuildPolicyPatchSpec {
    /// Apply the patch and reject a resulting policy which violates the same
    /// invariants as a directly evaluated policy root.
    pub fn apply_validated(self, policy: BuildPolicySpec) -> Result<BuildPolicySpec, BuildPolicyConversionError> {
        self.apply_validated_with_limits(policy, BuildPolicyValidationLimits::default())
    }

    /// Apply a patch with bounded array combination, then validate every field
    /// of the resulting policy under the same ceilings.
    pub fn apply_validated_with_limits(
        self,
        policy: BuildPolicySpec,
        limits: BuildPolicyValidationLimits,
    ) -> Result<BuildPolicySpec, BuildPolicyConversionError> {
        policy.validate_with_limits(limits)?;

        let Self {
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
            analyzers,
            pgo,
        } = self;
        let BuildPolicySpec {
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
            analyzers: current_analyzers,
            pgo: current_pgo,
        } = policy;

        let policy = BuildPolicySpec {
            build_subdir: build_subdir.apply(current_build_subdir),
            layout: layout.apply(current_layout),
            toolchains: toolchains.apply(current_toolchains),
            targets: targets.apply_validated_with_limits(current_targets, "targets", limits.max_targets)?,
            retired_targets: retired_targets.apply_validated_with_limits(
                current_retired_targets,
                "retired_targets",
                limits.max_retired_targets,
            )?,
            sandbox: sandbox.apply(current_sandbox),
            build_root: build_root.apply(current_build_root),
            sources: sources.apply(current_sources),
            tuning: tuning.apply(current_tuning),
            environment: environment.apply_validated_with_limits(
                current_environment,
                "environment",
                limits.max_environment_bindings,
            )?,
            builders: builders.apply(current_builders),
            analyzers: analyzers.apply_validated_with_limits(current_analyzers, "analyzers", limits.max_analyzers)?,
            pgo: pgo.apply(current_pgo),
        };
        policy.validate_with_limits(limits)?;
        Ok(policy)
    }
}

/// Semantic policy error with a stable field path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildPolicyConversionError {
    #[error("{field}: collection has {count} items, limit is {limit}")]
    CollectionLimit { field: String, count: usize, limit: usize },
    #[error("{field}: string has {bytes} bytes, limit is {limit}")]
    StringBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("policy collections contain {count} items in total, limit is {limit}")]
    TotalCollectionItemsLimit { count: usize, limit: usize },
    #[error("policy strings contain {bytes} bytes in total, limit is {limit}")]
    TotalStringBytesLimit { bytes: usize, limit: usize },
    #[error("{field}: text has at least {nodes} nodes, limit is {limit}")]
    TextNodeLimit { field: String, nodes: usize, limit: usize },
    #[error("{field}: text depth is {depth}, limit is {limit}")]
    TextDepthLimit { field: String, depth: usize, limit: usize },
    #[error("{field}: literal has {bytes} bytes, limit is {limit}")]
    TextLiteralBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("{field}: text literals contain {bytes} bytes in total, limit is {limit}")]
    TextTotalLiteralBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("policy text contains {nodes} nodes in total, limit is {limit}")]
    TotalTextNodesLimit { nodes: usize, limit: usize },
    #[error("policy text literals contain {bytes} bytes in total, limit is {limit}")]
    TotalTextLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("{field}: unable to reserve bounded capacity for {count} items")]
    Capacity { field: String, count: usize },
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("{field}: duplicate value `{value}`")]
    Duplicate { field: String, value: String },
    #[error("{field}: required value `{value}` is missing")]
    MissingRequired { field: String, value: String },
    #[error("{field}: value `{value}` must be last")]
    MustBeLast { field: String, value: String },
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
    #[error("{field}: invalid sandbox hostname `{value}`")]
    InvalidHostname { field: String, value: String },
    #[error("{field}: target name `{value}` must be a normalized safe relative path")]
    InvalidTargetName { field: String, value: String },
    #[error("{field}: unsupported artifact architecture `{value}`; expected one of {supported}")]
    UnsupportedArtifactArchitecture {
        field: String,
        value: String,
        supported: String,
    },
    #[error("{field}: guest path `{value}` is outside `{guest_root}`")]
    GuestPathOutsideRoot {
        field: String,
        value: String,
        guest_root: String,
    },
    #[error("{field}: guest path `{value}` overlaps {other_field} `{other}`")]
    OverlappingGuestPath {
        field: String,
        value: String,
        other_field: String,
        other: String,
    },
    #[error("{field}: platform component must be explicit, found `{value}`")]
    InvalidPlatformComponent { field: String, value: String },
    #[error("{field}: architecture `{value}` does not match `{expected}`")]
    ArchitectureMismatch {
        field: String,
        value: String,
        expected: String,
    },
    #[error("{field}: analyzer tool must be a binary or system-binary capability")]
    AnalyzerToolMustBeExecutable { field: String },
    #[error("{field}: analyzer executable `{value}` must be one normalized filename component")]
    InvalidAnalyzerExecutable { field: String, value: String },
    #[error("{field}: executable path `{value}` must be a normalized, non-root absolute path")]
    InvalidProgramPath { field: String, value: String },
    #[error("{field}: executable requirement `{value}` is invalid")]
    InvalidProgramRequirement { field: String, value: String },
    #[error("{field}: expected executable path `{expected}` for its provider, found `{found}`")]
    ProgramPathMismatch {
        field: String,
        expected: String,
        found: String,
    },
    #[error("{field}: package-bound executable path `{value}` must not use the binary provider namespaces")]
    AmbiguousPackageProgram { field: String, value: String },
    #[error("{field}: command argument contains an embedded NUL byte")]
    InvalidCommandArgument { field: String },
}

struct ResourceValidator {
    limits: BuildPolicyValidationLimits,
    total_collection_items: usize,
    total_string_bytes: usize,
    total_text_nodes: usize,
    total_text_literal_bytes: usize,
}

impl ResourceValidator {
    fn new(limits: BuildPolicyValidationLimits) -> Self {
        Self {
            limits,
            total_collection_items: 0,
            total_string_bytes: 0,
            total_text_nodes: 0,
            total_text_literal_bytes: 0,
        }
    }

    fn policy(&mut self, policy: &BuildPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.string("build_subdir", &policy.build_subdir)?;
        self.layout(&policy.layout)?;
        self.compiler_tools("toolchains.llvm", &policy.toolchains.llvm)?;
        self.compiler_tools("toolchains.gnu", &policy.toolchains.gnu)?;

        self.collection("targets", policy.targets.len(), self.limits.max_targets)?;
        for (index, target) in policy.targets.iter().enumerate() {
            self.target(&format!("targets[{index}]"), target)?;
        }
        self.collection(
            "retired_targets",
            policy.retired_targets.len(),
            self.limits.max_retired_targets,
        )?;
        for (index, target) in policy.retired_targets.iter().enumerate() {
            self.string(&format!("retired_targets[{index}].name"), &target.name)?;
            self.string(&format!("retired_targets[{index}].reason"), &target.reason)?;
        }

        self.sandbox(&policy.sandbox)?;
        self.build_root(&policy.build_root)?;
        self.sources(&policy.sources)?;
        self.tuning(&policy.tuning)?;
        self.bindings("environment", &policy.environment)?;
        self.builder("builders.cmake", &policy.builders.cmake)?;
        self.builder("builders.meson", &policy.builders.meson)?;
        self.builder("builders.cargo", &policy.builders.cargo)?;
        self.builder("builders.autotools", &policy.builders.autotools)?;
        self.collection("analyzers", policy.analyzers.len(), self.limits.max_analyzers)?;
        self.pgo(&policy.pgo)
    }

    fn collection(&mut self, field: &str, count: usize, limit: usize) -> Result<(), BuildPolicyConversionError> {
        if count > limit {
            return Err(BuildPolicyConversionError::CollectionLimit {
                field: field.to_owned(),
                count,
                limit,
            });
        }
        self.total_collection_items = self.total_collection_items.saturating_add(count);
        if self.total_collection_items > self.limits.max_total_collection_items {
            return Err(BuildPolicyConversionError::TotalCollectionItemsLimit {
                count: self.total_collection_items,
                limit: self.limits.max_total_collection_items,
            });
        }
        Ok(())
    }

    fn string(&mut self, field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
        let bytes = value.len();
        if bytes > self.limits.max_string_bytes {
            return Err(BuildPolicyConversionError::StringBytesLimit {
                field: field.to_owned(),
                bytes,
                limit: self.limits.max_string_bytes,
            });
        }
        self.total_string_bytes = self.total_string_bytes.saturating_add(bytes);
        if self.total_string_bytes > self.limits.max_total_string_bytes {
            return Err(BuildPolicyConversionError::TotalStringBytesLimit {
                bytes: self.total_string_bytes,
                limit: self.limits.max_total_string_bytes,
            });
        }
        Ok(())
    }

    fn text(&mut self, field: &str, value: &TextSpec) -> Result<(), BuildPolicyConversionError> {
        let mut stack = vec![(value, 1usize)];
        let mut nodes = 0usize;
        let mut literal_bytes = 0usize;

        while let Some((value, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            if nodes > self.limits.max_text_nodes {
                return Err(BuildPolicyConversionError::TextNodeLimit {
                    field: field.to_owned(),
                    nodes,
                    limit: self.limits.max_text_nodes,
                });
            }
            self.total_text_nodes = self.total_text_nodes.saturating_add(1);
            if self.total_text_nodes > self.limits.max_total_text_nodes {
                return Err(BuildPolicyConversionError::TotalTextNodesLimit {
                    nodes: self.total_text_nodes,
                    limit: self.limits.max_total_text_nodes,
                });
            }
            if depth > self.limits.max_text_depth {
                return Err(BuildPolicyConversionError::TextDepthLimit {
                    field: field.to_owned(),
                    depth,
                    limit: self.limits.max_text_depth,
                });
            }

            match value {
                TextSpec::Literal(value) => {
                    let bytes = value.len();
                    if bytes > self.limits.max_text_literal_bytes {
                        return Err(BuildPolicyConversionError::TextLiteralBytesLimit {
                            field: field.to_owned(),
                            bytes,
                            limit: self.limits.max_text_literal_bytes,
                        });
                    }
                    literal_bytes = literal_bytes.saturating_add(bytes);
                    if literal_bytes > self.limits.max_text_total_literal_bytes {
                        return Err(BuildPolicyConversionError::TextTotalLiteralBytesLimit {
                            field: field.to_owned(),
                            bytes: literal_bytes,
                            limit: self.limits.max_text_total_literal_bytes,
                        });
                    }
                    self.total_text_literal_bytes = self.total_text_literal_bytes.saturating_add(bytes);
                    if self.total_text_literal_bytes > self.limits.max_total_text_literal_bytes {
                        return Err(BuildPolicyConversionError::TotalTextLiteralBytesLimit {
                            bytes: self.total_text_literal_bytes,
                            limit: self.limits.max_total_text_literal_bytes,
                        });
                    }
                    self.string(field, value)?;
                }
                TextSpec::Context(_) => {}
                TextSpec::Concat(parts) => {
                    let minimum_nodes = nodes.saturating_add(stack.len()).saturating_add(parts.len());
                    if minimum_nodes > self.limits.max_text_nodes {
                        return Err(BuildPolicyConversionError::TextNodeLimit {
                            field: field.to_owned(),
                            nodes: minimum_nodes,
                            limit: self.limits.max_text_nodes,
                        });
                    }
                    let minimum_total = self
                        .total_text_nodes
                        .saturating_add(stack.len())
                        .saturating_add(parts.len());
                    if minimum_total > self.limits.max_total_text_nodes {
                        return Err(BuildPolicyConversionError::TotalTextNodesLimit {
                            nodes: minimum_total,
                            limit: self.limits.max_total_text_nodes,
                        });
                    }
                    stack
                        .try_reserve(parts.len())
                        .map_err(|_| BuildPolicyConversionError::Capacity {
                            field: field.to_owned(),
                            count: minimum_nodes,
                        })?;
                    let child_depth = depth.saturating_add(1);
                    for part in parts.iter().rev() {
                        stack.push((part, child_depth));
                    }
                }
            }
        }
        Ok(())
    }

    fn texts(&mut self, field: &str, values: &[TextSpec], limit: usize) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, values.len(), limit)?;
        for (index, value) in values.iter().enumerate() {
            self.text(&format!("{field}[{index}]"), value)?;
        }
        Ok(())
    }

    fn layout(&mut self, layout: &InstallLayoutSpec) -> Result<(), BuildPolicyConversionError> {
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
            self.text(&format!("layout.{name}"), value)?;
        }
        Ok(())
    }

    fn compiler_tools(&mut self, field: &str, tools: &CompilerToolsSpec) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("cc", &tools.cc),
            ("cxx", &tools.cxx),
            ("objc", &tools.objc),
            ("objcxx", &tools.objcxx),
            ("cpp", &tools.cpp),
            ("objcpp", &tools.objcpp),
            ("objcxxcpp", &tools.objcxxcpp),
            ("ar", &tools.ar),
            ("ld", &tools.ld),
            ("objcopy", &tools.objcopy),
            ("nm", &tools.nm),
            ("ranlib", &tools.ranlib),
            ("strip", &tools.strip),
        ] {
            self.build_command(&format!("{field}.{name}"), value)?;
        }
        Ok(())
    }

    fn target(&mut self, field: &str, target: &TargetPolicySpec) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("name", &target.name),
            ("target_triple", &target.target_triple),
            ("build_triple", &target.build_triple),
            ("host_triple", &target.host_triple),
            ("lib_suffix", &target.lib_suffix),
            ("artifact_architecture", &target.artifact_architecture),
        ] {
            self.string(&format!("{field}.{name}"), value)?;
        }
        if let TargetEmulationSpec::Emul32 { host_architecture } = &target.emulation {
            self.string(&format!("{field}.emulation.host_architecture"), host_architecture)?;
        }
        self.platform(&format!("{field}.build_platform"), &target.build_platform)?;
        self.platform(&format!("{field}.host_platform"), &target.host_platform)?;
        self.platform(&format!("{field}.target_platform"), &target.target_platform)?;
        self.toolchain_flags(&format!("{field}.architecture_flags"), &target.architecture_flags)?;
        self.bindings(&format!("{field}.environment"), &target.environment)
    }

    fn platform(&mut self, field: &str, platform: &PlatformPolicySpec) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("architecture", &platform.architecture),
            ("vendor", &platform.vendor),
            ("operating_system", &platform.operating_system),
            ("abi", &platform.abi),
        ] {
            self.string(&format!("{field}.{name}"), value)?;
        }
        Ok(())
    }

    fn sandbox(&mut self, sandbox: &SandboxPolicySpec) -> Result<(), BuildPolicyConversionError> {
        for (name, value) in [
            ("hostname", &sandbox.hostname),
            ("guest_root", &sandbox.guest_root),
            ("artifacts_dir", &sandbox.artifacts_dir),
            ("build_dir", &sandbox.build_dir),
            ("source_dir", &sandbox.source_dir),
            ("recipe_dir", &sandbox.recipe_dir),
            ("package_dir", &sandbox.package_dir),
            ("install_dir", &sandbox.install_dir),
        ] {
            self.string(&format!("sandbox.{name}"), value)?;
        }
        Ok(())
    }

    fn build_root(&mut self, root: &BuildRootPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.tools("build_root.base", &root.base)?;
        self.toolchain_inputs("build_root.toolchains", &root.toolchains)?;
        self.tools("build_root.emul32.base", &root.emul32.base)?;
        self.toolchain_inputs("build_root.emul32.toolchains", &root.emul32.toolchains)?;
        for (field, tool) in [
            ("build_root.analyzer_tools.pkg_config", &root.analyzer_tools.pkg_config),
            ("build_root.analyzer_tools.python", &root.analyzer_tools.python),
            (
                "build_root.analyzer_tools.llvm.objcopy",
                &root.analyzer_tools.llvm.objcopy,
            ),
            ("build_root.analyzer_tools.llvm.strip", &root.analyzer_tools.llvm.strip),
            (
                "build_root.analyzer_tools.gnu.objcopy",
                &root.analyzer_tools.gnu.objcopy,
            ),
            ("build_root.analyzer_tools.gnu.strip", &root.analyzer_tools.gnu.strip),
        ] {
            self.tool(field, tool)?;
        }
        let cache = &root.compiler_cache;
        self.program("build_root.compiler_cache.ccache", &cache.ccache)?;
        self.program("build_root.compiler_cache.sccache", &cache.sccache)?;
        for (name, value) in [
            ("ccache_dir", &cache.ccache_dir),
            ("sccache_dir", &cache.sccache_dir),
            ("go_cache_dir", &cache.go_cache_dir),
            ("go_mod_cache_dir", &cache.go_mod_cache_dir),
            ("cargo_cache_dir", &cache.cargo_cache_dir),
            ("zig_cache_dir", &cache.zig_cache_dir),
        ] {
            self.string(&format!("build_root.compiler_cache.{name}"), value)?;
        }
        self.build_command("build_root.mold.linker", &root.mold.linker)?;
        self.compiler_flags("build_root.mold.flags", &root.mold.flags)
    }

    fn toolchain_inputs(
        &mut self,
        field: &str,
        inputs: &ToolchainInputPolicySpec,
    ) -> Result<(), BuildPolicyConversionError> {
        self.tools(&format!("{field}.llvm"), &inputs.llvm)?;
        self.tools(&format!("{field}.gnu"), &inputs.gnu)
    }

    fn tools(&mut self, field: &str, tools: &[BuildToolSpec]) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, tools.len(), self.limits.max_build_root_tools)?;
        for (index, tool) in tools.iter().enumerate() {
            self.tool(&format!("{field}[{index}]"), tool)?;
        }
        Ok(())
    }

    fn tool(&mut self, field: &str, tool: &BuildToolSpec) -> Result<(), BuildPolicyConversionError> {
        let value = match tool {
            BuildToolSpec::Package(value) | BuildToolSpec::Binary(value) | BuildToolSpec::SystemBinary(value) => value,
        };
        self.string(field, value)
    }

    fn sources(&mut self, sources: &SourcePreparationPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.command("sources.git.create_directory", &sources.git.create_directory)?;
        self.command("sources.git.copy", &sources.git.copy)
    }

    fn tuning(&mut self, tuning: &TuningPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.collection("tuning.flags", tuning.flags.len(), self.limits.max_tuning_flags)?;
        for (index, flag) in tuning.flags.iter().enumerate() {
            let field = format!("tuning.flags[{index}]");
            self.string(&format!("{field}.name"), &flag.name)?;
            self.toolchain_flags(&format!("{field}.value"), &flag.value)?;
        }
        self.collection("tuning.groups", tuning.groups.len(), self.limits.max_tuning_groups)?;
        for (index, group) in tuning.groups.iter().enumerate() {
            let field = format!("tuning.groups[{index}]");
            self.string(&format!("{field}.name"), &group.name)?;
            self.tuning_option(&format!("{field}.value.base"), &group.value.base)?;
            if let Some(default) = &group.value.default {
                self.string(&format!("{field}.value.default"), default)?;
            }
            self.collection(
                &format!("{field}.value.choices"),
                group.value.choices.len(),
                self.limits.max_tuning_choices,
            )?;
            for (choice_index, choice) in group.value.choices.iter().enumerate() {
                let choice_field = format!("{field}.value.choices[{choice_index}]");
                self.string(&format!("{choice_field}.name"), &choice.name)?;
                self.tuning_option(&format!("{choice_field}.value"), &choice.value)?;
            }
        }
        self.collection(
            "tuning.default_groups",
            tuning.default_groups.len(),
            self.limits.max_tuning_default_groups,
        )?;
        for (index, group) in tuning.default_groups.iter().enumerate() {
            self.string(&format!("tuning.default_groups[{index}]"), group)?;
        }
        Ok(())
    }

    fn tuning_option(&mut self, field: &str, option: &TuningOptionSpec) -> Result<(), BuildPolicyConversionError> {
        for (name, values) in [("enabled", &option.enabled), ("disabled", &option.disabled)] {
            let values_field = format!("{field}.{name}");
            self.collection(&values_field, values.len(), self.limits.max_tuning_option_flags)?;
            for (index, value) in values.iter().enumerate() {
                self.string(&format!("{values_field}[{index}]"), value)?;
            }
        }
        Ok(())
    }

    fn bindings(&mut self, field: &str, bindings: &[EnvironmentBindingSpec]) -> Result<(), BuildPolicyConversionError> {
        self.collection(field, bindings.len(), self.limits.max_environment_bindings)?;
        for (index, binding) in bindings.iter().enumerate() {
            self.string(&format!("{field}[{index}].name"), &binding.name)?;
            self.text(&format!("{field}[{index}].value"), &binding.value)?;
        }
        Ok(())
    }

    fn builder(&mut self, field: &str, builder: &StandardBuilderPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.bindings(&format!("{field}.environment"), &builder.environment)?;
        self.command(&format!("{field}.setup"), &builder.setup)?;
        self.command(&format!("{field}.build"), &builder.build)?;
        self.command(&format!("{field}.install"), &builder.install)?;
        self.command(&format!("{field}.check"), &builder.check)
    }

    fn command(&mut self, field: &str, command: &BuilderCommandSpec) -> Result<(), BuildPolicyConversionError> {
        self.program(&format!("{field}.program"), &command.program)?;
        self.text(&format!("{field}.working_dir"), &command.working_dir)?;
        self.texts(
            &format!("{field}.args"),
            &command.args,
            self.limits.max_builder_arguments,
        )?;
        self.bindings(&format!("{field}.environment"), &command.environment)
    }

    fn build_command(&mut self, field: &str, command: &BuildCommandSpec) -> Result<(), BuildPolicyConversionError> {
        self.program(&format!("{field}.program"), &command.program)?;
        self.collection(
            &format!("{field}.args"),
            command.args.len(),
            self.limits.max_builder_arguments,
        )?;
        for (index, argument) in command.args.iter().enumerate() {
            self.string(&format!("{field}.args[{index}]"), argument)?;
        }
        Ok(())
    }

    fn program(&mut self, field: &str, program: &BuildProgramSpec) -> Result<(), BuildPolicyConversionError> {
        self.string(&format!("{field}.path"), &program.path)?;
        self.tool(&format!("{field}.requirement"), &program.requirement)
    }

    fn toolchain_flags(&mut self, field: &str, flags: &ToolchainFlagsSpec) -> Result<(), BuildPolicyConversionError> {
        self.compiler_flags(&format!("{field}.common"), &flags.common)?;
        self.compiler_flags(&format!("{field}.gnu"), &flags.gnu)?;
        self.compiler_flags(&format!("{field}.llvm"), &flags.llvm)
    }

    fn compiler_flags(&mut self, field: &str, flags: &CompilerFlagsSpec) -> Result<(), BuildPolicyConversionError> {
        for (name, values) in [
            ("c", &flags.c),
            ("cxx", &flags.cxx),
            ("f", &flags.f),
            ("d", &flags.d),
            ("rust", &flags.rust),
            ("vala", &flags.vala),
            ("go", &flags.go),
            ("ld", &flags.ld),
        ] {
            self.texts(&format!("{field}.{name}"), values, self.limits.max_compiler_flags)?;
        }
        Ok(())
    }

    fn pgo(&mut self, pgo: &PgoPolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.program("pgo.shell_interpreter", &pgo.shell_interpreter)?;
        self.program("pgo.merge_program", &pgo.merge_program)?;
        self.texts("pgo.merge_args", &pgo.merge_args, self.limits.max_pgo_arguments)?;
        self.program("pgo.copy_program", &pgo.copy_program)?;
        self.program("pgo.remove_program", &pgo.remove_program)?;
        self.toolchain_flags("pgo.sample", &pgo.sample)?;
        self.pgo_stage("pgo.stage_one", &pgo.stage_one)?;
        self.pgo_stage("pgo.stage_two", &pgo.stage_two)?;
        self.pgo_stage("pgo.use_profile", &pgo.use_profile)
    }

    fn pgo_stage(&mut self, field: &str, stage: &PgoStagePolicySpec) -> Result<(), BuildPolicyConversionError> {
        self.toolchain_flags(&format!("{field}.flags"), &stage.flags)?;
        let Some(finish) = &stage.finish else {
            return Ok(());
        };
        self.text(&format!("{field}.finish.output"), &finish.output)?;
        self.texts(
            &format!("{field}.finish.inputs"),
            &finish.inputs,
            self.limits.max_pgo_inputs,
        )?;
        if let Some(copy_to) = &finish.copy_to {
            self.text(&format!("{field}.finish.copy_to"), copy_to)?;
        }
        Ok(())
    }
}

impl BuilderCommandSpec {
    /// Validate a command supplied independently from a complete policy.
    ///
    /// Mason uses this at fragment-taking boundaries so callers cannot bypass
    /// the collection, string, text, or semantic checks applied to commands
    /// embedded in [`BuildPolicySpec`].
    pub fn validate_with_limits(&self, limits: BuildPolicyValidationLimits) -> Result<(), BuildPolicyConversionError> {
        let mut validator = ResourceValidator::new(limits);
        validator.command("command", self)?;
        validate_command("command", self)
    }
}

/// Validate an environment fragment supplied independently from a complete
/// policy under the same resource and semantic rules as policy-owned bindings.
pub fn validate_environment_bindings_with_limits(
    bindings: &[EnvironmentBindingSpec],
    limits: BuildPolicyValidationLimits,
) -> Result<(), BuildPolicyConversionError> {
    let mut validator = ResourceValidator::new(limits);
    validator.bindings("environment", bindings)?;
    validate_bindings("environment", bindings)
}

impl BuildPolicySpec {
    /// Validate invariants needed before the policy can participate in a
    /// derivation fingerprint.
    pub fn validate(&self) -> Result<(), BuildPolicyConversionError> {
        self.validate_with_limits(BuildPolicyValidationLimits::default())
    }

    /// Validate semantic invariants and all configured finite resource
    /// ceilings before the policy participates in a derivation fingerprint.
    pub fn validate_with_limits(&self, limits: BuildPolicyValidationLimits) -> Result<(), BuildPolicyConversionError> {
        let mut validator = ResourceValidator::new(limits);
        validator.policy(self)?;
        self.validate_semantics()
    }

    fn validate_semantics(&self) -> Result<(), BuildPolicyConversionError> {
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
        validate_analyzers(&self.analyzers)?;

        validate_program("pgo.shell_interpreter", &self.pgo.shell_interpreter)?;
        validate_program("pgo.merge_program", &self.pgo.merge_program)?;
        if self.pgo.merge_args.is_empty() {
            return Err(BuildPolicyConversionError::Empty {
                field: "pgo.merge_args".to_owned(),
            });
        }
        for (index, argument) in self.pgo.merge_args.iter().enumerate() {
            require_text(&format!("pgo.merge_args[{index}]"), argument)?;
        }
        validate_program("pgo.copy_program", &self.pgo.copy_program)?;
        validate_program("pgo.remove_program", &self.pgo.remove_program)?;
        validate_pgo_stage("pgo.stage_one", &self.pgo.stage_one)?;
        validate_pgo_stage("pgo.stage_two", &self.pgo.stage_two)?;
        validate_pgo_stage("pgo.use_profile", &self.pgo.use_profile)
    }
}

fn validate_analyzers(analyzers: &[AnalyzerKind]) -> Result<(), BuildPolicyConversionError> {
    if analyzers.is_empty() {
        return Err(BuildPolicyConversionError::Empty {
            field: "analyzers".to_owned(),
        });
    }

    let mut values = BTreeSet::new();
    for analyzer in analyzers {
        if !values.insert(*analyzer) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "analyzers".to_owned(),
                value: analyzer.as_str().to_owned(),
            });
        }
    }

    let Some(include_any) = analyzers
        .iter()
        .position(|analyzer| *analyzer == AnalyzerKind::IncludeAny)
    else {
        return Err(BuildPolicyConversionError::MissingRequired {
            field: "analyzers".to_owned(),
            value: AnalyzerKind::IncludeAny.as_str().to_owned(),
        });
    };
    if include_any + 1 != analyzers.len() {
        return Err(BuildPolicyConversionError::MustBeLast {
            field: "analyzers".to_owned(),
            value: AnalyzerKind::IncludeAny.as_str().to_owned(),
        });
    }

    Ok(())
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

    if !SUPPORTED_ARTIFACT_ARCHITECTURES.contains(&target.artifact_architecture.as_str()) {
        return Err(BuildPolicyConversionError::UnsupportedArtifactArchitecture {
            field: format!("{field}.artifact_architecture"),
            value: target.artifact_architecture.clone(),
            supported: SUPPORTED_ARTIFACT_ARCHITECTURES.join(", "),
        });
    }

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
    validate_hostname("sandbox.hostname", &sandbox.hostname)?;
    validate_guest_path("sandbox.guest_root", &sandbox.guest_root)?;
    let mut paths = BTreeSet::new();
    for (name, value) in [
        ("artifacts_dir", &sandbox.artifacts_dir),
        ("build_dir", &sandbox.build_dir),
        ("source_dir", &sandbox.source_dir),
        ("recipe_dir", &sandbox.recipe_dir),
        ("package_dir", &sandbox.package_dir),
        ("install_dir", &sandbox.install_dir),
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
    reject_guest_path_overlaps(&[
        ("sandbox.artifacts_dir", &sandbox.artifacts_dir),
        ("sandbox.build_dir", &sandbox.build_dir),
        ("sandbox.source_dir", &sandbox.source_dir),
        ("sandbox.recipe_dir", &sandbox.recipe_dir),
        ("sandbox.install_dir", &sandbox.install_dir),
    ])?;
    Ok(())
}

fn validate_hostname(field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
    let labels_are_valid = !value.is_empty()
        && value.len() <= 64
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        });
    if labels_are_valid {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::InvalidHostname {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn validate_build_root(
    build_root: &BuildRootPolicySpec,
    sandbox: &SandboxPolicySpec,
) -> Result<(), BuildPolicyConversionError> {
    validate_tools("build_root.base", &build_root.base)?;
    validate_toolchain_inputs("build_root.toolchains", &build_root.toolchains)?;
    validate_tools("build_root.emul32.base", &build_root.emul32.base)?;
    validate_toolchain_inputs("build_root.emul32.toolchains", &build_root.emul32.toolchains)?;
    validate_analyzer_tools(&build_root.analyzer_tools)?;

    let cache = &build_root.compiler_cache;
    validate_program("build_root.compiler_cache.ccache", &cache.ccache)?;
    validate_program("build_root.compiler_cache.sccache", &cache.sccache)?;
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
    reject_guest_path_overlaps(&[
        ("sandbox.artifacts_dir", &sandbox.artifacts_dir),
        ("sandbox.build_dir", &sandbox.build_dir),
        ("sandbox.source_dir", &sandbox.source_dir),
        ("sandbox.recipe_dir", &sandbox.recipe_dir),
        ("sandbox.install_dir", &sandbox.install_dir),
        ("build_root.compiler_cache.ccache_dir", &cache.ccache_dir),
        ("build_root.compiler_cache.sccache_dir", &cache.sccache_dir),
        ("build_root.compiler_cache.go_cache_dir", &cache.go_cache_dir),
        ("build_root.compiler_cache.go_mod_cache_dir", &cache.go_mod_cache_dir),
        ("build_root.compiler_cache.cargo_cache_dir", &cache.cargo_cache_dir),
        ("build_root.compiler_cache.zig_cache_dir", &cache.zig_cache_dir),
    ])?;
    validate_build_command("build_root.mold.linker", &build_root.mold.linker)?;
    validate_compiler_flags("build_root.mold.flags", &build_root.mold.flags)
}

fn validate_analyzer_tools(tools: &AnalyzerToolsPolicySpec) -> Result<(), BuildPolicyConversionError> {
    for (field, tool) in [
        ("build_root.analyzer_tools.pkg_config", &tools.pkg_config),
        ("build_root.analyzer_tools.python", &tools.python),
        ("build_root.analyzer_tools.llvm.objcopy", &tools.llvm.objcopy),
        ("build_root.analyzer_tools.llvm.strip", &tools.llvm.strip),
        ("build_root.analyzer_tools.gnu.objcopy", &tools.gnu.objcopy),
        ("build_root.analyzer_tools.gnu.strip", &tools.gnu.strip),
    ] {
        let target = match tool {
            BuildToolSpec::Package(_) => {
                return Err(BuildPolicyConversionError::AnalyzerToolMustBeExecutable {
                    field: field.to_owned(),
                });
            }
            BuildToolSpec::Binary(target) | BuildToolSpec::SystemBinary(target) => target,
        };
        if !is_normalized_executable_name(target) {
            return Err(BuildPolicyConversionError::InvalidAnalyzerExecutable {
                field: field.to_owned(),
                value: target.clone(),
            });
        }
        tool.dependency()
            .map_err(|_| BuildPolicyConversionError::InvalidAnalyzerExecutable {
                field: field.to_owned(),
                value: target.clone(),
            })?;
    }
    Ok(())
}

fn is_normalized_executable_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_control)
}

fn reject_guest_path_overlaps(paths: &[(&str, &str)]) -> Result<(), BuildPolicyConversionError> {
    for (index, (field, value)) in paths.iter().enumerate() {
        for (other_field, other) in &paths[..index] {
            if Path::new(value).starts_with(other) || Path::new(other).starts_with(value) {
                return Err(BuildPolicyConversionError::OverlappingGuestPath {
                    field: (*field).to_owned(),
                    value: (*value).to_owned(),
                    other_field: (*other_field).to_owned(),
                    other: (*other).to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn validate_toolchain_inputs(field: &str, inputs: &ToolchainInputPolicySpec) -> Result<(), BuildPolicyConversionError> {
    validate_tools(&format!("{field}.llvm"), &inputs.llvm)?;
    validate_tools(&format!("{field}.gnu"), &inputs.gnu)
}

fn validate_sources(sources: &SourcePreparationPolicySpec) -> Result<(), BuildPolicyConversionError> {
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
    let mut normalized = PathBuf::new();
    let mut normal_components = 0usize;
    let mut safe_components = true;
    for component in path.components() {
        match component {
            Component::RootDir if normalized.as_os_str().is_empty() => normalized.push(component.as_os_str()),
            Component::Normal(_) => {
                normal_components += 1;
                normalized.push(component.as_os_str());
            }
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
                safe_components = false;
            }
        }
    }
    if path.is_absolute() && normal_components > 0 && safe_components && normalized.as_os_str() == path.as_os_str() {
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
        ("ar", &tools.ar),
        ("ld", &tools.ld),
        ("objcopy", &tools.objcopy),
        ("nm", &tools.nm),
        ("ranlib", &tools.ranlib),
        ("strip", &tools.strip),
    ] {
        validate_build_command(&format!("{field}.{name}"), value)?;
    }
    Ok(())
}

fn validate_build_command(field: &str, command: &BuildCommandSpec) -> Result<(), BuildPolicyConversionError> {
    validate_program(&format!("{field}.program"), &command.program)?;
    for (index, argument) in command.args.iter().enumerate() {
        if argument.contains('\0') {
            return Err(BuildPolicyConversionError::InvalidCommandArgument {
                field: format!("{field}.args[{index}]"),
            });
        }
    }
    Ok(())
}

fn validate_builder(field: &str, builder: &StandardBuilderPolicySpec) -> Result<(), BuildPolicyConversionError> {
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
    validate_program(&format!("{field}.program"), &command.program)?;
    require_text(&format!("{field}.working_dir"), &command.working_dir)?;
    for (index, argument) in command.args.iter().enumerate() {
        require_text(&format!("{field}.args[{index}]"), argument)?;
    }
    validate_bindings(&format!("{field}.environment"), &command.environment)
}

fn validate_program(field: &str, program: &BuildProgramSpec) -> Result<(), BuildPolicyConversionError> {
    let path = Path::new(&program.path);
    let mut normalized = PathBuf::new();
    let mut normal_components = 0;
    let mut safe_components = true;
    for component in path.components() {
        match component {
            Component::RootDir if normalized.as_os_str().is_empty() => normalized.push(component.as_os_str()),
            Component::Normal(_) => {
                normal_components += 1;
                normalized.push(component.as_os_str());
            }
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
                safe_components = false;
            }
        }
    }
    if !path.is_absolute() || normal_components == 0 || !safe_components || normalized.as_os_str() != path.as_os_str() {
        return Err(BuildPolicyConversionError::InvalidProgramPath {
            field: format!("{field}.path"),
            value: program.path.clone(),
        });
    }

    let target = match &program.requirement {
        BuildToolSpec::Package(target) => {
            require_string(&format!("{field}.requirement"), target)?;
            program
                .requirement
                .dependency()
                .map_err(|_| BuildPolicyConversionError::InvalidProgramRequirement {
                    field: format!("{field}.requirement"),
                    value: target.clone(),
                })?;
            if path
                .parent()
                .is_some_and(|parent| parent == Path::new("/usr/bin") || parent == Path::new("/usr/sbin"))
            {
                return Err(BuildPolicyConversionError::AmbiguousPackageProgram {
                    field: format!("{field}.path"),
                    value: program.path.clone(),
                });
            }
            return Ok(());
        }
        BuildToolSpec::Binary(target) | BuildToolSpec::SystemBinary(target) => target,
    };
    if !is_normalized_executable_name(target) || program.requirement.dependency().is_err() {
        return Err(BuildPolicyConversionError::InvalidProgramRequirement {
            field: format!("{field}.requirement"),
            value: target.clone(),
        });
    }
    let expected = program
        .requirement
        .executable_program()
        .expect("binary requirements have canonical programs");
    if program.path != expected {
        return Err(BuildPolicyConversionError::ProgramPathMismatch {
            field: format!("{field}.path"),
            expected,
            found: program.path.clone(),
        });
    }
    Ok(())
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
    if text_is_statically_empty(value) {
        Err(BuildPolicyConversionError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn text_is_statically_empty(value: &TextSpec) -> bool {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            TextSpec::Literal(value) if !value.is_empty() => return false,
            TextSpec::Context(_) => return false,
            TextSpec::Concat(parts) => stack.extend(parts),
            TextSpec::Literal(_) => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use gluon_config::Source;

    use super::*;

    fn repository_policy() -> BuildPolicySpec {
        evaluate_gluon(&Source::new(
            "crates/mason/data/policy/default.glu",
            include_str!("../../../mason/data/policy/default.glu"),
        ))
        .unwrap()
        .policy
    }

    #[test]
    fn text_node_limit_accepts_n_and_rejects_n_plus_one() {
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_nodes = 4;

        let at_limit = TextSpec::Concat(vec![
            TextSpec::Literal("a".to_owned()),
            TextSpec::Literal("b".to_owned()),
            TextSpec::Literal("c".to_owned()),
        ]);
        assert_eq!(at_limit.validate_with_limits(limits), Ok(()));

        let over_limit = TextSpec::Concat(vec![
            TextSpec::Literal("a".to_owned()),
            TextSpec::Literal("b".to_owned()),
            TextSpec::Literal("c".to_owned()),
            TextSpec::Literal("d".to_owned()),
        ]);
        assert_eq!(
            over_limit.validate_with_limits(limits),
            Err(BuildPolicyConversionError::TextNodeLimit {
                field: "text".to_owned(),
                nodes: 5,
                limit: 4,
            })
        );
    }

    #[test]
    fn text_literal_limits_accept_n_and_reject_n_plus_one() {
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_literal_bytes = 3;
        limits.max_text_total_literal_bytes = 3;
        assert_eq!(TextSpec::Literal("abc".to_owned()).validate_with_limits(limits), Ok(()));
        assert_eq!(
            TextSpec::Literal("abcd".to_owned()).validate_with_limits(limits),
            Err(BuildPolicyConversionError::TextLiteralBytesLimit {
                field: "text".to_owned(),
                bytes: 4,
                limit: 3,
            })
        );

        limits.max_text_literal_bytes = 3;
        let over_total = TextSpec::Concat(vec![
            TextSpec::Literal("ab".to_owned()),
            TextSpec::Literal("cd".to_owned()),
        ]);
        assert_eq!(
            over_total.validate_with_limits(limits),
            Err(BuildPolicyConversionError::TextTotalLiteralBytesLimit {
                field: "text".to_owned(),
                bytes: 4,
                limit: 3,
            })
        );
    }

    #[test]
    fn deeply_nested_text_is_rejected_iteratively() {
        let mut value = TextSpec::Literal("x".to_owned());
        for _ in 0..20_000 {
            value = TextSpec::Concat(vec![value]);
        }
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_nodes = 25_000;
        limits.max_text_depth = 64;
        assert_eq!(
            value.validate_with_limits(limits),
            Err(BuildPolicyConversionError::TextDepthLimit {
                field: "text".to_owned(),
                depth: 65,
                limit: 64,
            })
        );
    }

    #[test]
    fn representative_policy_collection_accepts_n_and_rejects_n_plus_one() {
        let policy = repository_policy();
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_targets = policy.targets.len();
        assert_eq!(policy.validate_with_limits(limits), Ok(()));

        let mut oversized = policy;
        oversized.targets.push(oversized.targets[0].clone());
        assert_eq!(
            oversized.validate_with_limits(limits),
            Err(BuildPolicyConversionError::CollectionLimit {
                field: "targets".to_owned(),
                count: limits.max_targets + 1,
                limit: limits.max_targets,
            })
        );
    }

    #[test]
    fn aggregate_budgets_accept_n_and_reject_n_plus_one() {
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_total_collection_items = 3;
        let mut at_limit = ResourceValidator::new(limits);
        at_limit.collection("first", 1, 3).unwrap();
        at_limit.collection("second", 2, 3).unwrap();
        assert_eq!(
            at_limit.collection("third", 1, 3),
            Err(BuildPolicyConversionError::TotalCollectionItemsLimit { count: 4, limit: 3 })
        );

        limits.max_total_string_bytes = 3;
        let mut strings = ResourceValidator::new(limits);
        strings.string("first", "a").unwrap();
        strings.string("second", "bc").unwrap();
        assert_eq!(
            strings.string("third", "d"),
            Err(BuildPolicyConversionError::TotalStringBytesLimit { bytes: 4, limit: 3 })
        );

        limits.max_total_text_literal_bytes = 3;
        let mut text = ResourceValidator::new(limits);
        text.text("first", &TextSpec::Literal("a".to_owned())).unwrap();
        text.text("second", &TextSpec::Literal("bc".to_owned())).unwrap();
        assert_eq!(
            text.text("third", &TextSpec::Literal("d".to_owned())),
            Err(BuildPolicyConversionError::TotalTextLiteralBytesLimit { bytes: 4, limit: 3 })
        );

        limits.max_total_text_nodes = 3;
        let mut nodes = ResourceValidator::new(limits);
        nodes.text("first", &TextSpec::Literal("a".to_owned())).unwrap();
        nodes
            .text("second", &TextSpec::Concat(vec![TextSpec::Literal("b".to_owned())]))
            .unwrap();
        assert_eq!(
            nodes.text("third", &TextSpec::Context(ContextValue::Jobs)),
            Err(BuildPolicyConversionError::TotalTextNodesLimit { nodes: 4, limit: 3 })
        );
    }

    #[test]
    fn actual_policy_total_text_nodes_accepts_n_and_rejects_n_plus_one() {
        let policy = repository_policy();
        let mut measured = ResourceValidator::new(BuildPolicyValidationLimits::default());
        measured.policy(&policy).unwrap();
        let total_text_nodes = measured.total_text_nodes;

        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_total_text_nodes = total_text_nodes;
        assert_eq!(policy.validate_with_limits(limits), Ok(()));

        let mut oversized = policy;
        oversized
            .sources
            .git
            .copy
            .args
            .push(TextSpec::Context(ContextValue::Jobs));
        assert_eq!(
            oversized.validate_with_limits(limits),
            Err(BuildPolicyConversionError::TotalTextNodesLimit {
                nodes: total_text_nodes + 1,
                limit: total_text_nodes,
            })
        );
    }

    #[test]
    fn every_dynamic_policy_branch_contributes_to_the_collection_aggregate() {
        let policy = repository_policy();
        let mut measured = ResourceValidator::new(BuildPolicyValidationLimits::default());
        measured.policy(&policy).unwrap();
        let total_collection_items = measured.total_collection_items;
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_total_collection_items = total_collection_items;

        macro_rules! assert_counted {
            ($mutate:expr) => {{
                let mut oversized = policy.clone();
                $mutate(&mut oversized);
                assert!(matches!(
                    oversized.validate_with_limits(limits),
                    Err(BuildPolicyConversionError::TotalCollectionItemsLimit { limit, .. })
                        if limit == total_collection_items
                ));
            }};
        }

        assert_counted!(|value: &mut BuildPolicySpec| value.targets[0].environment.push(value.environment[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value.targets[0]
            .architecture_flags
            .common
            .c
            .push(TextSpec::Literal("-fbranch-test".to_owned())));
        assert_counted!(|value: &mut BuildPolicySpec| value.retired_targets.push(value.retired_targets[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value.build_root.base.push(value.build_root.base[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value
            .sources
            .git
            .copy
            .args
            .push(TextSpec::Literal("branch-test".to_owned())));
        assert_counted!(|value: &mut BuildPolicySpec| value
            .tuning
            .default_groups
            .push(value.tuning.default_groups[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value.environment.push(value.environment[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value
            .builders
            .cmake
            .environment
            .push(value.environment[0].clone()));
        assert_counted!(|value: &mut BuildPolicySpec| value.analyzers.push(value.analyzers[0]));
        assert_counted!(|value: &mut BuildPolicySpec| value.pgo.merge_args.push(value.pgo.merge_args[0].clone()));
    }

    #[test]
    fn array_patch_preflights_lengths_and_preserves_order() {
        assert_eq!(
            ArrayPatch::Append(vec![3]).apply_validated_with_limits(vec![1, 2], "values", 3),
            Ok(vec![1, 2, 3])
        );
        assert_eq!(
            ArrayPatch::Prepend(vec![1, 2]).apply_validated_with_limits(vec![3], "values", 3),
            Ok(vec![1, 2, 3])
        );
        assert_eq!(
            ArrayPatch::Replace(vec![1, 2, 3, 4]).apply_validated_with_limits(Vec::new(), "values", 3),
            Err(BuildPolicyConversionError::CollectionLimit {
                field: "values".to_owned(),
                count: 4,
                limit: 3,
            })
        );
        assert_eq!(
            ArrayPatch::Append(vec![3, 4]).apply_validated_with_limits(vec![1, 2], "values", 3),
            Err(BuildPolicyConversionError::CollectionLimit {
                field: "values".to_owned(),
                count: 4,
                limit: 3,
            })
        );
    }

    #[test]
    fn validated_patch_revalidates_scalar_replacements_with_same_limits() {
        let policy = repository_policy();
        let mut layout = policy.layout.clone();
        layout.prefix = TextSpec::Literal("x".repeat(129));
        let patch = BuildPolicyPatchSpec {
            layout: ValuePatch::Set(layout),
            ..BuildPolicyPatchSpec::default()
        };
        let mut limits = BuildPolicyValidationLimits::default();
        limits.max_text_literal_bytes = 128;

        assert_eq!(
            patch.apply_validated_with_limits(policy, limits),
            Err(BuildPolicyConversionError::TextLiteralBytesLimit {
                field: "layout.prefix".to_owned(),
                bytes: 129,
                limit: 128,
            })
        );
    }
}
