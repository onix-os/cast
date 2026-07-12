// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use thiserror::Error;

use crate::{Build, Package, Recipe};

// Keep this list aligned with the relation kinds accepted by Moss. Bare
// package names do not use a kind prefix and remain valid as before.
const RELATION_KINDS: &[&str] = &[
    "name",
    "soname",
    "pkgconfig",
    "interpreter",
    "cmake",
    "python",
    "binary",
    "sysbinary",
    "pkgconfig32",
];

/// A format-independent recipe invariant violation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// Versions must start with an ASCII digit so version comparison is well-defined.
    #[error("source.version: version must start with an integer (found `{version}`)")]
    VersionMustStartWithDigit { version: String },
    /// Release zero is never a valid package release.
    #[error("source.release: release must be greater than zero (found `{release}`)")]
    ReleaseMustBePositive { release: u64 },
    /// A dependency string could not be parsed as a package name or a typed
    /// package relation.
    #[error("{field}: invalid dependency `{value}`; expected a package name or supported kind(target)")]
    InvalidDependency { field: String, value: String },
    /// A provider string could not be parsed as a package name or a typed
    /// package relation.
    #[error("{field}: invalid provider `{value}`; expected a package name or supported kind(target)")]
    InvalidProvider { field: String, value: String },
}

impl ValidationError {
    /// Return the stable field path associated with this error.
    pub fn field(&self) -> &str {
        match self {
            Self::VersionMustStartWithDigit { .. } => "source.version",
            Self::ReleaseMustBePositive { .. } => "source.release",
            Self::InvalidDependency { field, .. } | Self::InvalidProvider { field, .. } => field,
        }
    }
}

/// Validate invariants shared by every recipe input format.
pub fn validate(recipe: &Recipe) -> Result<(), ValidationError> {
    if !recipe
        .source
        .version
        .starts_with(|character: char| character.is_ascii_digit())
    {
        return Err(ValidationError::VersionMustStartWithDigit {
            version: recipe.source.version.clone(),
        });
    }

    if recipe.source.release == 0 {
        return Err(ValidationError::ReleaseMustBePositive {
            release: recipe.source.release,
        });
    }

    validate_build(&recipe.build, "build")?;
    validate_package(&recipe.package, "package")?;

    for (index, profile) in recipe.profiles.iter().enumerate() {
        validate_build(&profile.value, &format!("profiles[{index}].value"))?;
    }
    for (index, package) in recipe.sub_packages.iter().enumerate() {
        validate_package(&package.value, &format!("sub_packages[{index}].value"))?;
    }

    Ok(())
}

/// Validate the dependency relations held by one build definition.
pub fn validate_build(build: &Build, field: &str) -> Result<(), ValidationError> {
    validate_dependencies(&build.build_deps, &format!("{field}.build_deps"))?;
    validate_dependencies(&build.check_deps, &format!("{field}.check_deps"))
}

/// Validate the dependency and provider relations held by one fully resolved
/// package definition.
pub fn validate_package(package: &Package, field: &str) -> Result<(), ValidationError> {
    validate_dependencies(&package.run_deps, &format!("{field}.run_deps"))?;
    validate_providers(&package.conflicts, &format!("{field}.conflicts"))
}

