//! Lua declaration DTOs for the build-policy domain (Phase L5, in progress).
//!
//! The build policy is pervaded by tuple/newtype enum variants (`TextSpec`,
//! `BuildToolSpec`, `ValuePatch`, …). Serde's internally-tagged `#[serde(tag =
//! "kind")]` encoding — the uniform Lua encoding every other domain uses — does
//! not support tuple variants, so those enums cannot be decoded by deriving
//! `Deserialize` on the domain type the way the build lock's struct/unit enums
//! were. This module holds the struct-variant Lua DTOs plus `From` conversions
//! that bridge that gap. It is the foundation of the full build-policy adapter;
//! the remaining spec tree is layered on top in later slices.

// The full build-policy adapter is assembled over several slices; these
// foundation DTOs are exercised by the tests below until the top-level
// evaluator that consumes them lands.
#![cfg_attr(not(test), allow(dead_code))]

use declarative_config::{Diagnostic, Source};
use lua_config::{LuaEngine, LuaOption, LuaPatch};
use serde::Deserialize;

use super::{
    AnalyzerKind, AnalyzerToolchainPolicySpec, AnalyzerToolsPolicySpec, ArrayPatch,
    BuildCommandSpec, BuildPolicyPatchSpec, BuildPolicySpec, BuildProgramSpec, BuildRootPolicySpec,
    BuildToolSpec, BuilderCommandSpec, BuildersPolicySpec, CompilerCachePolicySpec,
    CompilerFlagsSpec, CompilerToolsSpec, ContextValue, Emul32InputPolicySpec,
    EnvironmentBindingSpec, EnvironmentCondition, GitPreparationPolicySpec, InstallLayoutSpec,
    MoldPolicySpec, NamedTuningChoiceSpec, NamedTuningFlagSpec, NamedTuningGroupSpec, PgoFinishSpec,
    PgoPolicySpec, PgoStagePolicySpec, PlatformPolicySpec, RetiredTargetPolicySpec,
    SandboxPolicySpec, SourcePreparationPolicySpec, StandardBuilderPolicySpec, TargetEmulationSpec,
    TargetPolicySpec, TextSpec, ToolchainFlagsSpec, ToolchainInputPolicySpec, ToolchainsSpec,
    TuningGroupSpec, TuningOptionSpec, TuningPolicySpec, ValuePatch,
};

/// Map a `Vec` of Lua DTOs to a `Vec` of their domain values.
fn text_vec(values: Vec<LuaTextSpec>) -> Vec<TextSpec> {
    values.into_iter().map(Into::into).collect()
}

/// Map a `Vec` of Lua tool DTOs to a `Vec` of their domain values.
fn tool_vec(values: Vec<LuaBuildToolSpec>) -> Vec<BuildToolSpec> {
    values.into_iter().map(Into::into).collect()
}

/// Convert an optional Lua DTO into an optional domain value.
fn optional<L, D: From<L>>(value: LuaOption<L>) -> Option<D> {
    Option::<L>::from(value).map(Into::into)
}

/// Convert a decoded [`LuaPatch`] into the domain [`ValuePatch`], mapping the
/// `set` payload through its own `Into` conversion so patched DTO values reach
/// their domain form.
pub(crate) fn value_patch<L, D>(patch: LuaPatch<L>) -> ValuePatch<D>
where
    D: From<L>,
{
    match patch {
        LuaPatch::Keep => ValuePatch::Keep,
        LuaPatch::Set { value } => ValuePatch::Set(value.into()),
    }
}

/// The Lua encoding of an [`ArrayPatch`]: `{ kind = "keep" }` or a
/// `{ kind = "replace" | "prepend" | "append", values = { … } }` overlay.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaArrayPatch<T> {
    Keep,
    Replace { values: Vec<T> },
    Prepend { values: Vec<T> },
    Append { values: Vec<T> },
}

/// Convert a decoded [`LuaArrayPatch`] into the domain [`ArrayPatch`], mapping
/// each element through its own `Into` conversion.
pub(crate) fn array_patch<L, D>(patch: LuaArrayPatch<L>) -> ArrayPatch<D>
where
    D: From<L>,
{
    let convert = |values: Vec<L>| values.into_iter().map(Into::into).collect();
    match patch {
        LuaArrayPatch::Keep => ArrayPatch::Keep,
        LuaArrayPatch::Replace { values } => ArrayPatch::Replace(convert(values)),
        LuaArrayPatch::Prepend { values } => ArrayPatch::Prepend(convert(values)),
        LuaArrayPatch::Append { values } => ArrayPatch::Append(convert(values)),
    }
}

/// The Lua encoding of a [`TextSpec`]. The domain enum's tuple variants become
/// struct variants so the uniform `{ kind = … }` tag applies; `Context` reuses
/// the all-unit [`ContextValue`], which decodes directly from its snake_case
/// name.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaTextSpec {
    Literal { value: String },
    Context { value: ContextValue },
    Concat { values: Vec<LuaTextSpec> },
}

impl From<LuaTextSpec> for TextSpec {
    fn from(text: LuaTextSpec) -> Self {
        match text {
            LuaTextSpec::Literal { value } => Self::Literal(value),
            LuaTextSpec::Context { value } => Self::Context(value),
            LuaTextSpec::Concat { values } => {
                Self::Concat(values.into_iter().map(Into::into).collect())
            }
        }
    }
}

/// The Lua encoding of a [`BuildToolSpec`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaBuildToolSpec {
    Package { value: String },
    Binary { value: String },
    SystemBinary { value: String },
}

impl From<LuaBuildToolSpec> for BuildToolSpec {
    fn from(tool: LuaBuildToolSpec) -> Self {
        match tool {
            LuaBuildToolSpec::Package { value } => Self::Package(value),
            LuaBuildToolSpec::Binary { value } => Self::Binary(value),
            LuaBuildToolSpec::SystemBinary { value } => Self::SystemBinary(value),
        }
    }
}

