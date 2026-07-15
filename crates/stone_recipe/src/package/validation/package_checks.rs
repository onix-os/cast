use std::collections::{BTreeMap, BTreeSet};

use crate::{
    PathSpec,
    package::{BuilderSpec, HooksSpec, PackageSpec},
    spec::{is_normalized_relative_path, is_safe_artifact_component},
};
use url::Url;

use super::{
    PackageConversionError, PackageValidationLimits,
    budget::PackageBudget,
    field_checks::{valid_package_name, valid_profile_name, validate_trimmed_text},
};

impl PackageSpec {
    /// Validate the concrete package declaration without lowering it through
    /// the transitional recipe model.
    pub fn validate(&self) -> Result<(), PackageConversionError> {
        self.validate_with_limits(PackageValidationLimits::default())
    }

    /// Validate with an explicit post-evaluation resource budget.
    pub fn validate_with_limits(&self, limits: PackageValidationLimits) -> Result<(), PackageConversionError> {
        PackageBudget::new(limits).validate(self)?;
        if !valid_package_name(&self.meta.pname) {
            return Err(PackageConversionError::InvalidPackageName {
                name: self.meta.pname.clone(),
            });
        }
        if !self
            .meta
            .version
            .starts_with(|character: char| character.is_ascii_digit())
        {
            return Err(PackageConversionError::VersionMustStartWithDigit {
                version: self.meta.version.clone(),
            });
        }
        if !is_safe_artifact_component(&self.meta.version) {
            return Err(PackageConversionError::InvalidVersionComponent {
                version: self.meta.version.clone(),
            });
        }
        if self.meta.release <= 0 {
            return Err(PackageConversionError::ReleaseMustBePositive {
                release: self.meta.release,
            });
        }
        self.validate_metadata()?;
        self.validate_selectors()?;
        self.validate_outputs()?;
        if self.options.networking {
            return Err(PackageConversionError::FrozenBuildNetworkingUnsupported);
        }

        let mut source_destinations = BTreeMap::<String, (usize, &'static str)>::new();
        for (index, source) in self.sources.iter().enumerate() {
            source
                .validate()
                .map_err(|source_error| PackageConversionError::InvalidSource {
                    field: format!("sources[{index}].{}", source_error.field()),
                    source: source_error,
                })?;
            let destination =
                source
                    .materialization_name()
                    .map_err(|source_error| PackageConversionError::InvalidSource {
                        field: format!("sources[{index}].{}", source_error.field()),
                        source: source_error,
                    })?;
            let destination_field = source.materialization_field();
            if let Some((first_index, first_destination_field)) =
                source_destinations.insert(destination.clone(), (index, destination_field))
            {
                return Err(PackageConversionError::DuplicateSourceMaterialization {
                    field: format!("sources[{index}].{destination_field}"),
                    value: destination,
                    first_field: format!("sources[{first_index}].{first_destination_field}"),
                });
            }
        }

        let mut profile_names = BTreeMap::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            if !valid_profile_name(&profile.name) {
                return Err(PackageConversionError::InvalidProfileName {
                    index,
                    name: profile.name.clone(),
                });
            }
            if let Some(first_index) = profile_names.insert(profile.name.as_str(), index) {
                return Err(PackageConversionError::DuplicateProfileName {
                    first_index,
                    duplicate_index: index,
                    name: profile.name.clone(),
                });
            }
        }

        Self::validate_builder_contract(&self.builder, &self.hooks, "builder", "hooks")?;
        for (index, profile) in self.profiles.iter().enumerate() {
            Self::validate_builder_contract(
                &profile.builder,
                &profile.hooks,
                &format!("profiles[{index}].builder"),
                &format!("profiles[{index}].hooks"),
            )?;
        }

        self.validate_relations()
    }

    fn validate_metadata(&self) -> Result<(), PackageConversionError> {
        let homepage = Url::parse(&self.meta.homepage).map_err(|source| PackageConversionError::InvalidHomepage {
            value: self.meta.homepage.clone(),
            source,
        })?;
        if !matches!(homepage.scheme(), "http" | "https") || !homepage.has_host() {
            return Err(PackageConversionError::UnsupportedHomepage {
                value: self.meta.homepage.clone(),
            });
        }
        if !homepage.username().is_empty() || homepage.password().is_some() {
            return Err(PackageConversionError::HomepageCredentials);
        }

        if self.meta.license.is_empty() {
            return Err(PackageConversionError::InvalidText {
                field: "meta.license".to_owned(),
                value: String::new(),
                requirement: "must declare at least one license expression",
            });
        }
        let mut licenses = BTreeMap::new();
        for (index, license) in self.meta.license.iter().enumerate() {
            let field = format!("meta.license[{index}]");
            validate_trimmed_text(
                &field,
                license,
                "must be non-empty, trimmed, and contain no control characters",
            )?;
            if let Some(first_index) = licenses.insert(license.as_str(), index) {
                return Err(PackageConversionError::DuplicateValue {
                    field,
                    value: license.clone(),
                    first_field: format!("meta.license[{first_index}]"),
                });
            }
        }
        Ok(())
    }

