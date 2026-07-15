use std::path::{Component, Path, PathBuf};

use super::{DerivationValidationError, field_checks::reject_embedded_nul};

pub(super) fn validate_normalized_absolute_path<'a>(
    field: &str,
    value: &'a str,
) -> Result<&'a Path, DerivationValidationError> {
    reject_embedded_nul(field, value)?;
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
    if !path.is_absolute() || normal_components == 0 || !safe_components || normalized.as_os_str() != path.as_os_str() {
        return Err(DerivationValidationError::UnsafeAbsolutePath {
            field: field.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(path)
}

pub(super) fn require_path_contained(
    field: &str,
    path: &Path,
    root_field: &str,
    root: &Path,
) -> Result<(), DerivationValidationError> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(DerivationValidationError::PathOutsideRoot {
            field: field.to_owned(),
            value: path.display().to_string(),
            root_field: root_field.to_owned(),
            root: root.display().to_string(),
        })
    }
}

pub(super) fn require_proper_path_child(
    field: &str,
    path: &Path,
    root_field: &str,
    root: &Path,
) -> Result<(), DerivationValidationError> {
    if path != root && path.starts_with(root) {
        Ok(())
    } else {
        Err(DerivationValidationError::PathOutsideRoot {
            field: field.to_owned(),
            value: path.display().to_string(),
            root_field: root_field.to_owned(),
            root: root.display().to_string(),
        })
    }
}

pub(super) fn validate_sandbox_hostname(value: &str) -> Result<(), DerivationValidationError> {
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
        Err(DerivationValidationError::InvalidSandboxHostname {
            value: value.to_owned(),
        })
    }
}

pub(super) fn reject_layout_path_overlaps(paths: &[(&str, &str)]) -> Result<(), DerivationValidationError> {
    for (index, (field, value)) in paths.iter().enumerate() {
        for (other_field, other) in &paths[..index] {
            if Path::new(value).starts_with(other) || Path::new(other).starts_with(value) {
                return Err(DerivationValidationError::OverlappingLayoutPath {
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

pub(super) fn validate_step_working_dir(
    step_field: &str,
    working_dir: &str,
    build_dir: &Path,
) -> Result<(), DerivationValidationError> {
    let field = format!("{step_field}.working_dir");
    let working_dir = validate_normalized_absolute_path(&field, working_dir)?;
    require_path_contained(&field, working_dir, "job build_dir", build_dir)
}