/// The Lua encoding of a [`CompilerFlagsSpec`] — eight ordered flag lists, each
/// element a [`TextSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaCompilerFlagsSpec {
    pub c: Vec<LuaTextSpec>,
    pub cxx: Vec<LuaTextSpec>,
    pub f: Vec<LuaTextSpec>,
    pub d: Vec<LuaTextSpec>,
    pub rust: Vec<LuaTextSpec>,
    pub vala: Vec<LuaTextSpec>,
    pub go: Vec<LuaTextSpec>,
    pub ld: Vec<LuaTextSpec>,
}

impl From<LuaCompilerFlagsSpec> for CompilerFlagsSpec {
    fn from(flags: LuaCompilerFlagsSpec) -> Self {
        Self {
            c: text_vec(flags.c),
            cxx: text_vec(flags.cxx),
            f: text_vec(flags.f),
            d: text_vec(flags.d),
            rust: text_vec(flags.rust),
            vala: text_vec(flags.vala),
            go: text_vec(flags.go),
            ld: text_vec(flags.ld),
        }
    }
}

/// The Lua encoding of an [`InstallLayoutSpec`] — the canonical install
/// directory locators, each a [`TextSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaInstallLayoutSpec {
    pub prefix: LuaTextSpec,
    pub bindir: LuaTextSpec,
    pub sbindir: LuaTextSpec,
    pub includedir: LuaTextSpec,
    pub libdir: LuaTextSpec,
    pub libexecdir: LuaTextSpec,
    pub datadir: LuaTextSpec,
    pub vendordir: LuaTextSpec,
    pub docdir: LuaTextSpec,
    pub infodir: LuaTextSpec,
    pub localedir: LuaTextSpec,
    pub mandir: LuaTextSpec,
    pub sysconfdir: LuaTextSpec,
    pub localstatedir: LuaTextSpec,
    pub sharedstatedir: LuaTextSpec,
    pub runstatedir: LuaTextSpec,
    pub sysusersdir: LuaTextSpec,
    pub tmpfilesdir: LuaTextSpec,
    pub udevrulesdir: LuaTextSpec,
    pub bash_completions_dir: LuaTextSpec,
    pub fish_completions_dir: LuaTextSpec,
    pub elvish_completions_dir: LuaTextSpec,
    pub zsh_completions_dir: LuaTextSpec,
}

impl From<LuaInstallLayoutSpec> for InstallLayoutSpec {
    fn from(layout: LuaInstallLayoutSpec) -> Self {
        Self {
            prefix: layout.prefix.into(),
            bindir: layout.bindir.into(),
            sbindir: layout.sbindir.into(),
            includedir: layout.includedir.into(),
            libdir: layout.libdir.into(),
            libexecdir: layout.libexecdir.into(),
            datadir: layout.datadir.into(),
            vendordir: layout.vendordir.into(),
            docdir: layout.docdir.into(),
            infodir: layout.infodir.into(),
            localedir: layout.localedir.into(),
            mandir: layout.mandir.into(),
            sysconfdir: layout.sysconfdir.into(),
            localstatedir: layout.localstatedir.into(),
            sharedstatedir: layout.sharedstatedir.into(),
            runstatedir: layout.runstatedir.into(),
            sysusersdir: layout.sysusersdir.into(),
            tmpfilesdir: layout.tmpfilesdir.into(),
            udevrulesdir: layout.udevrulesdir.into(),
            bash_completions_dir: layout.bash_completions_dir.into(),
            fish_completions_dir: layout.fish_completions_dir.into(),
            elvish_completions_dir: layout.elvish_completions_dir.into(),
            zsh_completions_dir: layout.zsh_completions_dir.into(),
        }
    }
}

/// The Lua encoding of a [`BuildProgramSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildProgramSpec {
    pub path: String,
    pub requirement: LuaBuildToolSpec,
}

impl From<LuaBuildProgramSpec> for BuildProgramSpec {
    fn from(program: LuaBuildProgramSpec) -> Self {
        Self {
            path: program.path,
            requirement: program.requirement.into(),
        }
    }
}

/// The Lua encoding of a [`BuildCommandSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildCommandSpec {
    pub program: LuaBuildProgramSpec,
    pub args: Vec<String>,
}

impl From<LuaBuildCommandSpec> for BuildCommandSpec {
    fn from(command: LuaBuildCommandSpec) -> Self {
        Self {
            program: command.program.into(),
            args: command.args,
        }
    }
}

/// The Lua encoding of a [`CompilerToolsSpec`] — one build command per toolchain
/// executable role.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaCompilerToolsSpec {
    pub cc: LuaBuildCommandSpec,
    pub cxx: LuaBuildCommandSpec,
    pub objc: LuaBuildCommandSpec,
    pub objcxx: LuaBuildCommandSpec,
    pub cpp: LuaBuildCommandSpec,
    pub objcpp: LuaBuildCommandSpec,
    pub objcxxcpp: LuaBuildCommandSpec,
    pub ar: LuaBuildCommandSpec,
    pub ld: LuaBuildCommandSpec,
    pub objcopy: LuaBuildCommandSpec,
    pub nm: LuaBuildCommandSpec,
    pub ranlib: LuaBuildCommandSpec,
    pub strip: LuaBuildCommandSpec,
}

impl From<LuaCompilerToolsSpec> for CompilerToolsSpec {
    fn from(tools: LuaCompilerToolsSpec) -> Self {
        Self {
            cc: tools.cc.into(),
            cxx: tools.cxx.into(),
            objc: tools.objc.into(),
            objcxx: tools.objcxx.into(),
            cpp: tools.cpp.into(),
            objcpp: tools.objcpp.into(),
            objcxxcpp: tools.objcxxcpp.into(),
            ar: tools.ar.into(),
            ld: tools.ld.into(),
            objcopy: tools.objcopy.into(),
            nm: tools.nm.into(),
            ranlib: tools.ranlib.into(),
            strip: tools.strip.into(),
        }
    }
}

/// The Lua encoding of a [`ToolchainsSpec`] — the LLVM and GNU tool tables.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaToolchainsSpec {
    pub llvm: LuaCompilerToolsSpec,
    pub gnu: LuaCompilerToolsSpec,
}

impl From<LuaToolchainsSpec> for ToolchainsSpec {
    fn from(toolchains: LuaToolchainsSpec) -> Self {
        Self {
            llvm: toolchains.llvm.into(),
            gnu: toolchains.gnu.into(),
        }
    }
}

