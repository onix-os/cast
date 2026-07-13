// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed repository policy used to lower package builders into frozen plans.
//!
//! Policy text is deliberately finite: authored values may only contain
//! literals and references to known planning inputs. There is no general
//! string interpolation or action expansion at this boundary.

use std::collections::BTreeSet;

use thiserror::Error;

pub use self::gluon::{
    BUILD_POLICY_ABI_VERSION, BuildPolicyEvaluationError, EvaluatedBuildPolicy, GLUON_BUILD_POLICY_ABI, evaluate_gluon,
    evaluate_gluon_with, evaluate_gluon_with_inputs,
};

mod gluon;

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

/// One concrete build/host/target platform policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPolicySpec {
    pub name: String,
    pub target_triple: String,
    pub build_platform: String,
    pub host_platform: String,
    pub lib_suffix: String,
    pub architecture_flags: CompilerFlagsSpec,
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
    Binary(String),
    SystemBinary(String),
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
    pub environment: Vec<EnvironmentBindingSpec>,
    pub builders: BuildersPolicySpec,
    pub pgo: PgoPolicySpec,
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

        let mut targets = BTreeSet::new();
        for (index, target) in self.targets.iter().enumerate() {
            let field = format!("targets[{index}]");
            require_string(&format!("{field}.name"), &target.name)?;
            require_string(&format!("{field}.target_triple"), &target.target_triple)?;
            require_string(&format!("{field}.build_platform"), &target.build_platform)?;
            require_string(&format!("{field}.host_platform"), &target.host_platform)?;
            if !targets.insert(target.name.as_str()) {
                return Err(BuildPolicyConversionError::Duplicate {
                    field: "targets".to_owned(),
                    value: target.name.clone(),
                });
            }
        }

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
        validate_pgo_stage("pgo.stage_one", &self.pgo.stage_one)?;
        validate_pgo_stage("pgo.stage_two", &self.pgo.stage_two)?;
        validate_pgo_stage("pgo.use_profile", &self.pgo.use_profile)
    }
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
            BuildToolSpec::Binary(target) | BuildToolSpec::SystemBinary(target) => target,
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
