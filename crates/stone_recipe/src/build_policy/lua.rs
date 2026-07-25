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

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Diagnostic, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_option, lua_string, pretty_lua};

use super::{
    AnalyzerToolchainPolicySpec, AnalyzerToolsPolicySpec,
    BuildCommandSpec, BuildPolicyConversionError, BuildPolicyPatchSpec, BuildPolicySpec,
    BuildProgramSpec, BuildRootPolicySpec,
    BuildToolSpec, BuilderCommandSpec, BuildersPolicySpec, CompilerCachePolicySpec,
    CompilerFlagsSpec, CompilerToolsSpec, ContextValue, Emul32InputPolicySpec,
    EnvironmentBindingSpec, EnvironmentCondition, GitPreparationPolicySpec, InstallLayoutSpec,
    MoldPolicySpec, NamedTuningChoiceSpec, NamedTuningFlagSpec, NamedTuningGroupSpec, PgoFinishSpec,
    PgoPolicySpec, PgoStagePolicySpec, PlatformPolicySpec, RetiredTargetPolicySpec,
    SandboxCredentialPolicySpec, SandboxDevPolicySpec, SandboxFilesystemPolicySpec,
    SandboxPolicySpec, SandboxSysPolicySpec, SandboxTmpPolicySpec, SourcePreparationPolicySpec,
    StandardBuilderPolicySpec, TargetEmulationSpec, TargetPolicySpec, TextSpec, ToolchainFlagsSpec,
    ToolchainInputPolicySpec, ToolchainsSpec, TuningGroupSpec, TuningOptionSpec, TuningPolicySpec,
};

mod convert;
pub(crate) use convert::*;

/// Stateless Lua adapter for the build-policy declaration and its patch overlay.
#[derive(Debug, Clone, Default)]
pub struct LuaBuildPolicyEvaluator {
    engine: LuaEngine,
}

impl LuaBuildPolicyEvaluator {
    /// Decode a complete authored build policy.
    pub fn evaluate(&self, source: &Source) -> Result<BuildPolicySpec, Diagnostic> {
        Ok(self.engine.evaluate_as::<LuaBuildPolicySpec>(source)?.value.into())
    }

    /// Decode a sparse build-policy patch overlay.
    pub(crate) fn evaluate_patch(&self, source: &Source) -> Result<BuildPolicyPatchSpec, Diagnostic> {
        Ok(self.engine.evaluate_as::<LuaBuildPolicyPatchSpec>(source)?.value.into())
    }
}

impl DeclarationEvaluator<BuildPolicySpec> for LuaBuildPolicyEvaluator {
    type Identity = EvaluationIdentity;
    type Error = BuildPolicyConversionError;

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
        DeclarationEvaluation<BuildPolicySpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<LuaBuildPolicySpec>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let policy: BuildPolicySpec = evaluation.value.into();
        policy.validate().map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: policy,
            identity: evaluation.identity,
        })
    }
}

impl DeclarationEvaluator<BuildPolicyPatchSpec> for LuaBuildPolicyEvaluator {
    type Identity = EvaluationIdentity;
    type Error = BuildPolicyConversionError;

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
        DeclarationEvaluation<BuildPolicyPatchSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<LuaBuildPolicyPatchSpec>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        Ok(DeclarationEvaluation {
            value: evaluation.value.into(),
            identity: evaluation.identity,
        })
    }
}

/// One registered build-policy layer language (`.glu` or `.lua`), selected by a
/// layer file's extension. Both engines reach the same validated
/// [`BuildPolicySpec`]/[`BuildPolicyPatchSpec`] with a shared conversion error,
/// so the composition loader stays language-neutral.
#[derive(Debug, Clone)]
pub enum BuildPolicyEvaluator {
    Gluon(super::GluonBuildPolicyEvaluator),
    Lua(LuaBuildPolicyEvaluator),
}

impl BuildPolicyEvaluator {
    /// The registered layer languages, `.glu` first, sharing a conversion error.
    pub fn registered() -> [Self; 2] {
        [
            Self::Gluon(super::GluonBuildPolicyEvaluator::default()),
            Self::Lua(LuaBuildPolicyEvaluator::default()),
        ]
    }
}

