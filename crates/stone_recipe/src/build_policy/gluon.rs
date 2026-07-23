//! Restricted Gluon evaluation boundary for typed build policy.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation as DeclarationEvaluation,
    LanguageSpec, Limits, SourceRoot,
};
use gluon_config::{Diagnostic, EvaluationIdentity, GluonEngine, Source};

use super::{
    AnalyzerKind, AnalyzerToolchainPolicySpec, AnalyzerToolsPolicySpec, ArrayPatch, BuildCommandSpec,
    BuildPolicyConversionError, BuildPolicyPatchSpec, BuildPolicySpec, BuildProgramSpec, BuildRootPolicySpec,
    BuildToolSpec, BuilderCommandSpec, BuildersPolicySpec, CompilerCachePolicySpec, CompilerFlagsSpec,
    CompilerToolsSpec, ContextValue, Emul32InputPolicySpec, EnvironmentBindingSpec, EnvironmentCondition,
    GitPreparationPolicySpec, InstallLayoutSpec, MoldPolicySpec, NamedTuningChoiceSpec, NamedTuningFlagSpec,
    NamedTuningGroupSpec, PgoFinishSpec, PgoPolicySpec, PgoStagePolicySpec, PlatformPolicySpec,
    RetiredTargetPolicySpec, SandboxCredentialPolicySpec, SandboxDevPolicySpec, SandboxFilesystemPolicySpec,
    SandboxPolicySpec, SandboxSysPolicySpec, SandboxTmpPolicySpec, SourcePreparationPolicySpec,
    StandardBuilderPolicySpec, TargetEmulationSpec, TargetPolicySpec, TextSpec, ToolchainFlagsSpec,
    ToolchainInputPolicySpec, ToolchainsSpec, TuningGroupSpec, TuningOptionSpec, TuningPolicySpec, ValuePatch,
};

/// Version of the typed repository build-policy ABI.
pub const BUILD_POLICY_ABI_VERSION: u32 = 5;

/// Pure helpers imported by policy roots as `cast.build_policy.v5`.
pub const GLUON_BUILD_POLICY_ABI: &str = include_str!("../../gluon/build_policy.glu");

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

/// Stateful Gluon adapter for total build policies and total policy patches.
///
/// The complete v5 ABI and pure primitive catalog are fixed when the adapter
/// is constructed. Callers select the owned Rust declaration type through the
/// typed evaluator role rather than through a Gluon-named entry point.
#[derive(Debug, Clone)]
pub struct GluonBuildPolicyEvaluator {
    engine: GluonEngine,
}

impl Default for GluonBuildPolicyEvaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl GluonBuildPolicyEvaluator {
    pub fn new(limits: Limits) -> Self {
        Self::from_engine(GluonEngine::new(limits))
            .expect("the embedded build-policy ABI is valid and unique")
    }

    pub fn from_engine(engine: GluonEngine) -> Result<Self, Diagnostic> {
        let mut import_policy = engine.import_policy().clone();
        import_policy.enable_array_primitives();
        import_policy.enable_string_primitives();
        import_policy.insert_embedded_module("std.types", GLUON_PURE_TYPES)?;
        import_policy.insert_embedded_module(
            "cast.build_policy.v5",
            GLUON_BUILD_POLICY_ABI,
        )?;
        Ok(Self {
            engine: engine.with_import_policy(import_policy),
        })
    }

