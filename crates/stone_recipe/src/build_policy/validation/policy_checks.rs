use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::build_policy::{
    AnalyzerKind, AnalyzerToolsPolicySpec, BuildPolicySpec, BuildRootPolicySpec, BuildToolSpec, PlatformPolicySpec,
    SUPPORTED_ARTIFACT_ARCHITECTURES, SandboxPolicySpec, SourcePreparationPolicySpec, TargetEmulationSpec,
    TargetPolicySpec, ToolchainInputPolicySpec,
};

use super::builder_checks::{
    require_string, require_text, validate_bindings, validate_build_command, validate_builder, validate_command,
    validate_compiler_flags, validate_layout, validate_pgo_stage, validate_program, validate_toolchain_flags,
    validate_tools, validate_tools_record,
};
use super::resource::ResourceValidator;
use super::tuning_checks::validate_tuning;
use super::{BuildPolicyConversionError, BuildPolicyValidationLimits};

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

pub(super) fn is_normalized_executable_name(value: &str) -> bool {
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
