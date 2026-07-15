use crate::{
    package::valid_package_name,
    spec::{SourceUrlKind, is_safe_artifact_component, validate_source_url},
};

use super::DerivationValidationError;

pub(super) fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

pub(super) fn reject_embedded_nul(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if value.as_bytes().contains(&0) {
        Err(DerivationValidationError::EmbeddedNul {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn require_nonempty(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if value.is_empty() {
        Err(DerivationValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(in crate::derivation) fn require_nonblank(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if value.trim().is_empty() {
        Err(DerivationValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn validate_package_name(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if valid_package_name(value) {
        Ok(())
    } else {
        Err(DerivationValidationError::InvalidPackageName {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

pub(super) fn validate_artifact_component(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if is_safe_artifact_component(value) {
        Ok(())
    } else {
        Err(DerivationValidationError::InvalidArtifactComponent {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

pub(super) fn validate_regex(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    regex::Regex::new(value)
        .map(|_| ())
        .map_err(|source| DerivationValidationError::InvalidRegex {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

pub(super) fn validate_glob(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    glob::Pattern::new(value)
        .map(|_| ())
        .map_err(|source| DerivationValidationError::InvalidGlob {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

pub(super) fn validate_url(index: usize, kind: SourceUrlKind, value: &str) -> Result<(), DerivationValidationError> {
    validate_source_url(kind, value)
        .map(drop)
        .map_err(|source| DerivationValidationError::InvalidSourceUrl { index, source })
}

pub(super) fn validate_source_destination(
    index: usize,
    field: &'static str,
    value: &str,
) -> Result<(), DerivationValidationError> {
    if !is_safe_artifact_component(value) {
        return Err(DerivationValidationError::UnsafeSourceDestination {
            index,
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}