    fn evaluate_policy(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicySpec, EvaluationIdentity>,
        DeclarationEvaluationError<BuildPolicyConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs::<GluonBuildPolicySpec>(source, explicit_inputs)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let policy: BuildPolicySpec = evaluation.value.into();
        policy
            .validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: policy,
            identity: evaluation.identity,
        })
    }

    fn evaluate_patch(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicyPatchSpec, EvaluationIdentity>,
        DeclarationEvaluationError<BuildPolicyConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs::<GluonBuildPolicyPatchSpec>(source, explicit_inputs)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        Ok(DeclarationEvaluation {
            value: evaluation.value.into(),
            identity: evaluation.identity,
        })
    }
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBoolean {
    False,
    True,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonContextValue {
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

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonTextPartSpec {
    LiteralPart { value: String },
    ContextPart { value: GluonContextValue },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTextSpec {
    parts: Vec<GluonTextPartSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonInstallLayoutSpec {
    prefix: GluonTextSpec,
    bindir: GluonTextSpec,
    sbindir: GluonTextSpec,
    includedir: GluonTextSpec,
    libdir: GluonTextSpec,
    libexecdir: GluonTextSpec,
    datadir: GluonTextSpec,
    vendordir: GluonTextSpec,
    docdir: GluonTextSpec,
    infodir: GluonTextSpec,
    localedir: GluonTextSpec,
    mandir: GluonTextSpec,
    sysconfdir: GluonTextSpec,
    localstatedir: GluonTextSpec,
    sharedstatedir: GluonTextSpec,
    runstatedir: GluonTextSpec,
    sysusersdir: GluonTextSpec,
    tmpfilesdir: GluonTextSpec,
    udevrulesdir: GluonTextSpec,
    bash_completions_dir: GluonTextSpec,
    fish_completions_dir: GluonTextSpec,
    elvish_completions_dir: GluonTextSpec,
    zsh_completions_dir: GluonTextSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonCompilerToolsSpec {
    cc: GluonBuildCommandSpec,
    cxx: GluonBuildCommandSpec,
    objc: GluonBuildCommandSpec,
    objcxx: GluonBuildCommandSpec,
    cpp: GluonBuildCommandSpec,
    objcpp: GluonBuildCommandSpec,
    objcxxcpp: GluonBuildCommandSpec,
    ar: GluonBuildCommandSpec,
    ld: GluonBuildCommandSpec,
    objcopy: GluonBuildCommandSpec,
    nm: GluonBuildCommandSpec,
    ranlib: GluonBuildCommandSpec,
    strip: GluonBuildCommandSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonToolchainsSpec {
    llvm: GluonCompilerToolsSpec,
    gnu: GluonCompilerToolsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonCompilerFlagsSpec {
    c: Vec<GluonTextSpec>,
    cxx: Vec<GluonTextSpec>,
    f: Vec<GluonTextSpec>,
    d: Vec<GluonTextSpec>,
    rust: Vec<GluonTextSpec>,
    vala: Vec<GluonTextSpec>,
    go: Vec<GluonTextSpec>,
    ld: Vec<GluonTextSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPlatformPolicySpec {
    architecture: String,
    vendor: String,
    operating_system: String,
    abi: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonTargetEmulationSpec {
    Native,
    Emul32 { host_architecture: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTargetPolicySpec {
    name: String,
    target_triple: String,
    build_triple: String,
    host_triple: String,
    lib_suffix: String,
    artifact_architecture: String,
    emulation: GluonTargetEmulationSpec,
    build_platform: GluonPlatformPolicySpec,
    host_platform: GluonPlatformPolicySpec,
    target_platform: GluonPlatformPolicySpec,
    architecture_flags: GluonToolchainFlagsSpec,
    environment: Vec<GluonEnvironmentBindingSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRetiredTargetPolicySpec {
    name: String,
    reason: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonEnvironmentCondition {
    Always,
    CompilerCacheEnabled,
    CompilerCacheDisabled,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonEnvironmentBindingSpec {
    name: String,
    value: GluonTextSpec,
    condition: GluonEnvironmentCondition,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBuildToolSpec {
    Package { target: String },
    Binary { target: String },
    SystemBinary { target: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonToolchainInputPolicySpec {
    llvm: Vec<GluonBuildToolSpec>,
    gnu: Vec<GluonBuildToolSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonEmul32InputPolicySpec {
    base: Vec<GluonBuildToolSpec>,
    toolchains: GluonToolchainInputPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonAnalyzerToolchainPolicySpec {
    objcopy: GluonBuildToolSpec,
    strip: GluonBuildToolSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonAnalyzerToolsPolicySpec {
    pkg_config: GluonBuildToolSpec,
    python: GluonBuildToolSpec,
    llvm: GluonAnalyzerToolchainPolicySpec,
    gnu: GluonAnalyzerToolchainPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonCompilerCachePolicySpec {
    ccache: GluonBuildProgramSpec,
    sccache: GluonBuildProgramSpec,
    ccache_dir: String,
    sccache_dir: String,
    go_cache_dir: String,
    go_mod_cache_dir: String,
    cargo_cache_dir: String,
    zig_cache_dir: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonMoldPolicySpec {
    linker: GluonBuildCommandSpec,
    flags: GluonCompilerFlagsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildRootPolicySpec {
    base: Vec<GluonBuildToolSpec>,
    toolchains: GluonToolchainInputPolicySpec,
    emul32: GluonEmul32InputPolicySpec,
    analyzer_tools: GluonAnalyzerToolsPolicySpec,
    compiler_cache: GluonCompilerCachePolicySpec,
    mold: GluonMoldPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonSandboxTmpPolicySpec {
    EmptyTmp,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonSandboxSysPolicySpec {
    NoSys,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonSandboxDevPolicySpec {
    NoDev,
    MinimalDev,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonSandboxCredentialPolicySpec {
    IsolatedRootCredentials,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSandboxFilesystemPolicySpec {
    tmp: GluonSandboxTmpPolicySpec,
    sys: GluonSandboxSysPolicySpec,
    dev: GluonSandboxDevPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSandboxPolicySpec {
    hostname: String,
    credentials: GluonSandboxCredentialPolicySpec,
    filesystems: GluonSandboxFilesystemPolicySpec,
    guest_root: String,
    artifacts_dir: String,
    build_dir: String,
    source_dir: String,
    recipe_dir: String,
    package_dir: String,
    install_dir: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuilderCommandSpec {
    program: GluonBuildProgramSpec,
    args: Vec<GluonTextSpec>,
    environment: Vec<GluonEnvironmentBindingSpec>,
    working_dir: GluonTextSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildProgramSpec {
    path: String,
    requirement: GluonBuildToolSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildCommandSpec {
    program: GluonBuildProgramSpec,
    args: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonGitPreparationPolicySpec {
    create_directory: GluonBuilderCommandSpec,
    copy: GluonBuilderCommandSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSourcePreparationPolicySpec {
    git: GluonGitPreparationPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonStandardBuilderPolicySpec {
    environment: Vec<GluonEnvironmentBindingSpec>,
    setup: GluonBuilderCommandSpec,
    build: GluonBuilderCommandSpec,
    install: GluonBuilderCommandSpec,
    check: GluonBuilderCommandSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildersPolicySpec {
    cmake: GluonStandardBuilderPolicySpec,
    meson: GluonStandardBuilderPolicySpec,
    cargo: GluonStandardBuilderPolicySpec,
    autotools: GluonStandardBuilderPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonToolchainFlagsSpec {
    common: GluonCompilerFlagsSpec,
    gnu: GluonCompilerFlagsSpec,
    llvm: GluonCompilerFlagsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonNamedTuningFlagSpec {
    name: String,
    value: GluonToolchainFlagsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningOptionSpec {
    enabled: Vec<String>,
    disabled: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonNamedTuningChoiceSpec {
    name: String,
    value: GluonTuningOptionSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptionalChoiceName {
    NoChoice,
    SomeChoice(String),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningGroupSpec {
    base: GluonTuningOptionSpec,
    default: GluonOptionalChoiceName,
    choices: Vec<GluonNamedTuningChoiceSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonNamedTuningGroupSpec {
    name: String,
    value: GluonTuningGroupSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningPolicySpec {
    flags: Vec<GluonNamedTuningFlagSpec>,
    groups: Vec<GluonNamedTuningGroupSpec>,
    default_groups: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptionalTextSpec {
    NoText,
    SomeText(GluonTextSpec),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPgoFinishSpec {
    output: GluonTextSpec,
    inputs: Vec<GluonTextSpec>,
    copy_to: GluonOptionalTextSpec,
    remove_output_first: GluonBoolean,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptionalPgoFinishSpec {
    NoPgoFinish,
    SomePgoFinish(GluonPgoFinishSpec),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPgoStagePolicySpec {
    flags: GluonToolchainFlagsSpec,
    finish: GluonOptionalPgoFinishSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPgoPolicySpec {
    shell_interpreter: GluonBuildProgramSpec,
    merge_program: GluonBuildProgramSpec,
    merge_args: Vec<GluonTextSpec>,
    copy_program: GluonBuildProgramSpec,
    remove_program: GluonBuildProgramSpec,
    sample: GluonToolchainFlagsSpec,
    stage_one: GluonPgoStagePolicySpec,
    stage_two: GluonPgoStagePolicySpec,
    use_profile: GluonPgoStagePolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
#[allow(clippy::enum_variant_names)]
enum GluonAnalyzerKind {
    AnalyzerIgnoreBlocked,
    AnalyzerBinary,
    AnalyzerElf,
    AnalyzerPkgConfig,
    AnalyzerPython,
    AnalyzerCMake,
    AnalyzerCompressMan,
    AnalyzerIncludeAny,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicySpec {
    build_subdir: String,
    layout: GluonInstallLayoutSpec,
    toolchains: GluonToolchainsSpec,
    targets: Vec<GluonTargetPolicySpec>,
    retired_targets: Vec<GluonRetiredTargetPolicySpec>,
    sandbox: GluonSandboxPolicySpec,
    build_root: GluonBuildRootPolicySpec,
    sources: GluonSourcePreparationPolicySpec,
    tuning: GluonTuningPolicySpec,
    environment: Vec<GluonEnvironmentBindingSpec>,
    builders: GluonBuildersPolicySpec,
    analyzers: Vec<GluonAnalyzerKind>,
    pgo: GluonPgoPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonValuePatch<T> {
    KeepValue,
    SetValue(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
#[allow(clippy::enum_variant_names)]
enum GluonArrayPatch<T> {
    KeepArray,
    ReplaceArray(Vec<T>),
    PrependArray(Vec<T>),
    AppendArray(Vec<T>),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicyPatchSpec {
    build_subdir: GluonValuePatch<String>,
    layout: GluonValuePatch<GluonInstallLayoutSpec>,
    toolchains: GluonValuePatch<GluonToolchainsSpec>,
    targets: GluonArrayPatch<GluonTargetPolicySpec>,
    retired_targets: GluonArrayPatch<GluonRetiredTargetPolicySpec>,
    sandbox: GluonValuePatch<GluonSandboxPolicySpec>,
    build_root: GluonValuePatch<GluonBuildRootPolicySpec>,
    sources: GluonValuePatch<GluonSourcePreparationPolicySpec>,
    tuning: GluonValuePatch<GluonTuningPolicySpec>,
    environment: GluonArrayPatch<GluonEnvironmentBindingSpec>,
    builders: GluonValuePatch<GluonBuildersPolicySpec>,
    analyzers: GluonArrayPatch<GluonAnalyzerKind>,
    pgo: GluonValuePatch<GluonPgoPolicySpec>,
}

include!("gluon/conversions.rs");

impl DeclarationEvaluator<BuildPolicySpec> for GluonBuildPolicyEvaluator {
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

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<BuildPolicySpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_policy(source, &[])
    }
}

impl DeclarationInputEvaluator<BuildPolicySpec> for GluonBuildPolicyEvaluator {
    fn evaluate_with_inputs(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicySpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_policy(source, explicit_inputs)
    }
}

impl DeclarationEvaluator<BuildPolicyPatchSpec> for GluonBuildPolicyEvaluator {
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

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<BuildPolicyPatchSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_patch(source, &[])
    }
}

impl DeclarationInputEvaluator<BuildPolicyPatchSpec>
    for GluonBuildPolicyEvaluator
{
    fn evaluate_with_inputs(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicyPatchSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_patch(source, explicit_inputs)
    }
}
