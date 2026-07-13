// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use stone::relation::{Dependency, ParseError};
use thiserror::Error;

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
}

impl ValidationError {
    /// Return the stable field path associated with this error.
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidDependency { field, .. } => field,
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
}