/// The Lua encoding of a [`ToolchainFlagsSpec`] — common/GNU/LLVM flag sets.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaToolchainFlagsSpec {
    pub common: LuaCompilerFlagsSpec,
    pub gnu: LuaCompilerFlagsSpec,
    pub llvm: LuaCompilerFlagsSpec,
}

impl From<LuaToolchainFlagsSpec> for ToolchainFlagsSpec {
    fn from(flags: LuaToolchainFlagsSpec) -> Self {
        Self {
            common: flags.common.into(),
            gnu: flags.gnu.into(),
            llvm: flags.llvm.into(),
        }
    }
}

/// The Lua encoding of an [`EnvironmentBindingSpec`]. `condition` reuses the
/// all-unit [`EnvironmentCondition`], which decodes from its snake_case name.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaEnvironmentBindingSpec {
    pub name: String,
    pub value: LuaTextSpec,
    pub condition: EnvironmentCondition,
}

impl From<LuaEnvironmentBindingSpec> for EnvironmentBindingSpec {
    fn from(binding: LuaEnvironmentBindingSpec) -> Self {
        Self {
            name: binding.name,
            value: binding.value.into(),
            condition: binding.condition,
        }
    }
}

/// The Lua encoding of a [`TargetPolicySpec`]. The triple/platform fields are
/// plain data; emulation and the platforms decode directly on their domain
/// types, while the architecture flags and environment reuse the Lua wrappers.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaTargetPolicySpec {
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
    pub architecture_flags: LuaToolchainFlagsSpec,
    pub environment: Vec<LuaEnvironmentBindingSpec>,
}

impl From<LuaTargetPolicySpec> for TargetPolicySpec {
    fn from(target: LuaTargetPolicySpec) -> Self {
        Self {
            name: target.name,
            target_triple: target.target_triple,
            build_triple: target.build_triple,
            host_triple: target.host_triple,
            lib_suffix: target.lib_suffix,
            artifact_architecture: target.artifact_architecture,
            emulation: target.emulation,
            build_platform: target.build_platform,
            host_platform: target.host_platform,
            target_platform: target.target_platform,
            architecture_flags: target.architecture_flags.into(),
            environment: target.environment.into_iter().map(Into::into).collect(),
        }
    }
}

/// The Lua encoding of a [`ToolchainInputPolicySpec`] — per-toolchain build-root
/// tool inputs.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaToolchainInputPolicySpec {
    pub llvm: Vec<LuaBuildToolSpec>,
    pub gnu: Vec<LuaBuildToolSpec>,
}

impl From<LuaToolchainInputPolicySpec> for ToolchainInputPolicySpec {
    fn from(inputs: LuaToolchainInputPolicySpec) -> Self {
        Self {
            llvm: tool_vec(inputs.llvm),
            gnu: tool_vec(inputs.gnu),
        }
    }
}

/// The Lua encoding of an [`Emul32InputPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaEmul32InputPolicySpec {
    pub base: Vec<LuaBuildToolSpec>,
    pub toolchains: LuaToolchainInputPolicySpec,
}

impl From<LuaEmul32InputPolicySpec> for Emul32InputPolicySpec {
    fn from(inputs: LuaEmul32InputPolicySpec) -> Self {
        Self {
            base: tool_vec(inputs.base),
            toolchains: inputs.toolchains.into(),
        }
    }
}

/// The Lua encoding of a [`BuilderCommandSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuilderCommandSpec {
    pub program: LuaBuildProgramSpec,
    pub args: Vec<LuaTextSpec>,
    pub environment: Vec<LuaEnvironmentBindingSpec>,
    pub working_dir: LuaTextSpec,
}

impl From<LuaBuilderCommandSpec> for BuilderCommandSpec {
    fn from(command: LuaBuilderCommandSpec) -> Self {
        Self {
            program: command.program.into(),
            args: text_vec(command.args),
            environment: command.environment.into_iter().map(Into::into).collect(),
            working_dir: command.working_dir.into(),
        }
    }
}

/// The Lua encoding of an [`AnalyzerToolchainPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaAnalyzerToolchainPolicySpec {
    pub objcopy: LuaBuildToolSpec,
    pub strip: LuaBuildToolSpec,
}

impl From<LuaAnalyzerToolchainPolicySpec> for AnalyzerToolchainPolicySpec {
    fn from(tools: LuaAnalyzerToolchainPolicySpec) -> Self {
        Self {
            objcopy: tools.objcopy.into(),
            strip: tools.strip.into(),
        }
    }
}

/// The Lua encoding of an [`AnalyzerToolsPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaAnalyzerToolsPolicySpec {
    pub pkg_config: LuaBuildToolSpec,
    pub python: LuaBuildToolSpec,
    pub llvm: LuaAnalyzerToolchainPolicySpec,
    pub gnu: LuaAnalyzerToolchainPolicySpec,
}

impl From<LuaAnalyzerToolsPolicySpec> for AnalyzerToolsPolicySpec {
    fn from(tools: LuaAnalyzerToolsPolicySpec) -> Self {
        Self {
            pkg_config: tools.pkg_config.into(),
            python: tools.python.into(),
            llvm: tools.llvm.into(),
            gnu: tools.gnu.into(),
        }
    }
}

/// The Lua encoding of a [`CompilerCachePolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaCompilerCachePolicySpec {
    pub ccache: LuaBuildProgramSpec,
    pub sccache: LuaBuildProgramSpec,
    pub ccache_dir: String,
    pub sccache_dir: String,
    pub go_cache_dir: String,
    pub go_mod_cache_dir: String,
    pub cargo_cache_dir: String,
    pub zig_cache_dir: String,
}

impl From<LuaCompilerCachePolicySpec> for CompilerCachePolicySpec {
    fn from(cache: LuaCompilerCachePolicySpec) -> Self {
        Self {
            ccache: cache.ccache.into(),
            sccache: cache.sccache.into(),
            ccache_dir: cache.ccache_dir,
            sccache_dir: cache.sccache_dir,
            go_cache_dir: cache.go_cache_dir,
            go_mod_cache_dir: cache.go_mod_cache_dir,
            cargo_cache_dir: cache.cargo_cache_dir,
            zig_cache_dir: cache.zig_cache_dir,
        }
    }
}