impl DeclarationEvaluator<BuildPolicySpec> for BuildPolicyEvaluator {
    type Identity = EvaluationIdentity;
    type Error = BuildPolicyConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Gluon(evaluator) => {
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::language_spec(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::language_spec(evaluator)
            }
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(evaluator) => {
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::limits(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::limits(evaluator)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(evaluator) => Self::Gluon(
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::with_source_root(evaluator, source_root),
            ),
            Self::Lua(evaluator) => Self::Lua(
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::with_source_root(evaluator, source_root),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<BuildPolicySpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        match self {
            Self::Gluon(evaluator) => evaluator.evaluate_within(source, deadline),
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::evaluate_within(evaluator, source, deadline)
            }
        }
    }
}

impl DeclarationEvaluator<BuildPolicyPatchSpec> for BuildPolicyEvaluator {
    type Identity = EvaluationIdentity;
    type Error = BuildPolicyConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Gluon(evaluator) => {
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::language_spec(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::language_spec(evaluator)
            }
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(evaluator) => {
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::limits(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::limits(evaluator)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(evaluator) => Self::Gluon(
                <super::GluonBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::with_source_root(evaluator, source_root),
            ),
            Self::Lua(evaluator) => Self::Lua(
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::with_source_root(evaluator, source_root),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<BuildPolicyPatchSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        match self {
            Self::Gluon(evaluator) => evaluator.evaluate_within(source, deadline),
            Self::Lua(evaluator) => {
                <LuaBuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::evaluate_within(evaluator, source, deadline)
            }
        }
    }
}

// ---- Emitter (write path) ------------------------------------------------
//
// Produces the same tagged Lua encoding the decoders above accept, so an
// emitted policy re-decodes to an equal [`BuildPolicySpec`]. This is the build
// policy's write path — what a generated-slot authority switch writes when it
// converts a `policy.glu` fragment to `policy.lua`.

/// Emit a complete [`BuildPolicySpec`] as generated-marked Lua source.
pub fn encode_lua_policy(policy: &BuildPolicySpec) -> String {
    pretty_lua(&format!(
        "{marker}return {{\n\
         build_subdir = {build_subdir},\n\
         layout = {layout},\n\
         toolchains = {toolchains},\n\
         targets = {targets},\n\
         retired_targets = {retired_targets},\n\
         sandbox = {sandbox},\n\
         build_root = {build_root},\n\
         sources = {sources},\n\
         tuning = {tuning},\n\
         environment = {environment},\n\
         builders = {builders},\n\
         analyzers = {analyzers},\n\
         pgo = {pgo},\n\
         }}\n",
        marker = GENERATED_LUA_MARKER,
        build_subdir = lua_string(&policy.build_subdir),
        layout = install_layout(&policy.layout),
        toolchains = toolchains(&policy.toolchains),
        targets = seq(&policy.targets, target_policy),
        retired_targets = seq(&policy.retired_targets, retired_target),
        sandbox = sandbox_policy(&policy.sandbox),
        build_root = build_root(&policy.build_root),
        sources = source_prep(&policy.sources),
        tuning = tuning_policy(&policy.tuning),
        environment = seq(&policy.environment, environment_binding),
        builders = builders(&policy.builders),
        analyzers = seq(&policy.analyzers, |kind| lua_string(&serde_snake_case(kind.as_str()))),
        pgo = pgo_policy(&policy.pgo),
    ))
}

/// Emit a slice as a Lua array table using a per-element encoder.
fn seq<T>(items: &[T], encode: impl Fn(&T) -> String) -> String {
    let body = items.iter().map(encode).collect::<Vec<_>>().join(", ");
    format!("{{ {body} }}")
}

/// Emit a slice of strings as a Lua array table of string literals.
fn string_seq(items: &[String]) -> String {
    seq(items, |value| lua_string(value))
}

/// Serde's `snake_case` rename applied to a PascalCase variant name, so an
/// emitted enum tag re-decodes through the same `rename_all` the DTOs derive.
fn serde_snake_case(name: &str) -> String {
    let mut snake = String::with_capacity(name.len() + 4);
    for (index, character) in name.char_indices() {
        if index > 0 && character.is_ascii_uppercase() {
            snake.push('_');
        }
        snake.push(character.to_ascii_lowercase());
    }
    snake
}

/// Emit a [`ContextValue`] as its snake_case name string literal.
fn context_value(value: ContextValue) -> String {
    lua_string(&serde_snake_case(context_value_name(value)))
}

/// The exact PascalCase identifier of each [`ContextValue`] variant, used only
/// as input to [`serde_snake_case`] so the emitted tag matches the decoder.
fn context_value_name(value: ContextValue) -> &'static str {
    match value {
        ContextValue::PackageName => "PackageName",
        ContextValue::PackageVersion => "PackageVersion",
        ContextValue::PackageRelease => "PackageRelease",
        ContextValue::SourceDir => "SourceDir",
        ContextValue::InstallRoot => "InstallRoot",
        ContextValue::BuildRoot => "BuildRoot",
        ContextValue::WorkDir => "WorkDir",
        ContextValue::BuilderDir => "BuilderDir",
        ContextValue::PgoDir => "PgoDir",
        ContextValue::Jobs => "Jobs",
        ContextValue::SourceDateEpoch => "SourceDateEpoch",
        ContextValue::PgoStage => "PgoStage",
        ContextValue::TargetTriple => "TargetTriple",
        ContextValue::BuildPlatform => "BuildPlatform",
        ContextValue::HostPlatform => "HostPlatform",
        ContextValue::LibSuffix => "LibSuffix",
        ContextValue::Prefix => "Prefix",
        ContextValue::BinDir => "BinDir",
        ContextValue::SbinDir => "SbinDir",
        ContextValue::IncludeDir => "IncludeDir",
        ContextValue::LibDir => "LibDir",
        ContextValue::LibexecDir => "LibexecDir",
        ContextValue::DataDir => "DataDir",
        ContextValue::VendorDir => "VendorDir",
        ContextValue::DocDir => "DocDir",
        ContextValue::InfoDir => "InfoDir",
        ContextValue::LocaleDir => "LocaleDir",
        ContextValue::ManDir => "ManDir",
        ContextValue::SysconfDir => "SysconfDir",
        ContextValue::LocalStateDir => "LocalStateDir",
        ContextValue::SharedStateDir => "SharedStateDir",
        ContextValue::RunStateDir => "RunStateDir",
        ContextValue::CFlags => "CFlags",
        ContextValue::CxxFlags => "CxxFlags",
        ContextValue::FFlags => "FFlags",
        ContextValue::DFlags => "DFlags",
        ContextValue::RustFlags => "RustFlags",
        ContextValue::ValaFlags => "ValaFlags",
        ContextValue::GoFlags => "GoFlags",
        ContextValue::LdFlags => "LdFlags",
        ContextValue::Cc => "Cc",
        ContextValue::Cxx => "Cxx",
        ContextValue::Objc => "Objc",
        ContextValue::Objcxx => "Objcxx",
        ContextValue::Cpp => "Cpp",
        ContextValue::Objcpp => "Objcpp",
        ContextValue::Objcxxcpp => "Objcxxcpp",
        ContextValue::Ar => "Ar",
        ContextValue::Ld => "Ld",
        ContextValue::Objcopy => "Objcopy",
        ContextValue::Nm => "Nm",
        ContextValue::Ranlib => "Ranlib",
        ContextValue::Strip => "Strip",
        ContextValue::CcacheDir => "CcacheDir",
        ContextValue::SccacheDir => "SccacheDir",
        ContextValue::GoCacheDir => "GoCacheDir",
        ContextValue::GoModCacheDir => "GoModCacheDir",
        ContextValue::CargoCacheDir => "CargoCacheDir",
        ContextValue::ZigCacheDir => "ZigCacheDir",
        ContextValue::RustcWrapper => "RustcWrapper",
        ContextValue::SourcePath => "SourcePath",
        ContextValue::SourceDestination => "SourceDestination",
    }
}

/// Emit a [`TextSpec`] as its tagged `{ kind = … }` encoding.
fn text_spec(text: &TextSpec) -> String {
    match text {
        TextSpec::Literal(value) => {
            format!(r#"{{ kind = "literal", value = {} }}"#, lua_string(value))
        }
        TextSpec::Context(value) => {
            format!(r#"{{ kind = "context", value = {} }}"#, context_value(*value))
        }
        TextSpec::Concat(values) => {
            format!(r#"{{ kind = "concat", values = {} }}"#, seq(values, text_spec))
        }
    }
}

/// Emit a [`BuildToolSpec`] as its tagged capability encoding.
fn build_tool(tool: &BuildToolSpec) -> String {
    let (kind, value) = match tool {
        BuildToolSpec::Package(value) => ("package", value),
        BuildToolSpec::Binary(value) => ("binary", value),
        BuildToolSpec::SystemBinary(value) => ("system_binary", value),
    };
    format!(r#"{{ kind = "{kind}", value = {} }}"#, lua_string(value))
}

/// Emit an [`EnvironmentCondition`] as its snake_case name string literal.
fn environment_condition(condition: EnvironmentCondition) -> String {
    let name = match condition {
        EnvironmentCondition::Always => "always",
        EnvironmentCondition::CompilerCacheEnabled => "compiler_cache_enabled",
        EnvironmentCondition::CompilerCacheDisabled => "compiler_cache_disabled",
    };
    lua_string(name)
}

/// Emit an [`EnvironmentBindingSpec`].
fn environment_binding(binding: &EnvironmentBindingSpec) -> String {
    format!(
        "{{ name = {}, value = {}, condition = {} }}",
        lua_string(&binding.name),
        text_spec(&binding.value),
        environment_condition(binding.condition),
    )
}

/// Emit a [`CompilerFlagsSpec`] — eight ordered flag lists of text specs.
fn compiler_flags(flags: &CompilerFlagsSpec) -> String {
    format!(
        "{{ c = {}, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} }}",
        seq(&flags.c, text_spec),
        seq(&flags.cxx, text_spec),
        seq(&flags.f, text_spec),
        seq(&flags.d, text_spec),
        seq(&flags.rust, text_spec),
        seq(&flags.vala, text_spec),
        seq(&flags.go, text_spec),
        seq(&flags.ld, text_spec),
    )
}

/// Emit a [`ToolchainFlagsSpec`] — common/GNU/LLVM flag sets.
fn toolchain_flags(flags: &ToolchainFlagsSpec) -> String {
    format!(
        "{{ common = {}, gnu = {}, llvm = {} }}",
        compiler_flags(&flags.common),
        compiler_flags(&flags.gnu),
        compiler_flags(&flags.llvm),
    )
}

/// Emit an [`InstallLayoutSpec`] — every install directory locator.
fn install_layout(layout: &InstallLayoutSpec) -> String {
    format!(
        "{{ prefix = {}, bindir = {}, sbindir = {}, includedir = {}, libdir = {}, \
         libexecdir = {}, datadir = {}, vendordir = {}, docdir = {}, infodir = {}, \
         localedir = {}, mandir = {}, sysconfdir = {}, localstatedir = {}, sharedstatedir = {}, \
         runstatedir = {}, sysusersdir = {}, tmpfilesdir = {}, udevrulesdir = {}, \
         bash_completions_dir = {}, fish_completions_dir = {}, elvish_completions_dir = {}, \
         zsh_completions_dir = {} }}",
        text_spec(&layout.prefix),
        text_spec(&layout.bindir),
        text_spec(&layout.sbindir),
        text_spec(&layout.includedir),
        text_spec(&layout.libdir),
        text_spec(&layout.libexecdir),
        text_spec(&layout.datadir),
        text_spec(&layout.vendordir),
        text_spec(&layout.docdir),
        text_spec(&layout.infodir),
        text_spec(&layout.localedir),
        text_spec(&layout.mandir),
        text_spec(&layout.sysconfdir),
        text_spec(&layout.localstatedir),
        text_spec(&layout.sharedstatedir),
        text_spec(&layout.runstatedir),
        text_spec(&layout.sysusersdir),
        text_spec(&layout.tmpfilesdir),
        text_spec(&layout.udevrulesdir),
        text_spec(&layout.bash_completions_dir),
        text_spec(&layout.fish_completions_dir),
        text_spec(&layout.elvish_completions_dir),
        text_spec(&layout.zsh_completions_dir),
    )
}

/// Emit a [`BuildProgramSpec`].
fn build_program(program: &BuildProgramSpec) -> String {
    format!(
        "{{ path = {}, requirement = {} }}",
        lua_string(&program.path),
        build_tool(&program.requirement),
    )
}

/// Emit a [`BuildCommandSpec`].
fn build_command(command: &BuildCommandSpec) -> String {
    format!(
        "{{ program = {}, args = {} }}",
        build_program(&command.program),
        string_seq(&command.args),
    )
}

/// Emit a [`CompilerToolsSpec`] — one build command per toolchain role.
fn compiler_tools(tools: &CompilerToolsSpec) -> String {
    format!(
        "{{ cc = {}, cxx = {}, objc = {}, objcxx = {}, cpp = {}, objcpp = {}, objcxxcpp = {}, \
         ar = {}, ld = {}, objcopy = {}, nm = {}, ranlib = {}, strip = {} }}",
        build_command(&tools.cc),
        build_command(&tools.cxx),
        build_command(&tools.objc),
        build_command(&tools.objcxx),
        build_command(&tools.cpp),
        build_command(&tools.objcpp),
        build_command(&tools.objcxxcpp),
        build_command(&tools.ar),
        build_command(&tools.ld),
        build_command(&tools.objcopy),
        build_command(&tools.nm),
        build_command(&tools.ranlib),
        build_command(&tools.strip),
    )
}

/// Emit a [`ToolchainsSpec`] — the LLVM and GNU tool tables.
fn toolchains(spec: &ToolchainsSpec) -> String {
    format!(
        "{{ llvm = {}, gnu = {} }}",
        compiler_tools(&spec.llvm),
        compiler_tools(&spec.gnu),
    )
}

/// Emit a [`PlatformPolicySpec`] as pure data.
fn platform(platform: &PlatformPolicySpec) -> String {
    format!(
        "{{ architecture = {}, vendor = {}, operating_system = {}, abi = {} }}",
        lua_string(&platform.architecture),
        lua_string(&platform.vendor),
        lua_string(&platform.operating_system),
        lua_string(&platform.abi),
    )
}

/// Emit a [`TargetEmulationSpec`] as its tagged encoding.
fn target_emulation(emulation: &TargetEmulationSpec) -> String {
    match emulation {
        TargetEmulationSpec::Native => r#"{ kind = "native" }"#.to_owned(),
        TargetEmulationSpec::Emul32 { host_architecture } => format!(
            r#"{{ kind = "emul32", host_architecture = {} }}"#,
            lua_string(host_architecture),
        ),
    }
}

/// Emit a [`TargetPolicySpec`].
fn target_policy(target: &TargetPolicySpec) -> String {
    format!(
        "{{ name = {}, target_triple = {}, build_triple = {}, host_triple = {}, lib_suffix = {}, \
         artifact_architecture = {}, emulation = {}, build_platform = {}, host_platform = {}, \
         target_platform = {}, architecture_flags = {}, environment = {} }}",
        lua_string(&target.name),
        lua_string(&target.target_triple),
        lua_string(&target.build_triple),
        lua_string(&target.host_triple),
        lua_string(&target.lib_suffix),
        lua_string(&target.artifact_architecture),
        target_emulation(&target.emulation),
        platform(&target.build_platform),
        platform(&target.host_platform),
        platform(&target.target_platform),
        toolchain_flags(&target.architecture_flags),
        seq(&target.environment, environment_binding),
    )
}

/// Emit a [`RetiredTargetPolicySpec`].
fn retired_target(target: &RetiredTargetPolicySpec) -> String {
    format!(
        "{{ name = {}, reason = {} }}",
        lua_string(&target.name),
        lua_string(&target.reason),
    )
}

/// Emit a [`SandboxCredentialPolicySpec`] as its snake_case name.
fn sandbox_credentials(credentials: SandboxCredentialPolicySpec) -> String {
    let name = match credentials {
        SandboxCredentialPolicySpec::IsolatedRoot => "isolated_root",
    };
    lua_string(name)
}

/// Emit a [`SandboxFilesystemPolicySpec`] as its three named modes.
fn sandbox_filesystems(filesystems: SandboxFilesystemPolicySpec) -> String {
    let tmp = match filesystems.tmp {
        SandboxTmpPolicySpec::Empty => "empty",
    };
    let sys = match filesystems.sys {
        SandboxSysPolicySpec::None => "none",
    };
    let dev = match filesystems.dev {
        SandboxDevPolicySpec::None => "none",
        SandboxDevPolicySpec::Minimal => "minimal",
    };
    format!(
        "{{ tmp = {}, sys = {}, dev = {} }}",
        lua_string(tmp),
        lua_string(sys),
        lua_string(dev),
    )
}

/// Emit a [`SandboxPolicySpec`] as pure data.
fn sandbox_policy(sandbox: &SandboxPolicySpec) -> String {
    format!(
        "{{ hostname = {}, credentials = {}, filesystems = {}, guest_root = {}, artifacts_dir = {}, \
         build_dir = {}, source_dir = {}, recipe_dir = {}, package_dir = {}, install_dir = {} }}",
        lua_string(&sandbox.hostname),
        sandbox_credentials(sandbox.credentials),
        sandbox_filesystems(sandbox.filesystems),
        lua_string(&sandbox.guest_root),
        lua_string(&sandbox.artifacts_dir),
        lua_string(&sandbox.build_dir),
        lua_string(&sandbox.source_dir),
        lua_string(&sandbox.recipe_dir),
        lua_string(&sandbox.package_dir),
        lua_string(&sandbox.install_dir),
    )
}

/// Emit a [`ToolchainInputPolicySpec`].
fn toolchain_input(inputs: &ToolchainInputPolicySpec) -> String {
    format!(
        "{{ llvm = {}, gnu = {} }}",
        seq(&inputs.llvm, build_tool),
        seq(&inputs.gnu, build_tool),
    )
}

/// Emit an [`Emul32InputPolicySpec`].
fn emul32_input(inputs: &Emul32InputPolicySpec) -> String {
    format!(
        "{{ base = {}, toolchains = {} }}",
        seq(&inputs.base, build_tool),
        toolchain_input(&inputs.toolchains),
    )
}

/// Emit an [`AnalyzerToolchainPolicySpec`].
fn analyzer_toolchain(tools: &AnalyzerToolchainPolicySpec) -> String {
    format!(
        "{{ objcopy = {}, strip = {} }}",
        build_tool(&tools.objcopy),
        build_tool(&tools.strip),
    )
}

/// Emit an [`AnalyzerToolsPolicySpec`].
fn analyzer_tools(tools: &AnalyzerToolsPolicySpec) -> String {
    format!(
        "{{ pkg_config = {}, python = {}, llvm = {}, gnu = {} }}",
        build_tool(&tools.pkg_config),
        build_tool(&tools.python),
        analyzer_toolchain(&tools.llvm),
        analyzer_toolchain(&tools.gnu),
    )
}

/// Emit a [`CompilerCachePolicySpec`].
fn compiler_cache(cache: &CompilerCachePolicySpec) -> String {
    format!(
        "{{ ccache = {}, sccache = {}, ccache_dir = {}, sccache_dir = {}, go_cache_dir = {}, \
         go_mod_cache_dir = {}, cargo_cache_dir = {}, zig_cache_dir = {} }}",
        build_program(&cache.ccache),
        build_program(&cache.sccache),
        lua_string(&cache.ccache_dir),
        lua_string(&cache.sccache_dir),
        lua_string(&cache.go_cache_dir),
        lua_string(&cache.go_mod_cache_dir),
        lua_string(&cache.cargo_cache_dir),
        lua_string(&cache.zig_cache_dir),
    )
}

/// Emit a [`MoldPolicySpec`].
fn mold(mold: &MoldPolicySpec) -> String {
    format!(
        "{{ linker = {}, flags = {} }}",
        build_command(&mold.linker),
        compiler_flags(&mold.flags),
    )
}

/// Emit a [`BuildRootPolicySpec`].
fn build_root(root: &BuildRootPolicySpec) -> String {
    format!(
        "{{ base = {}, toolchains = {}, emul32 = {}, analyzer_tools = {}, compiler_cache = {}, mold = {} }}",
        seq(&root.base, build_tool),
        toolchain_input(&root.toolchains),
        emul32_input(&root.emul32),
        analyzer_tools(&root.analyzer_tools),
        compiler_cache(&root.compiler_cache),
        mold(&root.mold),
    )
}

/// Emit a [`BuilderCommandSpec`].
fn builder_command(command: &BuilderCommandSpec) -> String {
    format!(
        "{{ program = {}, args = {}, environment = {}, working_dir = {} }}",
        build_program(&command.program),
        seq(&command.args, text_spec),
        seq(&command.environment, environment_binding),
        text_spec(&command.working_dir),
    )
}

/// Emit a [`GitPreparationPolicySpec`].
fn git_prep(git: &GitPreparationPolicySpec) -> String {
    format!(
        "{{ create_directory = {}, copy = {} }}",
        builder_command(&git.create_directory),
        builder_command(&git.copy),
    )
}

/// Emit a [`SourcePreparationPolicySpec`].
fn source_prep(sources: &SourcePreparationPolicySpec) -> String {
    format!("{{ git = {} }}", git_prep(&sources.git))
}

/// Emit a [`StandardBuilderPolicySpec`].
fn standard_builder(builder: &StandardBuilderPolicySpec) -> String {
    format!(
        "{{ environment = {}, setup = {}, build = {}, install = {}, check = {} }}",
        seq(&builder.environment, environment_binding),
        builder_command(&builder.setup),
        builder_command(&builder.build),
        builder_command(&builder.install),
        builder_command(&builder.check),
    )
}

/// Emit a [`BuildersPolicySpec`] — the four standard builders.
fn builders(builders: &BuildersPolicySpec) -> String {
    format!(
        "{{ cmake = {}, meson = {}, cargo = {}, autotools = {} }}",
        standard_builder(&builders.cmake),
        standard_builder(&builders.meson),
        standard_builder(&builders.cargo),
        standard_builder(&builders.autotools),
    )
}

/// Emit a [`PgoFinishSpec`].
fn pgo_finish(finish: &PgoFinishSpec) -> String {
    format!(
        "{{ output = {}, inputs = {}, copy_to = {}, remove_output_first = {} }}",
        text_spec(&finish.output),
        seq(&finish.inputs, text_spec),
        lua_option(finish.copy_to.as_ref().map(text_spec)),
        finish.remove_output_first,
    )
}

/// Emit a [`PgoStagePolicySpec`].
fn pgo_stage(stage: &PgoStagePolicySpec) -> String {
    format!(
        "{{ flags = {}, finish = {} }}",
        toolchain_flags(&stage.flags),
        lua_option(stage.finish.as_ref().map(pgo_finish)),
    )
}

/// Emit a [`PgoPolicySpec`].
fn pgo_policy(pgo: &PgoPolicySpec) -> String {
    format!(
        "{{ shell_interpreter = {}, merge_program = {}, merge_args = {}, copy_program = {}, \
         remove_program = {}, sample = {}, stage_one = {}, stage_two = {}, use_profile = {} }}",
        build_program(&pgo.shell_interpreter),
        build_program(&pgo.merge_program),
        seq(&pgo.merge_args, text_spec),
        build_program(&pgo.copy_program),
        build_program(&pgo.remove_program),
        toolchain_flags(&pgo.sample),
        pgo_stage(&pgo.stage_one),
        pgo_stage(&pgo.stage_two),
        pgo_stage(&pgo.use_profile),
    )
}

/// Emit a [`TuningOptionSpec`].
fn tuning_option(option: &TuningOptionSpec) -> String {
    format!(
        "{{ enabled = {}, disabled = {} }}",
        string_seq(&option.enabled),
        string_seq(&option.disabled),
    )
}

/// Emit a [`NamedTuningChoiceSpec`].
fn named_tuning_choice(choice: &NamedTuningChoiceSpec) -> String {
    format!(
        "{{ name = {}, value = {} }}",
        lua_string(&choice.name),
        tuning_option(&choice.value),
    )
}

/// Emit a [`TuningGroupSpec`].
fn tuning_group(group: &TuningGroupSpec) -> String {
    format!(
        "{{ base = {}, default = {}, choices = {} }}",
        tuning_option(&group.base),
        lua_option(group.default.as_deref().map(lua_string)),
        seq(&group.choices, named_tuning_choice),
    )
}

/// Emit a [`NamedTuningGroupSpec`].
fn named_tuning_group(group: &NamedTuningGroupSpec) -> String {
    format!(
        "{{ name = {}, value = {} }}",
        lua_string(&group.name),
        tuning_group(&group.value),
    )
}

/// Emit a [`NamedTuningFlagSpec`].
fn named_tuning_flag(flag: &NamedTuningFlagSpec) -> String {
    format!(
        "{{ name = {}, value = {} }}",
        lua_string(&flag.name),
        toolchain_flags(&flag.value),
    )
}

/// Emit a [`TuningPolicySpec`].
fn tuning_policy(tuning: &TuningPolicySpec) -> String {
    format!(
        "{{ flags = {}, groups = {}, default_groups = {} }}",
        seq(&tuning.flags, named_tuning_flag),
        seq(&tuning.groups, named_tuning_group),
        string_seq(&tuning.default_groups),
    )
}

#[cfg(test)]
mod tests;
