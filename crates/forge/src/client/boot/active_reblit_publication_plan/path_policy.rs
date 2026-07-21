//! Canonical, FAT-safe publication-path admission.
//!
//! The descriptor publisher owns `.cast-payload-` names in every destination
//! parent. Canonical plan components reserve that prefix case-insensitively so
//! no output can alias this or another request's deterministic private stage.

use std::{os::unix::ffi::OsStrExt as _, path::{Path, PathBuf}};

use super::{
    ActiveReblitBootPublicationPlanError, MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES,
    PublicationPlanBudget,
};
use crate::linux_fs::is_retained_boot_file_private_component;

pub(super) fn require_normalized_relative_path(
    path: &Path,
    budget: &mut PublicationPlanBudget,
) -> Result<PathBuf, ActiveReblitBootPublicationPlanError> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return Err(ActiveReblitBootPublicationPlanError::EmptyPath);
    }
    if bytes.contains(&0) {
        return Err(ActiveReblitBootPublicationPlanError::NulPath { path: path.to_owned() });
    }
    let Some(text) = path.to_str() else {
        return Err(ActiveReblitBootPublicationPlanError::NonUtf8Path { path: path.to_owned() });
    };
    if path.is_absolute() || text.starts_with('/') {
        return Err(ActiveReblitBootPublicationPlanError::AbsolutePath { path: path.to_owned() });
    }
    if bytes.len() > budget.policy.max_single_path_bytes {
        return Err(ActiveReblitBootPublicationPlanError::SinglePathByteLimit {
            path: path.to_owned(),
            limit: budget.policy.max_single_path_bytes,
            actual: bytes.len(),
        });
    }

    let mut component_count = 0usize;
    for component in text.split('/') {
        budget.step()?;
        component_count = component_count.saturating_add(1);
        if component_count > budget.policy.max_components {
            return Err(ActiveReblitBootPublicationPlanError::PathComponentLimit {
                path: path.to_owned(),
                limit: budget.policy.max_components,
                actual: component_count,
            });
        }
        if component.is_empty() {
            return Err(ActiveReblitBootPublicationPlanError::EmptyPathComponent { path: path.to_owned() });
        }
        if component == "." {
            return Err(ActiveReblitBootPublicationPlanError::DotPathComponent { path: path.to_owned() });
        }
        if component == ".." {
            return Err(ActiveReblitBootPublicationPlanError::ParentPathComponent { path: path.to_owned() });
        }
        if component.chars().any(char::is_control) {
            return Err(ActiveReblitBootPublicationPlanError::ControlPathComponent { path: path.to_owned() });
        }
        if !component.is_ascii() {
            return Err(ActiveReblitBootPublicationPlanError::NonAsciiPathComponent { path: path.to_owned() });
        }
        if is_retained_boot_file_private_component(component) {
            return Err(ActiveReblitBootPublicationPlanError::ReservedPrivatePublicationComponent {
                path: path.to_owned(),
                component: component.to_owned(),
            });
        }
        if component.len() > MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES {
            return Err(ActiveReblitBootPublicationPlanError::FatComponentByteLimit {
                path: path.to_owned(),
                limit: MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES,
                actual: component.len(),
            });
        }
        if let Some(character) = component
            .chars()
            .find(|character| matches!(character, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
        {
            return Err(ActiveReblitBootPublicationPlanError::FatForbiddenCharacter {
                path: path.to_owned(),
                character,
            });
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(ActiveReblitBootPublicationPlanError::FatTrailingDotOrSpace { path: path.to_owned() });
        }
        if component.contains('~') {
            return Err(ActiveReblitBootPublicationPlanError::FatShortNameMarker { path: path.to_owned() });
        }
        if is_dos_reserved_component(component) {
            return Err(ActiveReblitBootPublicationPlanError::FatReservedName {
                path: path.to_owned(),
                component: component.to_owned(),
            });
        }
    }

    Ok(PathBuf::from(text))
}

fn is_dos_reserved_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4 && (&bytes[..3] == b"COM" || &bytes[..3] == b"LPT") && matches!(bytes[3], b'1'..=b'9')
}
