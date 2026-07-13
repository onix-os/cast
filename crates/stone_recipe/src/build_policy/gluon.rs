// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Restricted Gluon evaluation boundary for typed build policy.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};
use thiserror::Error;

use super::{
    BuildPolicyConversionError, BuildPolicySpec, BuildToolSpec, BuilderCommandSpec, BuildersPolicySpec,
    CompilerFlagsSpec, CompilerToolsSpec, ContextValue, EnvironmentBindingSpec, EnvironmentCondition,
    InstallLayoutSpec, PgoFinishSpec, PgoPolicySpec, PgoStagePolicySpec, StandardBuilderPolicySpec, TargetPolicySpec,
    TextSpec, ToolchainFlagsSpec, ToolchainsSpec,
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
    mold_ld: GluonTextSpec,
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
struct GluonTargetPolicySpec {
    name: String,
    target_triple: String,
    build_platform: String,
    host_platform: String,
    lib_suffix: String,
    architecture_flags: GluonCompilerFlagsSpec,
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
    Binary { target: String },
    SystemBinary { target: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuilderCommandSpec {
    program: GluonTextSpec,
    args: Vec<GluonTextSpec>,
    environment: Vec<GluonEnvironmentBindingSpec>,
    working_dir: GluonTextSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonStandardBuilderPolicySpec {
    required_tools: Vec<GluonBuildToolSpec>,
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
    sample: GluonToolchainFlagsSpec,
    stage_one: GluonPgoStagePolicySpec,
    stage_two: GluonPgoStagePolicySpec,
    use_profile: GluonPgoStagePolicySpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicySpec {
    vendor_id: String,
    build_subdir: String,
    layout: GluonInstallLayoutSpec,
    toolchains: GluonToolchainsSpec,
    targets: Vec<GluonTargetPolicySpec>,
    environment: Vec<GluonEnvironmentBindingSpec>,
    builders: GluonBuildersPolicySpec,
    pgo: GluonPgoPolicySpec,
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
    cc, cxx, objc, objcxx, cpp, objcpp, objcxxcpp, d, ar, ld, mold_ld, objcopy, nm, ranlib, strip,
});
convert_record!(GluonToolchainsSpec => ToolchainsSpec { llvm, gnu });
convert_record!(GluonTargetPolicySpec => TargetPolicySpec {
    name, target_triple, build_platform, host_platform, lib_suffix, architecture_flags,
});
convert_record!(GluonEnvironmentBindingSpec => EnvironmentBindingSpec { name, value, condition });
convert_record!(GluonBuildersPolicySpec => BuildersPolicySpec { cmake, meson, cargo, autotools });
convert_record!(GluonToolchainFlagsSpec => ToolchainFlagsSpec { common, gnu, llvm });

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

impl From<GluonStandardBuilderPolicySpec> for StandardBuilderPolicySpec {
    fn from(value: GluonStandardBuilderPolicySpec) -> Self {
        Self {
            required_tools: value.required_tools.into_iter().map(Into::into).collect(),
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
            vendor_id: value.vendor_id,
            build_subdir: value.build_subdir,
            layout: value.layout.into(),
            toolchains: value.toolchains.into(),
            targets: value.targets.into_iter().map(Into::into).collect(),
            environment: value.environment.into_iter().map(Into::into).collect(),
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
            GluonBuildToolSpec::Binary { target } => Self::Binary(target),
            GluonBuildToolSpec::SystemBinary { target } => Self::SystemBinary(target),
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