    fn validate_selectors(&self) -> Result<(), PackageConversionError> {
        let mut architectures = BTreeMap::new();
        for (index, architecture) in self.architectures.iter().enumerate() {
            let field = format!("architectures[{index}]");
            if !is_normalized_relative_path(architecture) {
                return Err(PackageConversionError::InvalidText {
                    field,
                    value: architecture.clone(),
                    requirement: "must be a normalized portable target name",
                });
            }
            if let Some(first_index) = architectures.insert(architecture.as_str(), index) {
                return Err(PackageConversionError::DuplicateValue {
                    field,
                    value: architecture.clone(),
                    first_field: format!("architectures[{first_index}]"),
                });
            }
        }

        let mut tuning = BTreeMap::new();
        for (index, entry) in self.tuning.iter().enumerate() {
            let field = format!("tuning[{index}].key");
            if !valid_package_name(&entry.key) {
                return Err(PackageConversionError::InvalidText {
                    field,
                    value: entry.key.clone(),
                    requirement: "must be a normalized tuning-group name",
                });
            }
            if let Some(first_index) = tuning.insert(entry.key.as_str(), index) {
                return Err(PackageConversionError::DuplicateValue {
                    field,
                    value: entry.key.clone(),
                    first_field: format!("tuning[{first_index}].key"),
                });
            }
            if let crate::TuningSpec::Config { value } = &entry.value
                && !valid_package_name(value)
            {
                return Err(PackageConversionError::InvalidText {
                    field: format!("tuning[{index}].value"),
                    value: value.clone(),
                    requirement: "must be a normalized tuning-choice name",
                });
            }
        }
        Ok(())
    }

    fn validate_outputs(&self) -> Result<(), PackageConversionError> {
        for (output_index, output) in self.outputs.iter().enumerate() {
            for (name, value) in [
                ("summary", output.summary.as_deref()),
                ("description", output.description.as_deref()),
            ] {
                if let Some(value) = value
                    && value.contains('\0')
                {
                    return Err(PackageConversionError::InvalidText {
                        field: format!("outputs[{output_index}].{name}"),
                        value: value.to_owned(),
                        requirement: "must not contain NUL characters",
                    });
                }
            }

            for (name, patterns) in [
                ("provides_exclude", &output.provides_exclude),
                ("runtime_exclude", &output.runtime_exclude),
            ] {
                let mut seen = BTreeMap::new();
                for (pattern_index, pattern) in patterns.iter().enumerate() {
                    let field = format!("outputs[{output_index}].{name}[{pattern_index}]");
                    regex::Regex::new(pattern).map_err(|source| PackageConversionError::InvalidRegex {
                        field: field.clone(),
                        value: pattern.clone(),
                        source: Box::new(source),
                    })?;
                    if let Some(first_index) = seen.insert(pattern.as_str(), pattern_index) {
                        return Err(PackageConversionError::DuplicateValue {
                            field,
                            value: pattern.clone(),
                            first_field: format!("outputs[{output_index}].{name}[{first_index}]"),
                        });
                    }
                }
            }

            let mut paths = BTreeMap::new();
            for (path_index, rule) in output.paths.iter().enumerate() {
                let (kind, pattern) = match rule {
                    PathSpec::Any { path } => ("any", path),
                    PathSpec::Exe { path } => ("exe", path),
                    PathSpec::Symlink { path } => ("symlink", path),
                    PathSpec::Special { path } => ("special", path),
                };
                let field = format!("outputs[{output_index}].paths[{path_index}].path");
                validate_trimmed_text(&field, pattern, "must be a non-empty glob without control characters")?;
                glob::Pattern::new(pattern).map_err(|source| PackageConversionError::InvalidGlob {
                    field: field.clone(),
                    value: pattern.clone(),
                    source,
                })?;
                if let Some(first_index) = paths.insert((kind, pattern.as_str()), path_index) {
                    return Err(PackageConversionError::DuplicateValue {
                        field,
                        value: format!("{kind}:{pattern}"),
                        first_field: format!("outputs[{output_index}].paths[{first_index}].path"),
                    });
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_builder_contract(
        builder: &BuilderSpec,
        hooks: &HooksSpec,
        builder_field: &str,
        hooks_field: &str,
    ) -> Result<(), PackageConversionError> {
        let mut environments = BTreeSet::new();
        for (index, environment) in builder.environment.iter().copied().enumerate() {
            if !environments.insert(environment) {
                return Err(PackageConversionError::DuplicateBuilderEnvironment {
                    field: format!("{builder_field}.environment[{index}]"),
                    environment,
                });
            }
        }

        for (field, supported, populated) in [
            ("pre_setup", builder.supported_hooks.setup, !hooks.pre_setup.is_empty()),
            (
                "post_setup",
                builder.supported_hooks.setup,
                !hooks.post_setup.is_empty(),
            ),
            ("pre_build", builder.supported_hooks.build, !hooks.pre_build.is_empty()),
            (
                "post_build",
                builder.supported_hooks.build,
                !hooks.post_build.is_empty(),
            ),
            ("pre_check", builder.supported_hooks.check, !hooks.pre_check.is_empty()),
            (
                "post_check",
                builder.supported_hooks.check,
                !hooks.post_check.is_empty(),
            ),
            (
                "pre_install",
                builder.supported_hooks.install,
                !hooks.pre_install.is_empty(),
            ),
            (
                "post_install",
                builder.supported_hooks.install,
                !hooks.post_install.is_empty(),
            ),
            (
                "pre_workload",
                builder.supported_hooks.workload,
                !hooks.pre_workload.is_empty(),
            ),
            (
                "post_workload",
                builder.supported_hooks.workload,
                !hooks.post_workload.is_empty(),
            ),
        ] {
            if populated && !supported {
                return Err(PackageConversionError::UnsupportedBuilderHook {
                    field: format!("{hooks_field}.{field}"),
                });
            }
        }

        Ok(())
    }
}
