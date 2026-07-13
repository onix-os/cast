// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Restricted Gluon evaluation boundary for typed build policy.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};
use thiserror::Error;

use super::{
    ArchivePreparationPolicySpec, ArrayPatch, BuildPolicyConversionError, BuildPolicyPatchSpec, BuildPolicySpec,
    BuildRootPolicySpec, BuildToolSpec, BuilderCommandSpec, BuildersPolicySpec, CompilerCachePolicySpec,
    CompilerFlagsSpec, CompilerToolsSpec, ContextValue, Emul32InputPolicySpec, EnvironmentBindingSpec,
    EnvironmentCondition, GitPreparationPolicySpec, InstallLayoutSpec, MoldPolicySpec, NamedTuningChoiceSpec,
    NamedTuningFlagSpec, NamedTuningGroupSpec, PgoFinishSpec, PgoPolicySpec, PgoStagePolicySpec, PlatformPolicySpec,
    RetiredTargetPolicySpec, SandboxPolicySpec, SourcePreparationPolicySpec, StandardBuilderPolicySpec,
    TargetEmulationSpec, TargetPolicySpec, TextSpec, ToolchainFlagsSpec, ToolchainInputPolicySpec, ToolchainsSpec,
    TuningGroupSpec, TuningOptionSpec, TuningPolicySpec, ValuePatch,
};

/// Version of the typed repository build-policy ABI.
pub const BUILD_POLICY_ABI_VERSION: u32 = 1;

/// Pure helpers imported by policy roots as `boulder.build_policy.v1`.
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

/// A normalized policy and the provenance of every evaluated input.
#[derive(Debug, Clone)]
pub struct EvaluatedBuildPolicy {
    pub policy: BuildPolicySpec,
    pub fingerprint: EvaluationFingerprint,
}

/// A normalized total policy patch and the provenance of every evaluated
/// input. Applying and validating it requires an explicit base policy.
#[derive(Debug, Clone)]
pub struct EvaluatedBuildPolicyPatch {
    pub patch: BuildPolicyPatchSpec,
    pub fingerprint: EvaluationFingerprint,
}