pub(crate) fn validate_dependencies(values: &[String], field: &str) -> Result<(), ValidationError> {
    for (index, value) in values.iter().enumerate() {
        if !valid_relation(value) {
            return Err(ValidationError::InvalidDependency {
                field: format!("{field}[{index}]"),
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn validate_providers(values: &[String], field: &str) -> Result<(), ValidationError> {
    for (index, value) in values.iter().enumerate() {
        if !valid_relation(value) {
            return Err(ValidationError::InvalidProvider {
                field: format!("{field}[{index}]"),
                value: value.clone(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_package_templates(package: &Package, field: &str) -> Result<(), ValidationError> {
    validate_relation_templates(
        &package.run_deps,
        &format!("{field}.run_deps"),
        RelationRole::Dependency,
    )?;
    validate_relation_templates(
        &package.conflicts,
        &format!("{field}.conflicts"),
        RelationRole::Provider,
    )
}

#[derive(Clone, Copy)]
enum RelationRole {
    Dependency,
    Provider,
}

fn validate_relation_templates(values: &[String], field: &str, role: RelationRole) -> Result<(), ValidationError> {
    for (index, value) in values.iter().enumerate() {
        let valid = normalize_deferred_relation(value).is_some_and(|normalized| valid_relation(&normalized));
        if !valid {
            let field = format!("{field}[{index}]");
            return Err(match role {
                RelationRole::Dependency => ValidationError::InvalidDependency {
                    field,
                    value: value.clone(),
                },
                RelationRole::Provider => ValidationError::InvalidProvider {
                    field,
                    value: value.clone(),
                },
            });
        }
    }
    Ok(())
}

fn normalize_deferred_relation(value: &str) -> Option<String> {
    let mut normalized = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some(start) = remaining.find("%(") {
        normalized.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let end = after_start.find(')')?;
        let identifier = &after_start[..end];
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        normalized.push_str("template");
        remaining = &after_start[end + 1..];
    }

    normalized.push_str(remaining);
    Some(normalized)
}

fn valid_relation(value: &str) -> bool {
    if !value.contains('(') {
        return true;
    }

    let Some((kind, target)) = value.split_once('(') else {
        return false;
    };
    target.ends_with(')') && RELATION_KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuildSpec, KeyValueSpec, PackageSpec, RecipeSpec, SourceSpec};

    fn recipe_spec() -> RecipeSpec {
        RecipeSpec::new(SourceSpec {
            name: "example".to_owned(),
            version: "1.0.0".to_owned(),
            release: 1,
            homepage: "https://example.com".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        })
    }

    #[test]
    fn recipe_spec_uses_shared_validation() {
        let mut spec = recipe_spec();
        spec.source.version = "v1.0.0".to_owned();
        let error = Recipe::try_from(spec).unwrap_err();

        assert_eq!(error.field(), "source.version");
        assert!(matches!(
            error,
            crate::RecipeConversionError::Validation(ValidationError::VersionMustStartWithDigit { .. })
        ));
    }

    #[test]
    fn every_recipe_relation_field_is_validated_with_its_indexed_path() {
        let cases: [(&str, fn(&mut RecipeSpec)); 8] = [
            ("build.build_deps[1]", |spec| {
                spec.build.build_deps = vec!["valid".to_owned(), "unknown(target)".to_owned()];
            }),
            ("build.check_deps[1]", |spec| {
                spec.build.check_deps = vec!["valid".to_owned(), "binary(unclosed".to_owned()];
            }),
            ("package.run_deps[1]", |spec| {
                spec.package.run_deps = vec!["valid".to_owned(), "unknown(target)".to_owned()];
            }),
            ("package.conflicts[1]", |spec| {
                spec.package.conflicts = vec!["valid".to_owned(), "binary(unclosed".to_owned()];
            }),
            ("profiles[0].value.build_deps[1]", |spec| {
                spec.profiles.push(KeyValueSpec {
                    key: "native".to_owned(),
                    value: BuildSpec {
                        build_deps: vec!["valid".to_owned(), "unknown(target)".to_owned()],
                        ..BuildSpec::default()
                    },
                });
            }),
            ("profiles[0].value.check_deps[1]", |spec| {
                spec.profiles.push(KeyValueSpec {
                    key: "native".to_owned(),
                    value: BuildSpec {
                        check_deps: vec!["valid".to_owned(), "binary(unclosed".to_owned()],
                        ..BuildSpec::default()
                    },
                });
            }),
            ("sub_packages[0].value.run_deps[1]", |spec| {
                spec.sub_packages.push(KeyValueSpec {
                    key: "example-devel".to_owned(),
                    value: PackageSpec {
                        run_deps: vec!["valid".to_owned(), "unknown(target)".to_owned()],
                        ..PackageSpec::default()
                    },
                });
            }),
            ("sub_packages[0].value.conflicts[1]", |spec| {
                spec.sub_packages.push(KeyValueSpec {
                    key: "example-devel".to_owned(),
                    value: PackageSpec {
                        conflicts: vec!["valid".to_owned(), "binary(unclosed".to_owned()],
                        ..PackageSpec::default()
                    },
                });
            }),
        ];

        for (expected_field, mutate) in cases {
            let mut spec = recipe_spec();
            mutate(&mut spec);
            let error = Recipe::try_from(spec).unwrap_err();
            assert_eq!(error.field(), expected_field);
        }
    }

    #[test]
    fn relation_validation_preserves_every_existing_kind_and_bare_names() {
        let mut spec = recipe_spec();
        spec.build.build_deps = [
            "plain-package",
            "name(package)",
            "soname(libexample.so.1)",
            "pkgconfig(example)",
            "interpreter(/bin/sh)",
            "cmake(Example)",
            "python(example)",
            "binary(example)",
            "sysbinary(example)",
            "pkgconfig32(example)",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        spec.package.conflicts = spec.build.build_deps.clone();

        Recipe::try_from(spec).unwrap();
    }

    #[test]
    fn macro_package_templates_are_explicitly_deferred_but_not_recipe_values() {
        let package = Package::from(PackageSpec {
            run_deps: vec!["%(name)-devel".to_owned(), "binary(%(tool))".to_owned()],
            conflicts: vec!["%(name)-legacy".to_owned()],
            ..PackageSpec::default()
        });

        validate_package_templates(&package, "packages[0].value").unwrap();
        let strict = validate_package(&package, "package").unwrap_err();
        assert_eq!(strict.field(), "package.run_deps[0]");

        for invalid in ["unknown(%(name))", "binary(%(name)", "%(not-valid)"] {
            let package = Package::from(PackageSpec {
                run_deps: vec![invalid.to_owned()],
                ..PackageSpec::default()
            });
            let error = validate_package_templates(&package, "packages[0].value").unwrap_err();
            assert_eq!(error.field(), "packages[0].value.run_deps[0]");
        }
    }
}
