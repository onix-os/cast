//! Typed repository policy used to lower package builders into frozen plans.
//!
//! Policy text is deliberately finite: authored values may only contain
//! literals and references to known planning inputs. There is no general
//! string interpolation or action expansion at this boundary.

use stone::relation::{Dependency, Kind as RelationKind, ParseError};

pub use self::gluon::{
    BUILD_POLICY_ABI_VERSION, GLUON_BUILD_POLICY_ABI,
    GluonBuildPolicyEvaluator,
};
pub use self::lua::{BuildPolicyEvaluator, LuaBuildPolicyEvaluator, encode_lua_policy};

mod gluon;
pub mod layers;
mod lua;
mod validation;

pub use validation::{
    BuildPolicyConversionError, BuildPolicyValidationLimits, validate_environment_bindings_with_limits,
};

/// Artifact architecture values supported by Stone emission in this ABI.
pub const SUPPORTED_ARTIFACT_ARCHITECTURES: &[&str] = &["x86_64", "x86", "aarch64", "riscv64"];

/// A value supplied explicitly by the planner when policy is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct TuningOptionSpec {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

/// One named choice within a tuning group.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PlatformPolicySpec {
    pub architecture: String,
    pub vendor: String,
    pub operating_system: String,
    pub abi: String,
}

/// Whether a target executes natively or through one explicitly named
/// compatibility mode.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RetiredTargetPolicySpec {
    pub name: String,
    pub reason: String,
}

/// Condition under which an environment binding is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCredentialPolicySpec {
    /// Namespace user/group ID zero maps only to the invoking caller.
    IsolatedRoot,
}

/// Repository-authorized pseudo-filesystems available to a build sandbox.
///
/// Proc is unconditionally absent from frozen builds and is therefore not an
/// authored policy value. The finite modes also cannot express any `/sys`
/// mount or a full host `/dev` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct SandboxFilesystemPolicySpec {
    pub tmp: SandboxTmpPolicySpec,
    pub sys: SandboxSysPolicySpec,
    pub dev: SandboxDevPolicySpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTmpPolicySpec {
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSysPolicySpec {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[cfg(test)]
mod tests {
    use super::validation::ResourceValidator;
    use super::*;

    include!("validation/tests/resource_limits_and_patches.rs");
}