/// Failure to evaluate a typed build policy.
#[derive(Debug, Error)]
pub enum BuildPolicyEvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] BuildPolicyConversionError),
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
    cc: GluonTextSpec,
    cxx: GluonTextSpec,
    objc: GluonTextSpec,
    objcxx: GluonTextSpec,
    cpp: GluonTextSpec,
    objcpp: GluonTextSpec,
    objcxxcpp: GluonTextSpec,
    d: GluonTextSpec,
    ar: GluonTextSpec,
    ld: GluonTextSpec,
    objcopy: GluonTextSpec,
    nm: GluonTextSpec,
    ranlib: GluonTextSpec,
    strip: GluonTextSpec,
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
struct GluonCompilerCachePolicySpec {
    required_tools: Vec<GluonBuildToolSpec>,
    default_path: String,
    compiler_path: String,
    ccache_dir: String,
    sccache_dir: String,
    go_cache_dir: String,
    go_mod_cache_dir: String,
    cargo_cache_dir: String,
    zig_cache_dir: String,
    rustc_wrapper: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonMoldPolicySpec {
    required_tools: Vec<GluonBuildToolSpec>,
    linker: GluonTextSpec,
    flags: GluonCompilerFlagsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildRootPolicySpec {
    base: Vec<GluonBuildToolSpec>,
    toolchains: GluonToolchainInputPolicySpec,
    emul32: GluonEmul32InputPolicySpec,
    compiler_cache: GluonCompilerCachePolicySpec,
    mold: GluonMoldPolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSandboxPolicySpec {
    hostname: String,
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
    program: GluonTextSpec,
    args: Vec<GluonTextSpec>,
    environment: Vec<GluonEnvironmentBindingSpec>,
    working_dir: GluonTextSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonArchivePreparationPolicySpec {
    required_tools: Vec<GluonBuildToolSpec>,
    create_directory: GluonBuilderCommandSpec,
    unpack: GluonBuilderCommandSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonGitPreparationPolicySpec {
    required_tools: Vec<GluonBuildToolSpec>,
    create_directory: GluonBuilderCommandSpec,
    copy: GluonBuilderCommandSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSourcePreparationPolicySpec {
    archive: GluonArchivePreparationPolicySpec,
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
    required_tools: Vec<GluonBuildToolSpec>,
    merge_program: GluonTextSpec,
    merge_args: Vec<GluonTextSpec>,
    copy_program: GluonTextSpec,
    remove_program: GluonTextSpec,
    sample: GluonToolchainFlagsSpec,
    stage_one: GluonPgoStagePolicySpec,
    stage_two: GluonPgoStagePolicySpec,
    use_profile: GluonPgoStagePolicySpec,
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
    pgo: GluonValuePatch<GluonPgoPolicySpec>,
}

impl From<GluonBoolean> for bool {
    fn from(value: GluonBoolean) -> Self {
        matches!(value, GluonBoolean::True)
    }
}

impl From<GluonContextValue> for ContextValue {
    fn from(value: GluonContextValue) -> Self {
        match value {
            GluonContextValue::PackageName => Self::PackageName,
            GluonContextValue::PackageVersion => Self::PackageVersion,
            GluonContextValue::PackageRelease => Self::PackageRelease,
            GluonContextValue::SourceDir => Self::SourceDir,
            GluonContextValue::InstallRoot => Self::InstallRoot,
            GluonContextValue::BuildRoot => Self::BuildRoot,
            GluonContextValue::WorkDir => Self::WorkDir,
            GluonContextValue::BuilderDir => Self::BuilderDir,
            GluonContextValue::PgoDir => Self::PgoDir,
            GluonContextValue::Jobs => Self::Jobs,
            GluonContextValue::SourceDateEpoch => Self::SourceDateEpoch,
            GluonContextValue::PgoStage => Self::PgoStage,
            GluonContextValue::TargetTriple => Self::TargetTriple,
            GluonContextValue::BuildPlatform => Self::BuildPlatform,
            GluonContextValue::HostPlatform => Self::HostPlatform,
            GluonContextValue::LibSuffix => Self::LibSuffix,
            GluonContextValue::Prefix => Self::Prefix,
            GluonContextValue::BinDir => Self::BinDir,
            GluonContextValue::SbinDir => Self::SbinDir,
            GluonContextValue::IncludeDir => Self::IncludeDir,
            GluonContextValue::LibDir => Self::LibDir,
            GluonContextValue::LibexecDir => Self::LibexecDir,
            GluonContextValue::DataDir => Self::DataDir,
            GluonContextValue::VendorDir => Self::VendorDir,
            GluonContextValue::DocDir => Self::DocDir,
            GluonContextValue::InfoDir => Self::InfoDir,
            GluonContextValue::LocaleDir => Self::LocaleDir,
            GluonContextValue::ManDir => Self::ManDir,
            GluonContextValue::SysconfDir => Self::SysconfDir,
            GluonContextValue::LocalStateDir => Self::LocalStateDir,
            GluonContextValue::SharedStateDir => Self::SharedStateDir,
            GluonContextValue::RunStateDir => Self::RunStateDir,
            GluonContextValue::CFlags => Self::CFlags,
            GluonContextValue::CxxFlags => Self::CxxFlags,
            GluonContextValue::FFlags => Self::FFlags,
            GluonContextValue::DFlags => Self::DFlags,
            GluonContextValue::RustFlags => Self::RustFlags,
            GluonContextValue::ValaFlags => Self::ValaFlags,
            GluonContextValue::GoFlags => Self::GoFlags,
            GluonContextValue::LdFlags => Self::LdFlags,
            GluonContextValue::Cc => Self::Cc,
            GluonContextValue::Cxx => Self::Cxx,
            GluonContextValue::Objc => Self::Objc,
            GluonContextValue::Objcxx => Self::Objcxx,
            GluonContextValue::Cpp => Self::Cpp,
            GluonContextValue::Objcpp => Self::Objcpp,
            GluonContextValue::Objcxxcpp => Self::Objcxxcpp,
            GluonContextValue::D => Self::D,
            GluonContextValue::Ar => Self::Ar,
            GluonContextValue::Ld => Self::Ld,
            GluonContextValue::Objcopy => Self::Objcopy,
            GluonContextValue::Nm => Self::Nm,
            GluonContextValue::Ranlib => Self::Ranlib,
            GluonContextValue::Strip => Self::Strip,
            GluonContextValue::CompilerPath => Self::CompilerPath,
            GluonContextValue::CcacheDir => Self::CcacheDir,
            GluonContextValue::SccacheDir => Self::SccacheDir,
            GluonContextValue::GoCacheDir => Self::GoCacheDir,
            GluonContextValue::GoModCacheDir => Self::GoModCacheDir,
            GluonContextValue::CargoCacheDir => Self::CargoCacheDir,
            GluonContextValue::ZigCacheDir => Self::ZigCacheDir,
            GluonContextValue::RustcWrapper => Self::RustcWrapper,
            GluonContextValue::SourcePath => Self::SourcePath,
            GluonContextValue::SourceDestination => Self::SourceDestination,
            GluonContextValue::SourceStripComponents => Self::SourceStripComponents,
        }
    }
}

impl From<GluonTextSpec> for TextSpec {
    fn from(value: GluonTextSpec) -> Self {
        let mut parts = value.parts.into_iter().map(|part| match part {
            GluonTextPartSpec::LiteralPart { value } => Self::Literal(value),
            GluonTextPartSpec::ContextPart { value } => Self::Context(value.into()),
        });
        let Some(first) = parts.next() else {
            return Self::Concat(Vec::new());
        };
        match parts.next() {
            None => first,
            Some(second) => Self::Concat([first, second].into_iter().chain(parts).collect()),
        }
    }
}

impl From<GluonCompilerFlagsSpec> for CompilerFlagsSpec {
    fn from(value: GluonCompilerFlagsSpec) -> Self {
        Self {
            c: value.c.into_iter().map(Into::into).collect(),
            cxx: value.cxx.into_iter().map(Into::into).collect(),
            f: value.f.into_iter().map(Into::into).collect(),
            d: value.d.into_iter().map(Into::into).collect(),
            rust: value.rust.into_iter().map(Into::into).collect(),
            vala: value.vala.into_iter().map(Into::into).collect(),
            go: value.go.into_iter().map(Into::into).collect(),
            ld: value.ld.into_iter().map(Into::into).collect(),
        }
    }
}

macro_rules! convert_record {
    ($from:ty => $to:ty { $($field:ident),+ $(,)? }) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                Self { $($field: value.$field.into()),+ }
            }
        }
    };
}

convert_record!(GluonInstallLayoutSpec => InstallLayoutSpec {
    prefix, bindir, sbindir, includedir, libdir, libexecdir, datadir, vendordir, docdir, infodir, localedir,
    mandir, sysconfdir, localstatedir, sharedstatedir, runstatedir, sysusersdir, tmpfilesdir, udevrulesdir,
    bash_completions_dir, fish_completions_dir, elvish_completions_dir, zsh_completions_dir,
});
convert_record!(GluonCompilerToolsSpec => CompilerToolsSpec {
    cc, cxx, objc, objcxx, cpp, objcpp, objcxxcpp, d, ar, ld, objcopy, nm, ranlib, strip,
});
convert_record!(GluonToolchainsSpec => ToolchainsSpec { llvm, gnu });
convert_record!(GluonPlatformPolicySpec => PlatformPolicySpec {
    architecture, vendor, operating_system, abi,
});
impl From<GluonTargetPolicySpec> for TargetPolicySpec {
    fn from(value: GluonTargetPolicySpec) -> Self {
        Self {
            name: value.name,
            target_triple: value.target_triple,
            build_triple: value.build_triple,
            host_triple: value.host_triple,
            lib_suffix: value.lib_suffix,
            artifact_architecture: value.artifact_architecture,
            emulation: value.emulation.into(),
            build_platform: value.build_platform.into(),
            host_platform: value.host_platform.into(),
            target_platform: value.target_platform.into(),
            architecture_flags: value.architecture_flags.into(),
            environment: value.environment.into_iter().map(Into::into).collect(),
        }
    }
}
convert_record!(GluonRetiredTargetPolicySpec => RetiredTargetPolicySpec { name, reason });
convert_record!(GluonEnvironmentBindingSpec => EnvironmentBindingSpec { name, value, condition });
convert_record!(GluonSandboxPolicySpec => SandboxPolicySpec {
    hostname, guest_root, artifacts_dir, build_dir, source_dir, recipe_dir, package_dir, install_dir,
});
convert_record!(GluonSourcePreparationPolicySpec => SourcePreparationPolicySpec { archive, git });
convert_record!(GluonBuildersPolicySpec => BuildersPolicySpec { cmake, meson, cargo, autotools });
convert_record!(GluonToolchainFlagsSpec => ToolchainFlagsSpec { common, gnu, llvm });
convert_record!(GluonNamedTuningFlagSpec => NamedTuningFlagSpec { name, value });
convert_record!(GluonTuningOptionSpec => TuningOptionSpec { enabled, disabled });
convert_record!(GluonNamedTuningChoiceSpec => NamedTuningChoiceSpec { name, value });

impl From<GluonTuningGroupSpec> for TuningGroupSpec {
    fn from(value: GluonTuningGroupSpec) -> Self {
        Self {
            base: value.base.into(),
            default: match value.default {
                GluonOptionalChoiceName::NoChoice => None,
                GluonOptionalChoiceName::SomeChoice(value) => Some(value),
            },
            choices: value.choices.into_iter().map(Into::into).collect(),
        }
    }
}
convert_record!(GluonNamedTuningGroupSpec => NamedTuningGroupSpec { name, value });

impl From<GluonTuningPolicySpec> for TuningPolicySpec {
    fn from(value: GluonTuningPolicySpec) -> Self {
        Self {
            flags: value.flags.into_iter().map(Into::into).collect(),
            groups: value.groups.into_iter().map(Into::into).collect(),
            default_groups: value.default_groups,
        }
    }
}

impl From<GluonBuilderCommandSpec> for BuilderCommandSpec {
    fn from(value: GluonBuilderCommandSpec) -> Self {
        Self {
            program: value.program.into(),
            args: value.args.into_iter().map(Into::into).collect(),
            environment: value.environment.into_iter().map(Into::into).collect(),
            working_dir: value.working_dir.into(),
        }
    }
}

fn convert_tools(tools: Vec<GluonBuildToolSpec>) -> Vec<BuildToolSpec> {
    tools.into_iter().map(Into::into).collect()
}

impl From<GluonToolchainInputPolicySpec> for ToolchainInputPolicySpec {
    fn from(value: GluonToolchainInputPolicySpec) -> Self {
        Self {
            llvm: convert_tools(value.llvm),
            gnu: convert_tools(value.gnu),
        }
    }
}

impl From<GluonEmul32InputPolicySpec> for Emul32InputPolicySpec {
    fn from(value: GluonEmul32InputPolicySpec) -> Self {
        Self {
            base: convert_tools(value.base),
            toolchains: value.toolchains.into(),
        }
    }
}

impl From<GluonCompilerCachePolicySpec> for CompilerCachePolicySpec {
    fn from(value: GluonCompilerCachePolicySpec) -> Self {
        Self {
            required_tools: convert_tools(value.required_tools),
            default_path: value.default_path,
            compiler_path: value.compiler_path,
            ccache_dir: value.ccache_dir,
            sccache_dir: value.sccache_dir,
            go_cache_dir: value.go_cache_dir,
            go_mod_cache_dir: value.go_mod_cache_dir,
            cargo_cache_dir: value.cargo_cache_dir,
            zig_cache_dir: value.zig_cache_dir,
            rustc_wrapper: value.rustc_wrapper,
        }
    }
}

impl From<GluonMoldPolicySpec> for MoldPolicySpec {
    fn from(value: GluonMoldPolicySpec) -> Self {
        Self {
            required_tools: convert_tools(value.required_tools),
            linker: value.linker.into(),
            flags: value.flags.into(),
        }
    }
}

impl From<GluonBuildRootPolicySpec> for BuildRootPolicySpec {
    fn from(value: GluonBuildRootPolicySpec) -> Self {
        Self {
            base: convert_tools(value.base),
            toolchains: value.toolchains.into(),
            emul32: value.emul32.into(),
            compiler_cache: value.compiler_cache.into(),
            mold: value.mold.into(),
        }
    }
}

impl From<GluonArchivePreparationPolicySpec> for ArchivePreparationPolicySpec {
    fn from(value: GluonArchivePreparationPolicySpec) -> Self {
        Self {
            required_tools: convert_tools(value.required_tools),
            create_directory: value.create_directory.into(),
            unpack: value.unpack.into(),
        }
    }
}

impl From<GluonGitPreparationPolicySpec> for GitPreparationPolicySpec {
    fn from(value: GluonGitPreparationPolicySpec) -> Self {
        Self {
            required_tools: convert_tools(value.required_tools),
            create_directory: value.create_directory.into(),
            copy: value.copy.into(),
        }
    }
}

impl From<GluonStandardBuilderPolicySpec> for StandardBuilderPolicySpec {
    fn from(value: GluonStandardBuilderPolicySpec) -> Self {
        Self {
            environment: value.environment.into_iter().map(Into::into).collect(),
            setup: value.setup.into(),
            build: value.build.into(),
            install: value.install.into(),
            check: value.check.into(),
        }
    }
}

impl From<GluonPgoFinishSpec> for PgoFinishSpec {
    fn from(value: GluonPgoFinishSpec) -> Self {
        Self {
            output: value.output.into(),
            inputs: value.inputs.into_iter().map(Into::into).collect(),
            copy_to: match value.copy_to {
                GluonOptionalTextSpec::NoText => None,
                GluonOptionalTextSpec::SomeText(value) => Some(value.into()),
            },
            remove_output_first: value.remove_output_first.into(),
        }
    }
}

impl From<GluonPgoStagePolicySpec> for PgoStagePolicySpec {
    fn from(value: GluonPgoStagePolicySpec) -> Self {
        Self {
            flags: value.flags.into(),
            finish: match value.finish {
                GluonOptionalPgoFinishSpec::NoPgoFinish => None,
                GluonOptionalPgoFinishSpec::SomePgoFinish(value) => Some(value.into()),
            },
        }
    }
}

impl From<GluonPgoPolicySpec> for PgoPolicySpec {
    fn from(value: GluonPgoPolicySpec) -> Self {
        Self {
            required_tools: value.required_tools.into_iter().map(Into::into).collect(),
            merge_program: value.merge_program.into(),
            merge_args: value.merge_args.into_iter().map(Into::into).collect(),
            copy_program: value.copy_program.into(),
            remove_program: value.remove_program.into(),
            sample: value.sample.into(),
            stage_one: value.stage_one.into(),
            stage_two: value.stage_two.into(),
            use_profile: value.use_profile.into(),
        }
    }
}

impl From<GluonBuildPolicySpec> for BuildPolicySpec {
    fn from(value: GluonBuildPolicySpec) -> Self {
        Self {
            build_subdir: value.build_subdir,
            layout: value.layout.into(),
            toolchains: value.toolchains.into(),
            targets: value.targets.into_iter().map(Into::into).collect(),
            retired_targets: value.retired_targets.into_iter().map(Into::into).collect(),
            sandbox: value.sandbox.into(),
            build_root: value.build_root.into(),
            sources: value.sources.into(),
            tuning: value.tuning.into(),
            environment: value.environment.into_iter().map(Into::into).collect(),
            builders: value.builders.into(),
            pgo: value.pgo.into(),
        }
    }
}

impl<T, U> From<GluonValuePatch<T>> for ValuePatch<U>
where
    T: Into<U>,
{
    fn from(value: GluonValuePatch<T>) -> Self {
        match value {
            GluonValuePatch::KeepValue => Self::Keep,
            GluonValuePatch::SetValue(value) => Self::Set(value.into()),
        }
    }
}

impl<T, U> From<GluonArrayPatch<T>> for ArrayPatch<U>
where
    T: Into<U>,
{
    fn from(value: GluonArrayPatch<T>) -> Self {
        let convert = |values: Vec<T>| values.into_iter().map(Into::into).collect();
        match value {
            GluonArrayPatch::KeepArray => Self::Keep,
            GluonArrayPatch::ReplaceArray(values) => Self::Replace(convert(values)),
            GluonArrayPatch::PrependArray(values) => Self::Prepend(convert(values)),
            GluonArrayPatch::AppendArray(values) => Self::Append(convert(values)),
        }
    }
}

impl From<GluonBuildPolicyPatchSpec> for BuildPolicyPatchSpec {
    fn from(value: GluonBuildPolicyPatchSpec) -> Self {
        Self {
            build_subdir: value.build_subdir.into(),
            layout: value.layout.into(),
            toolchains: value.toolchains.into(),
            targets: value.targets.into(),
            retired_targets: value.retired_targets.into(),
            sandbox: value.sandbox.into(),
            build_root: value.build_root.into(),
            sources: value.sources.into(),
            tuning: value.tuning.into(),
            environment: value.environment.into(),
            builders: value.builders.into(),
            pgo: value.pgo.into(),
        }
    }
}

impl From<GluonEnvironmentCondition> for EnvironmentCondition {
    fn from(value: GluonEnvironmentCondition) -> Self {
        match value {
            GluonEnvironmentCondition::Always => Self::Always,
            GluonEnvironmentCondition::CompilerCacheEnabled => Self::CompilerCacheEnabled,
            GluonEnvironmentCondition::CompilerCacheDisabled => Self::CompilerCacheDisabled,
        }
    }
}

impl From<GluonBuildToolSpec> for BuildToolSpec {
    fn from(value: GluonBuildToolSpec) -> Self {
        match value {
            GluonBuildToolSpec::Package { target } => Self::Package(target),
            GluonBuildToolSpec::Binary { target } => Self::Binary(target),
            GluonBuildToolSpec::SystemBinary { target } => Self::SystemBinary(target),
        }
    }
}

impl From<GluonTargetEmulationSpec> for TargetEmulationSpec {
    fn from(value: GluonTargetEmulationSpec) -> Self {
        match value {
            GluonTargetEmulationSpec::Native => Self::Native,
            GluonTargetEmulationSpec::Emul32 { host_architecture } => Self::Emul32 { host_architecture },
        }
    }
}

/// Evaluate a typed policy with the restricted default evaluator.
pub fn evaluate_gluon(source: &Source) -> Result<EvaluatedBuildPolicy, BuildPolicyEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

/// Evaluate a typed policy with caller-selected limits and imports.
pub fn evaluate_gluon_with(
    evaluator: &Evaluator,
    source: &Source,
) -> Result<EvaluatedBuildPolicy, BuildPolicyEvaluationError> {
    evaluate_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate policy and bind host-resolved inputs into its fingerprint.
pub fn evaluate_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedBuildPolicy, BuildPolicyEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.enable_array_primitives();
    import_policy.insert_embedded_module("std.types", GLUON_PURE_TYPES)?;
    import_policy.insert_embedded_module("boulder.build_policy.v1", GLUON_BUILD_POLICY_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonBuildPolicySpec>(source, explicit_inputs)?;

    let policy: BuildPolicySpec = evaluation.value.into();
    policy.validate()?;

    Ok(EvaluatedBuildPolicy {
        policy,
        fingerprint: evaluation.fingerprint,
    })
}

/// Evaluate a total typed policy patch with the restricted default evaluator.
pub fn evaluate_patch_gluon(source: &Source) -> Result<EvaluatedBuildPolicyPatch, BuildPolicyEvaluationError> {
    evaluate_patch_gluon_with(&Evaluator::default(), source)
}

/// Evaluate a total typed policy patch with caller-selected limits and
/// imports.
pub fn evaluate_patch_gluon_with(
    evaluator: &Evaluator,
    source: &Source,
) -> Result<EvaluatedBuildPolicyPatch, BuildPolicyEvaluationError> {
    evaluate_patch_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate a policy patch and bind host-resolved inputs into its fingerprint.
///
/// A patch is intentionally not validated in isolation: `Keep` operations
/// need a concrete base. Call [`BuildPolicyPatchSpec::apply_validated`] before
/// accepting a composed policy.
pub fn evaluate_patch_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedBuildPolicyPatch, BuildPolicyEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.enable_array_primitives();
    import_policy.insert_embedded_module("std.types", GLUON_PURE_TYPES)?;
    import_policy.insert_embedded_module("boulder.build_policy.v1", GLUON_BUILD_POLICY_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonBuildPolicyPatchSpec>(source, explicit_inputs)?;

    Ok(EvaluatedBuildPolicyPatch {
        patch: evaluation.value.into(),
        fingerprint: evaluation.fingerprint,
    })
}