/// The Lua encoding of a [`MoldPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaMoldPolicySpec {
    pub linker: LuaBuildCommandSpec,
    pub flags: LuaCompilerFlagsSpec,
}

impl From<LuaMoldPolicySpec> for MoldPolicySpec {
    fn from(mold: LuaMoldPolicySpec) -> Self {
        Self {
            linker: mold.linker.into(),
            flags: mold.flags.into(),
        }
    }
}

/// The Lua encoding of a [`BuildRootPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildRootPolicySpec {
    pub base: Vec<LuaBuildToolSpec>,
    pub toolchains: LuaToolchainInputPolicySpec,
    pub emul32: LuaEmul32InputPolicySpec,
    pub analyzer_tools: LuaAnalyzerToolsPolicySpec,
    pub compiler_cache: LuaCompilerCachePolicySpec,
    pub mold: LuaMoldPolicySpec,
}

impl From<LuaBuildRootPolicySpec> for BuildRootPolicySpec {
    fn from(root: LuaBuildRootPolicySpec) -> Self {
        Self {
            base: tool_vec(root.base),
            toolchains: root.toolchains.into(),
            emul32: root.emul32.into(),
            analyzer_tools: root.analyzer_tools.into(),
            compiler_cache: root.compiler_cache.into(),
            mold: root.mold.into(),
        }
    }
}

/// The Lua encoding of a [`GitPreparationPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaGitPreparationPolicySpec {
    pub create_directory: LuaBuilderCommandSpec,
    pub copy: LuaBuilderCommandSpec,
}

impl From<LuaGitPreparationPolicySpec> for GitPreparationPolicySpec {
    fn from(git: LuaGitPreparationPolicySpec) -> Self {
        Self {
            create_directory: git.create_directory.into(),
            copy: git.copy.into(),
        }
    }
}

/// The Lua encoding of a [`SourcePreparationPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaSourcePreparationPolicySpec {
    pub git: LuaGitPreparationPolicySpec,
}

impl From<LuaSourcePreparationPolicySpec> for SourcePreparationPolicySpec {
    fn from(sources: LuaSourcePreparationPolicySpec) -> Self {
        Self {
            git: sources.git.into(),
        }
    }
}

/// The Lua encoding of a [`PgoFinishSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaPgoFinishSpec {
    pub output: LuaTextSpec,
    pub inputs: Vec<LuaTextSpec>,
    pub copy_to: LuaOption<LuaTextSpec>,
    pub remove_output_first: bool,
}

impl From<LuaPgoFinishSpec> for PgoFinishSpec {
    fn from(finish: LuaPgoFinishSpec) -> Self {
        Self {
            output: finish.output.into(),
            inputs: text_vec(finish.inputs),
            copy_to: optional(finish.copy_to),
            remove_output_first: finish.remove_output_first,
        }
    }
}

/// The Lua encoding of a [`PgoStagePolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaPgoStagePolicySpec {
    pub flags: LuaToolchainFlagsSpec,
    pub finish: LuaOption<LuaPgoFinishSpec>,
}

impl From<LuaPgoStagePolicySpec> for PgoStagePolicySpec {
    fn from(stage: LuaPgoStagePolicySpec) -> Self {
        Self {
            flags: stage.flags.into(),
            finish: optional(stage.finish),
        }
    }
}

/// The Lua encoding of a [`TuningGroupSpec`]. `base` and `choices` are pure and
/// decode directly; `default` uses the tagged option encoding.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaTuningGroupSpec {
    pub base: TuningOptionSpec,
    pub default: LuaOption<String>,
    pub choices: Vec<NamedTuningChoiceSpec>,
}

impl From<LuaTuningGroupSpec> for TuningGroupSpec {
    fn from(group: LuaTuningGroupSpec) -> Self {
        Self {
            base: group.base,
            default: Option::<String>::from(group.default),
            choices: group.choices,
        }
    }
}

/// The Lua encoding of a [`NamedTuningGroupSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaNamedTuningGroupSpec {
    pub name: String,
    pub value: LuaTuningGroupSpec,
}

impl From<LuaNamedTuningGroupSpec> for NamedTuningGroupSpec {
    fn from(group: LuaNamedTuningGroupSpec) -> Self {
        Self {
            name: group.name,
            value: group.value.into(),
        }
    }
}

/// The Lua encoding of a [`NamedTuningFlagSpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaNamedTuningFlagSpec {
    pub name: String,
    pub value: LuaToolchainFlagsSpec,
}

impl From<LuaNamedTuningFlagSpec> for NamedTuningFlagSpec {
    fn from(flag: LuaNamedTuningFlagSpec) -> Self {
        Self {
            name: flag.name,
            value: flag.value.into(),
        }
    }
}

/// The Lua encoding of a [`TuningPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaTuningPolicySpec {
    pub flags: Vec<LuaNamedTuningFlagSpec>,
    pub groups: Vec<LuaNamedTuningGroupSpec>,
    pub default_groups: Vec<String>,
}

impl From<LuaTuningPolicySpec> for TuningPolicySpec {
    fn from(tuning: LuaTuningPolicySpec) -> Self {
        Self {
            flags: tuning.flags.into_iter().map(Into::into).collect(),
            groups: tuning.groups.into_iter().map(Into::into).collect(),
            default_groups: tuning.default_groups,
        }
    }
}

/// The Lua encoding of a [`StandardBuilderPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaStandardBuilderPolicySpec {
    pub environment: Vec<LuaEnvironmentBindingSpec>,
    pub setup: LuaBuilderCommandSpec,
    pub build: LuaBuilderCommandSpec,
    pub install: LuaBuilderCommandSpec,
    pub check: LuaBuilderCommandSpec,
}

impl From<LuaStandardBuilderPolicySpec> for StandardBuilderPolicySpec {
    fn from(builder: LuaStandardBuilderPolicySpec) -> Self {
        Self {
            environment: builder.environment.into_iter().map(Into::into).collect(),
            setup: builder.setup.into(),
            build: builder.build.into(),
            install: builder.install.into(),
            check: builder.check.into(),
        }
    }
}

