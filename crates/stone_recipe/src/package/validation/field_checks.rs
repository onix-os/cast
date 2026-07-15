use std::collections::BTreeMap;

use crate::spec::is_normalized_relative_path;

use super::PackageConversionError;

pub(crate) fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_'))
}

pub(super) fn validate_trimmed_text(
    field: &str,
    value: &str,
    requirement: &'static str,
) -> Result<(), PackageConversionError> {
    if !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(PackageConversionError::InvalidText {
            field: field.to_owned(),
            value: value.to_owned(),
            requirement,
        })
    }
}

pub(super) fn validate_unique_step_values(
    values: &[String],
    field: &str,
    requirement: &'static str,
    normalized_name: bool,
) -> Result<(), PackageConversionError> {
    let mut seen = BTreeMap::new();
    for (index, value) in values.iter().enumerate() {
        let value_field = format!("{field}[{index}]");
        if normalized_name {
            if !valid_package_name(value) {
                return Err(PackageConversionError::InvalidText {
                    field: value_field,
                    value: value.clone(),
                    requirement,
                });
            }
        } else {
            validate_trimmed_text(&value_field, value, requirement)?;
        }
        if let Some(first_index) = seen.insert(value.as_str(), index) {
            return Err(PackageConversionError::DuplicateValue {
                field: value_field,
                value: value.clone(),
                first_field: format!("{field}[{first_index}]"),
            });
        }
    }
    Ok(())
}

pub(super) fn valid_output_name(name: &str) -> bool {
    valid_package_name(name)
}

pub(super) fn valid_profile_name(name: &str) -> bool {
    is_normalized_relative_path(name)
}

pub(super) fn valid_program_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path[1..]
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}
