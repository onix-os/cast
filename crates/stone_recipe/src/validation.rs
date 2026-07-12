// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use thiserror::Error;

use crate::Recipe;

/// A format-independent recipe invariant violation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// Versions must start with an ASCII digit so version comparison is well-defined.
    #[error("source.version: version must start with an integer (found `{version}`)")]
    VersionMustStartWithDigit { version: String },
    /// Release zero is never a valid package release.
    #[error("source.release: release must be greater than zero (found `{release}`)")]
    ReleaseMustBePositive { release: u64 },
}

impl ValidationError {
    /// Return the stable field path associated with this error.
    pub fn field(&self) -> &'static str {
        match self {
            Self::VersionMustStartWithDigit { .. } => "source.version",
            Self::ReleaseMustBePositive { .. } => "source.release",
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_yaml_recipe_uses_shared_validation() {
        let recipe = crate::from_str(
            r#"
name: example
version: v1.0.0
release: 1
homepage: https://example.com
license: MPL-2.0
"#,
        )
        .unwrap();

        let error = recipe.validate().unwrap_err();

        assert_eq!(error.field(), "source.version");
        assert!(matches!(error, ValidationError::VersionMustStartWithDigit { .. }));
    }
}
