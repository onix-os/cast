// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use stone::relation::{Dependency, ParseError, Provider};
use thiserror::Error;

use crate::OutputTemplateSpec;

/// A format-independent relation invariant violation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// A dependency string could not be parsed as a package name or a typed
    /// package relation.
    #[error("{field}: invalid dependency `{value}`: {source}")]
    InvalidDependency {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
    /// A provider string could not be parsed as a package name or a typed
    /// package relation.
    #[error("{field}: invalid provider `{value}`: {source}")]
    InvalidProvider {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
}

impl ValidationError {
    /// Return the stable field path associated with this error.
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidDependency { field, .. } | Self::InvalidProvider { field, .. } => field,
        }
    }
}

pub(crate) fn validate_dependencies(values: &[String], field: &str) -> Result<(), ValidationError> {
    for (index, value) in values.iter().enumerate() {
        Dependency::from_name(value).map_err(|source| ValidationError::InvalidDependency {
            field: format!("{field}[{index}]"),
            value: value.clone(),
            source,
        })?;
    }
    Ok(())
}

pub(crate) fn validate_package_templates(package: &OutputTemplateSpec, field: &str) -> Result<(), ValidationError> {
    validate_relation_templates(
        &package.runtime_inputs,
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
        let normalized = normalize_deferred_relation(value);
        let parsed = normalized
            .as_deref()
            .ok_or_else(|| ParseError::Malformed { value: value.clone() });
        let result = parsed.and_then(|normalized| match role {
            RelationRole::Dependency => Dependency::from_name(normalized).map(|_| ()),
            RelationRole::Provider => Provider::from_name(normalized).map(|_| ()),
        });
        if let Err(source) = result {
            let field = format!("{field}[{index}]");
            return Err(match role {
                RelationRole::Dependency => ValidationError::InvalidDependency {
                    field,
                    value: value.clone(),
                    source,
                },
                RelationRole::Provider => ValidationError::InvalidProvider {
                    field,
                    value: value.clone(),
                    source,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_relations_preserve_existing_kinds_and_indexed_errors() {
        let valid = [
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
        .map(str::to_owned);

        validate_dependencies(&valid, "dependencies").unwrap();
        let dependencies = ["valid".to_owned(), "unknown(target)".to_owned()];
        let error = validate_dependencies(&dependencies, "dependencies").unwrap_err();
        assert_eq!(error.field(), "dependencies[1]");
    }

    #[test]
    fn package_templates_defer_explicit_placeholders_only() {
        let template = OutputTemplateSpec {
            runtime_inputs: vec!["%(name)-devel".to_owned(), "binary(%(tool))".to_owned()],
            conflicts: vec!["%(name)-legacy".to_owned()],
            ..OutputTemplateSpec::default()
        };

        validate_package_templates(&template, "packages[0].value").unwrap();

        for invalid in ["unknown(%(name))", "binary(%(name)", "%(not-valid)"] {
            let template = OutputTemplateSpec {
                runtime_inputs: vec![invalid.to_owned()],
                ..OutputTemplateSpec::default()
            };
            let error = validate_package_templates(&template, "packages[0].value").unwrap_err();
            assert_eq!(error.field(), "packages[0].value.run_deps[0]");
        }
    }
}