/// The Lua encoding of a [`BuildersPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildersPolicySpec {
    pub cmake: LuaStandardBuilderPolicySpec,
    pub meson: LuaStandardBuilderPolicySpec,
    pub cargo: LuaStandardBuilderPolicySpec,
    pub autotools: LuaStandardBuilderPolicySpec,
}

impl From<LuaBuildersPolicySpec> for BuildersPolicySpec {
    fn from(builders: LuaBuildersPolicySpec) -> Self {
        Self {
            cmake: builders.cmake.into(),
            meson: builders.meson.into(),
            cargo: builders.cargo.into(),
            autotools: builders.autotools.into(),
        }
    }
}

/// The Lua encoding of a [`PgoPolicySpec`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaPgoPolicySpec {
    pub shell_interpreter: LuaBuildProgramSpec,
    pub merge_program: LuaBuildProgramSpec,
    pub merge_args: Vec<LuaTextSpec>,
    pub copy_program: LuaBuildProgramSpec,
    pub remove_program: LuaBuildProgramSpec,
    pub sample: LuaToolchainFlagsSpec,
    pub stage_one: LuaPgoStagePolicySpec,
    pub stage_two: LuaPgoStagePolicySpec,
    pub use_profile: LuaPgoStagePolicySpec,
}

impl From<LuaPgoPolicySpec> for PgoPolicySpec {
    fn from(pgo: LuaPgoPolicySpec) -> Self {
        Self {
            shell_interpreter: pgo.shell_interpreter.into(),
            merge_program: pgo.merge_program.into(),
            merge_args: text_vec(pgo.merge_args),
            copy_program: pgo.copy_program.into(),
            remove_program: pgo.remove_program.into(),
            sample: pgo.sample.into(),
            stage_one: pgo.stage_one.into(),
            stage_two: pgo.stage_two.into(),
            use_profile: pgo.use_profile.into(),
        }
    }
}

/// The Lua encoding of a complete [`BuildPolicySpec`]. Pure fields
/// (`retired_targets`, `sandbox`, `analyzers`) decode directly; the rest use the
/// sub-spec DTOs above.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildPolicySpec {
    pub build_subdir: String,
    pub layout: LuaInstallLayoutSpec,
    pub toolchains: LuaToolchainsSpec,
    pub targets: Vec<LuaTargetPolicySpec>,
    pub retired_targets: Vec<RetiredTargetPolicySpec>,
    pub sandbox: SandboxPolicySpec,
    pub build_root: LuaBuildRootPolicySpec,
    pub sources: LuaSourcePreparationPolicySpec,
    pub tuning: LuaTuningPolicySpec,
    pub environment: Vec<LuaEnvironmentBindingSpec>,
    pub builders: LuaBuildersPolicySpec,
    pub analyzers: Vec<AnalyzerKind>,
    pub pgo: LuaPgoPolicySpec,
}

impl From<LuaBuildPolicySpec> for BuildPolicySpec {
    fn from(policy: LuaBuildPolicySpec) -> Self {
        Self {
            build_subdir: policy.build_subdir,
            layout: policy.layout.into(),
            toolchains: policy.toolchains.into(),
            targets: policy.targets.into_iter().map(Into::into).collect(),
            retired_targets: policy.retired_targets,
            sandbox: policy.sandbox,
            build_root: policy.build_root.into(),
            sources: policy.sources.into(),
            tuning: policy.tuning.into(),
            environment: policy.environment.into_iter().map(Into::into).collect(),
            builders: policy.builders.into(),
            analyzers: policy.analyzers,
            pgo: policy.pgo.into(),
        }
    }
}

/// The Lua encoding of a [`BuildPolicyPatchSpec`] — a sparse overlay where every
/// field is a keep/set (or keep/replace/prepend/append) operation.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaBuildPolicyPatchSpec {
    pub build_subdir: LuaPatch<String>,
    pub layout: LuaPatch<LuaInstallLayoutSpec>,
    pub toolchains: LuaPatch<LuaToolchainsSpec>,
    pub targets: LuaArrayPatch<LuaTargetPolicySpec>,
    pub retired_targets: LuaArrayPatch<RetiredTargetPolicySpec>,
    pub sandbox: LuaPatch<SandboxPolicySpec>,
    pub build_root: LuaPatch<LuaBuildRootPolicySpec>,
    pub sources: LuaPatch<LuaSourcePreparationPolicySpec>,
    pub tuning: LuaPatch<LuaTuningPolicySpec>,
    pub environment: LuaArrayPatch<LuaEnvironmentBindingSpec>,
    pub builders: LuaPatch<LuaBuildersPolicySpec>,
    pub analyzers: LuaArrayPatch<AnalyzerKind>,
    pub pgo: LuaPatch<LuaPgoPolicySpec>,
}

impl From<LuaBuildPolicyPatchSpec> for BuildPolicyPatchSpec {
    fn from(patch: LuaBuildPolicyPatchSpec) -> Self {
        Self {
            build_subdir: value_patch(patch.build_subdir),
            layout: value_patch(patch.layout),
            toolchains: value_patch(patch.toolchains),
            targets: array_patch(patch.targets),
            retired_targets: array_patch(patch.retired_targets),
            sandbox: value_patch(patch.sandbox),
            build_root: value_patch(patch.build_root),
            sources: value_patch(patch.sources),
            tuning: value_patch(patch.tuning),
            environment: array_patch(patch.environment),
            builders: value_patch(patch.builders),
            analyzers: array_patch(patch.analyzers),
            pgo: value_patch(patch.pgo),
        }
    }
}

/// Stateless Lua adapter for the build-policy declaration and its patch overlay.
#[derive(Debug, Clone, Default)]
pub(crate) struct LuaBuildPolicyEvaluator {
    engine: LuaEngine,
}

impl LuaBuildPolicyEvaluator {
    /// Decode a complete authored build policy.
    pub(crate) fn evaluate(&self, source: &Source) -> Result<BuildPolicySpec, Diagnostic> {
        Ok(self.engine.evaluate_as::<LuaBuildPolicySpec>(source)?.value.into())
    }

