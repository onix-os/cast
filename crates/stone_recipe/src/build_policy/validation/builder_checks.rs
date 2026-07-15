use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::build_policy::{
    BuildCommandSpec, BuildProgramSpec, BuildToolSpec, BuilderCommandSpec, CompilerFlagsSpec, CompilerToolsSpec,
    EnvironmentBindingSpec, InstallLayoutSpec, PgoStagePolicySpec, StandardBuilderPolicySpec, TextSpec,
    ToolchainFlagsSpec,
};

use super::policy_checks::is_normalized_executable_name;
use super::resource::ResourceValidator;
use super::{BuildPolicyConversionError, BuildPolicyValidationLimits};

impl TextSpec {
    /// Validate one standalone text tree without recursive Rust calls.
    pub fn validate_with_limits(&self, limits: BuildPolicyValidationLimits) -> Result<(), BuildPolicyConversionError> {
        let mut validator = ResourceValidator::new(limits);
        validator.text("text", self)?;
        require_text("text", self)
    }
}

impl BuilderCommandSpec {
    /// Validate a command supplied independently from a complete policy.
    ///
    /// Mason uses this at fragment-taking boundaries so callers cannot bypass
    /// the collection, string, text, or semantic checks applied to commands
    /// embedded in [`crate::build_policy::BuildPolicySpec`].
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

pub(super) fn validate_toolchain_flags(
    field: &str,
    flags: &ToolchainFlagsSpec,
) -> Result<(), BuildPolicyConversionError> {
    validate_compiler_flags(&format!("{field}.common"), &flags.common)?;
    validate_compiler_flags(&format!("{field}.gnu"), &flags.gnu)?;
    validate_compiler_flags(&format!("{field}.llvm"), &flags.llvm)
}

pub(super) fn validate_compiler_flags(
    field: &str,
    flags: &CompilerFlagsSpec,
) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_layout(layout: &InstallLayoutSpec) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_tools_record(field: &str, tools: &CompilerToolsSpec) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_build_command(
    field: &str,
    command: &BuildCommandSpec,
) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_builder(
    field: &str,
    builder: &StandardBuilderPolicySpec,
) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_command(field: &str, command: &BuilderCommandSpec) -> Result<(), BuildPolicyConversionError> {
    validate_program(&format!("{field}.program"), &command.program)?;
    require_text(&format!("{field}.working_dir"), &command.working_dir)?;
    for (index, argument) in command.args.iter().enumerate() {
        require_text(&format!("{field}.args[{index}]"), argument)?;
    }
    validate_bindings(&format!("{field}.environment"), &command.environment)
}

pub(super) fn validate_program(field: &str, program: &BuildProgramSpec) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_bindings(
    field: &str,
    bindings: &[EnvironmentBindingSpec],
) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_tools(field: &str, tools: &[BuildToolSpec]) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn validate_pgo_stage(field: &str, stage: &PgoStagePolicySpec) -> Result<(), BuildPolicyConversionError> {
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

pub(super) fn require_string(field: &str, value: &str) -> Result<(), BuildPolicyConversionError> {
    if value.is_empty() {
        Err(BuildPolicyConversionError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn require_text(field: &str, value: &TextSpec) -> Result<(), BuildPolicyConversionError> {
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