    /// Decode a sparse build-policy patch overlay.
    pub(crate) fn evaluate_patch(&self, source: &Source) -> Result<BuildPolicyPatchSpec, Diagnostic> {
        Ok(self.engine.evaluate_as::<LuaBuildPolicyPatchSpec>(source)?.value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::SandboxDevPolicySpec;
    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("build-policy.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn a_literal_text_spec_decodes() {
        let text: TextSpec = decode::<LuaTextSpec>(r#"return { kind = "literal", value = "cc" }"#).into();
        assert_eq!(text, TextSpec::Literal("cc".to_owned()));
    }

    #[test]
    fn a_context_text_spec_decodes_the_unit_context_value() {
        let text: TextSpec =
            decode::<LuaTextSpec>(r#"return { kind = "context", value = "package_name" }"#).into();
        assert_eq!(text, TextSpec::Context(ContextValue::PackageName));
    }

    #[test]
    fn a_nested_concat_text_spec_decodes_recursively() {
        let source = r#"
return {
    kind = "concat",
    values = {
        { kind = "literal", value = "lib" },
        { kind = "context", value = "lib_suffix" },
    },
}
"#;
        let text: TextSpec = decode::<LuaTextSpec>(source).into();
        assert_eq!(
            text,
            TextSpec::Concat(vec![
                TextSpec::Literal("lib".to_owned()),
                TextSpec::Context(ContextValue::LibSuffix),
            ])
        );
    }

    #[test]
    fn build_tool_variants_decode() {
        let package: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "package", value = "cmake" }"#).into();
        assert_eq!(package, BuildToolSpec::Package("cmake".to_owned()));

        let system: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "system_binary", value = "/bin/sh" }"#)
                .into();
        assert_eq!(system, BuildToolSpec::SystemBinary("/bin/sh".to_owned()));
    }

    #[test]
    fn a_value_patch_keeps_or_sets_the_converted_payload() {
        let keep = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ValuePatch::Keep);

        let set = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "set", value = { kind = "binary", value = "meson" } }"#,
        ));
        assert_eq!(set, ValuePatch::Set(BuildToolSpec::Binary("meson".to_owned())));
    }

    #[test]
    fn an_array_patch_maps_every_operation_and_element() {
        let keep = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ArrayPatch::Keep);

        let append = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "append", values = { { kind = "package", value = "ninja" } } }"#,
        ));
        assert_eq!(
            append,
            ArrayPatch::Append(vec![BuildToolSpec::Package("ninja".to_owned())])
        );
    }

    #[test]
    fn compiler_flags_decode_with_empty_and_populated_lists() {
        let source = r#"
return {
    c = { { kind = "literal", value = "-Wall" } },
    cxx = {},
    f = {},
    d = {},
    rust = {},
    vala = {},
    go = {},
    ld = { { kind = "context", value = "ld_flags" } },
}
"#;
        let flags: CompilerFlagsSpec = decode::<LuaCompilerFlagsSpec>(source).into();
        assert_eq!(flags.c, vec![TextSpec::Literal("-Wall".to_owned())]);
        assert!(flags.cxx.is_empty());
        assert_eq!(flags.ld, vec![TextSpec::Context(ContextValue::LdFlags)]);
    }

    #[test]
    fn a_build_command_decodes_program_requirement_and_args() {
        let source = r#"
return {
    program = {
        path = "/usr/bin/cc",
        requirement = { kind = "package", value = "llvm" },
    },
    args = { "-fPIC", "-O2" },
}
"#;
        let command: BuildCommandSpec = decode::<LuaBuildCommandSpec>(source).into();
        assert_eq!(command.program.path, "/usr/bin/cc");
        assert_eq!(command.program.requirement, BuildToolSpec::Package("llvm".to_owned()));
        assert_eq!(command.args, vec!["-fPIC".to_owned(), "-O2".to_owned()]);
    }

    #[test]
    fn pure_target_types_decode_directly_on_the_domain_type() {
        let platform: PlatformPolicySpec = decode(
            r#"return { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" }"#,
        );
        assert_eq!(platform.architecture, "x86_64");

        let native: TargetEmulationSpec = decode(r#"return { kind = "native" }"#);
        assert_eq!(native, TargetEmulationSpec::Native);

        let emul32: TargetEmulationSpec =
            decode(r#"return { kind = "emul32", host_architecture = "x86_64" }"#);
        assert_eq!(
            emul32,
            TargetEmulationSpec::Emul32 { host_architecture: "x86_64".to_owned() }
        );

        let condition: EnvironmentCondition = decode(r#"return "compiler_cache_enabled""#);
        assert_eq!(condition, EnvironmentCondition::CompilerCacheEnabled);
    }

    #[test]
    fn an_environment_binding_decodes_value_and_condition() {
        let source = r#"
return {
    name = "CFLAGS",
    value = { kind = "context", value = "c_flags" },
    condition = "always",
}
"#;
        let binding: EnvironmentBindingSpec = decode::<LuaEnvironmentBindingSpec>(source).into();
        assert_eq!(binding.name, "CFLAGS");
        assert_eq!(binding.value, TextSpec::Context(ContextValue::CFlags));
        assert_eq!(binding.condition, EnvironmentCondition::Always);
    }

    #[test]
    fn the_sandbox_policy_decodes_directly_as_pure_data() {
        let source = r#"
return {
    hostname = "builder",
    credentials = "isolated_root",
    filesystems = { tmp = "empty", sys = "none", dev = "minimal" },
    guest_root = "/mason",
    artifacts_dir = "/mason/artifacts",
    build_dir = "/mason/build",
    source_dir = "/mason/source",
    recipe_dir = "/mason/recipe",
    package_dir = "/mason/package",
    install_dir = "/mason/install",
}
"#;
        let sandbox: SandboxPolicySpec = decode(source);
        assert_eq!(sandbox.hostname, "builder");
        assert_eq!(sandbox.filesystems.dev, SandboxDevPolicySpec::Minimal);
    }

    #[test]
    fn toolchain_input_tools_decode_through_the_wrapper() {
        let source = r#"
return {
    llvm = { { kind = "package", value = "clang" } },
    gnu = {},
}
"#;
        let inputs: ToolchainInputPolicySpec = decode::<LuaToolchainInputPolicySpec>(source).into();
        assert_eq!(inputs.llvm, vec![BuildToolSpec::Package("clang".to_owned())]);
        assert!(inputs.gnu.is_empty());
    }

    #[test]
    fn a_pgo_finish_decodes_optional_and_list_fields() {
        let with_copy = r#"
return {
    output = { kind = "literal", value = "merged.profdata" },
    inputs = { { kind = "literal", value = "a.profraw" }, { kind = "literal", value = "b.profraw" } },
    copy_to = { kind = "some", value = { kind = "literal", value = "final.profdata" } },
    remove_output_first = true,
}
"#;
        let finish: PgoFinishSpec = decode::<LuaPgoFinishSpec>(with_copy).into();
        assert_eq!(finish.output, TextSpec::Literal("merged.profdata".to_owned()));
        assert_eq!(finish.inputs.len(), 2);
        assert_eq!(finish.copy_to, Some(TextSpec::Literal("final.profdata".to_owned())));
        assert!(finish.remove_output_first);

        let without_copy = with_copy.replace(
            r#"copy_to = { kind = "some", value = { kind = "literal", value = "final.profdata" } },"#,
            r#"copy_to = { kind = "none" },"#,
        );
        let bare: PgoFinishSpec = decode::<LuaPgoFinishSpec>(&without_copy).into();
        assert_eq!(bare.copy_to, None);
    }

    #[test]
    fn analyzer_tools_decode_through_the_wrapper() {
        let source = r#"
return {
    pkg_config = { kind = "binary", value = "pkg-config" },
    python = { kind = "package", value = "python" },
    llvm = {
        objcopy = { kind = "binary", value = "llvm-objcopy" },
        strip = { kind = "binary", value = "llvm-strip" },
    },
    gnu = {
        objcopy = { kind = "binary", value = "objcopy" },
        strip = { kind = "binary", value = "strip" },
    },
}
"#;
        let tools: AnalyzerToolsPolicySpec = decode::<LuaAnalyzerToolsPolicySpec>(source).into();
        assert_eq!(tools.python, BuildToolSpec::Package("python".to_owned()));
        assert_eq!(tools.llvm.objcopy, BuildToolSpec::Binary("llvm-objcopy".to_owned()));
    }

    #[test]
    fn a_tuning_group_decodes_pure_choices_and_a_tagged_default() {
        let source = r#"
return {
    base = { enabled = { "lto" }, disabled = {} },
    default = { kind = "some", value = "balanced" },
    choices = {
        { name = "balanced", value = { enabled = { "o2" }, disabled = { "o3" } } },
    },
}
"#;
        let group: TuningGroupSpec = decode::<LuaTuningGroupSpec>(source).into();
        assert_eq!(group.base.enabled, vec!["lto".to_owned()]);
        assert_eq!(group.default, Some("balanced".to_owned()));
        assert_eq!(group.choices.len(), 1);
        assert_eq!(group.choices[0].name, "balanced");
    }

    // A complete policy is a large authored surface; rather than hand-write
    // ~250 lines of Lua, these Rust helpers assemble a minimal-but-complete
    // source (the profile forbids Lua-side helper functions, so the repetition
    // is generated here instead).
    fn lit(value: &str) -> String {
        format!(r#"{{ kind = "literal", value = "{value}" }}"#)
    }
    fn program() -> String {
        r#"{ path = "/bin/tool", requirement = { kind = "package", value = "t" } }"#.to_owned()
    }
    fn command() -> String {
        format!("{{ program = {}, args = {{}} }}", program())
    }
    fn builder_command() -> String {
        format!(
            "{{ program = {}, args = {{}}, environment = {{}}, working_dir = {} }}",
            program(),
            lit("/work")
        )
    }
    fn flags() -> String {
        "{ c = {}, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} }".to_owned()
    }
    fn toolchain_flags() -> String {
        format!("{{ common = {f}, gnu = {f}, llvm = {f} }}", f = flags())
    }
    fn compiler_tools() -> String {
        let roles = [
            "cc", "cxx", "objc", "objcxx", "cpp", "objcpp", "objcxxcpp", "ar", "ld", "objcopy",
            "nm", "ranlib", "strip",
        ];
        let body = roles.iter().map(|r| format!("{r} = {}", command())).collect::<Vec<_>>().join(", ");
        format!("{{ {body} }}")
    }
    fn tool() -> String {
        r#"{ kind = "package", value = "t" }"#.to_owned()
    }
    fn standard_builder() -> String {
        format!(
            "{{ environment = {{}}, setup = {c}, build = {c}, install = {c}, check = {c} }}",
            c = builder_command()
        )
    }
    fn stage() -> String {
        format!("{{ flags = {}, finish = {{ kind = \"none\" }} }}", toolchain_flags())
    }

    fn complete_policy_source() -> String {
        let layout_fields = [
            "prefix", "bindir", "sbindir", "includedir", "libdir", "libexecdir", "datadir",
            "vendordir", "docdir", "infodir", "localedir", "mandir", "sysconfdir", "localstatedir",
            "sharedstatedir", "runstatedir", "sysusersdir", "tmpfilesdir", "udevrulesdir",
            "bash_completions_dir", "fish_completions_dir", "elvish_completions_dir",
            "zsh_completions_dir",
        ];
        let layout = layout_fields
            .iter()
            .map(|f| format!("{f} = {}", lit(&format!("/{f}"))))
            .collect::<Vec<_>>()
            .join(", ");
        let toolchains = format!("{{ llvm = {t}, gnu = {t} }}", t = compiler_tools());
        let toolchain_inputs = "{ llvm = {}, gnu = {} }";
        let sandbox = r#"{
            hostname = "builder",
            credentials = "isolated_root",
            filesystems = { tmp = "empty", sys = "none", dev = "minimal" },
            guest_root = "/mason", artifacts_dir = "/a", build_dir = "/b", source_dir = "/s",
            recipe_dir = "/r", package_dir = "/p", install_dir = "/i"
        }"#;
        let build_root = format!(
            "{{ base = {{}}, toolchains = {ti}, emul32 = {{ base = {{}}, toolchains = {ti} }}, \
             analyzer_tools = {{ pkg_config = {t}, python = {t}, \
             llvm = {{ objcopy = {t}, strip = {t} }}, gnu = {{ objcopy = {t}, strip = {t} }} }}, \
             compiler_cache = {{ ccache = {p}, sccache = {p}, ccache_dir = \"/c\", sccache_dir = \"/c\", \
             go_cache_dir = \"/c\", go_mod_cache_dir = \"/c\", cargo_cache_dir = \"/c\", zig_cache_dir = \"/c\" }}, \
             mold = {{ linker = {cmd}, flags = {f} }} }}",
            ti = toolchain_inputs,
            t = tool(),
            p = program(),
            cmd = command(),
            f = flags(),
        );
        let sources = format!(
            "{{ git = {{ create_directory = {c}, copy = {c} }} }}",
            c = builder_command()
        );
        let builders = format!(
            "{{ cmake = {b}, meson = {b}, cargo = {b}, autotools = {b} }}",
            b = standard_builder()
        );
        let pgo = format!(
            "{{ shell_interpreter = {p}, merge_program = {p}, merge_args = {{}}, copy_program = {p}, \
             remove_program = {p}, sample = {tf}, stage_one = {s}, stage_two = {s}, use_profile = {s} }}",
            p = program(),
            tf = toolchain_flags(),
            s = stage(),
        );
        format!(
            "return {{\n\
             build_subdir = \"build\",\n\
             layout = {{ {layout} }},\n\
             toolchains = {toolchains},\n\
             targets = {{}},\n\
             retired_targets = {{}},\n\
             sandbox = {sandbox},\n\
             build_root = {build_root},\n\
             sources = {sources},\n\
             tuning = {{ flags = {{}}, groups = {{}}, default_groups = {{}} }},\n\
             environment = {{}},\n\
             builders = {builders},\n\
             analyzers = {{}},\n\
             pgo = {pgo},\n\
             }}"
        )
    }

    #[test]
    fn a_complete_build_policy_decodes_across_every_top_level_field() {
        let source = complete_policy_source();
        let policy = LuaBuildPolicyEvaluator::default()
            .evaluate(&Source::new("build-policy.lua", &source))
            .expect("complete policy decodes");

        assert_eq!(policy.build_subdir, "build");
        assert_eq!(policy.layout.prefix, TextSpec::Literal("/prefix".to_owned()));
        assert_eq!(policy.toolchains.llvm.cc.program.path, "/bin/tool");
        assert_eq!(policy.sandbox.hostname, "builder");
        assert_eq!(policy.build_root.compiler_cache.ccache_dir, "/c");
        assert_eq!(policy.builders.cmake.setup.program.path, "/bin/tool");
        assert_eq!(policy.pgo.shell_interpreter.path, "/bin/tool");
        assert!(policy.targets.is_empty());
    }

    #[test]
    fn an_all_keep_build_policy_patch_decodes_to_the_default_overlay() {
        let source = r#"
return {
    build_subdir = { kind = "keep" },
    layout = { kind = "keep" },
    toolchains = { kind = "keep" },
    targets = { kind = "keep" },
    retired_targets = { kind = "keep" },
    sandbox = { kind = "keep" },
    build_root = { kind = "keep" },
    sources = { kind = "keep" },
    tuning = { kind = "keep" },
    environment = { kind = "keep" },
    builders = { kind = "keep" },
    analyzers = { kind = "keep" },
    pgo = { kind = "keep" },
}
"#;
        let patch = LuaBuildPolicyEvaluator::default()
            .evaluate_patch(&Source::new("build-policy.lua", source))
            .expect("all-keep patch decodes");
        assert_eq!(patch, BuildPolicyPatchSpec::default());
    }

    #[test]
    fn a_build_policy_patch_sets_a_scalar_and_appends_an_analyzer() {
        let source = r#"
return {
    build_subdir = { kind = "set", value = "build" },
    layout = { kind = "keep" },
    toolchains = { kind = "keep" },
    targets = { kind = "keep" },
    retired_targets = { kind = "keep" },
    sandbox = { kind = "keep" },
    build_root = { kind = "keep" },
    sources = { kind = "keep" },
    tuning = { kind = "keep" },
    environment = { kind = "keep" },
    builders = { kind = "keep" },
    analyzers = { kind = "append", values = { "elf" } },
    pgo = { kind = "keep" },
}
"#;
        let patch = LuaBuildPolicyEvaluator::default()
            .evaluate_patch(&Source::new("build-policy.lua", source))
            .expect("patch decodes");
        assert_eq!(patch.build_subdir, ValuePatch::Set("build".to_owned()));
        assert_eq!(patch.analyzers, ArrayPatch::Append(vec![AnalyzerKind::Elf]));
    }

    #[test]
    fn an_analyzer_kind_decodes_from_its_snake_case_name() {
        let kind: super::super::AnalyzerKind = decode(r#"return "pkg_config""#);
        assert_eq!(kind, super::super::AnalyzerKind::PkgConfig);
    }

    fn literal_layout_field(name: &str) -> String {
        format!(r#"{name} = {{ kind = "literal", value = "/{name}" }}"#)
    }

    #[test]
    fn an_install_layout_decodes_every_locator() {
        let fields = [
            "prefix", "bindir", "sbindir", "includedir", "libdir", "libexecdir", "datadir",
            "vendordir", "docdir", "infodir", "localedir", "mandir", "sysconfdir", "localstatedir",
            "sharedstatedir", "runstatedir", "sysusersdir", "tmpfilesdir", "udevrulesdir",
            "bash_completions_dir", "fish_completions_dir", "elvish_completions_dir",
            "zsh_completions_dir",
        ];
        let body = fields.iter().map(|name| literal_layout_field(name)).collect::<Vec<_>>().join(",\n    ");
        let source = format!("return {{\n    {body},\n}}");

        let layout: InstallLayoutSpec = decode::<LuaInstallLayoutSpec>(&source).into();
        assert_eq!(layout.prefix, TextSpec::Literal("/prefix".to_owned()));
        assert_eq!(layout.zsh_completions_dir, TextSpec::Literal("/zsh_completions_dir".to_owned()));
    }
}
