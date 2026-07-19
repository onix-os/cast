use std::{fmt::Write as _, path::PathBuf};

use super::{
    ActiveReblitBlsComponentKind, ActiveReblitBlsComponentReason, ActiveReblitBlsRendererError, RenderBudget,
    allocation,
};

const MAX_FAT_COMPONENT_BYTES: usize = 255;

pub(super) fn payload_path(
    namespace: &str,
    digest: u128,
    length: u64,
    leaf: &str,
    leaf_kind: ActiveReblitBlsComponentKind,
    budget: &mut RenderBudget,
) -> Result<PathBuf, ActiveReblitBlsRendererError> {
    require_component(namespace, ActiveReblitBlsComponentKind::Namespace)?;
    require_component(leaf, leaf_kind)?;
    let token = checksum_identity_token(digest, length)?;
    build_relative_path(&["EFI", namespace, &token, leaf], budget)
}

fn checksum_identity_token(digest: u128, length: u64) -> Result<String, ActiveReblitBlsRendererError> {
    let mut token = String::new();
    token
        .try_reserve_exact("xxh3-".len() + 32 + "-l".len() + 16)
        .map_err(|source| allocation("BLS checksum identity component", source))?;
    write!(&mut token, "xxh3-{digest:032x}-l{length:016x}")
        .expect("writing fixed-width integers into a String cannot fail");
    Ok(token)
}

pub(super) fn entry_path(
    os_id: &str,
    version: &str,
    state: i32,
    budget: &mut RenderBudget,
) -> Result<PathBuf, ActiveReblitBlsRendererError> {
    require_component(os_id, ActiveReblitBlsComponentKind::OsId)?;
    require_component(version, ActiveReblitBlsComponentKind::KernelVersion)?;
    let state = state_decimal(state)?;
    let length = os_id
        .len()
        .checked_add(1)
        .and_then(|total| total.checked_add(version.len()))
        .and_then(|total| total.checked_add(1))
        .and_then(|total| total.checked_add(state.len()))
        .and_then(|total| total.checked_add(".conf".len()))
        .unwrap_or(usize::MAX);
    if length > MAX_FAT_COMPONENT_BYTES {
        return Err(ActiveReblitBlsRendererError::InvalidComponent {
            kind: ActiveReblitBlsComponentKind::EntryFilename,
            reason: ActiveReblitBlsComponentReason::TooLong,
        });
    }
    let mut filename = String::new();
    filename
        .try_reserve_exact(length)
        .map_err(|source| allocation("BLS entry filename", source))?;
    filename.push_str(os_id);
    filename.push('-');
    filename.push_str(version);
    filename.push('-');
    filename.push_str(&state);
    filename.push_str(".conf");
    require_component(&filename, ActiveReblitBlsComponentKind::EntryFilename)?;
    build_relative_path(&["loader", "entries", &filename], budget)
}

pub(super) fn fixed_path(
    path: &'static str,
    budget: &mut RenderBudget,
) -> Result<PathBuf, ActiveReblitBlsRendererError> {
    for component in path.split('/') {
        require_component(component, ActiveReblitBlsComponentKind::Namespace)?;
    }
    budget.admit_path(path.len())?;
    Ok(PathBuf::from(path))
}

pub(super) fn require_component(
    component: &str,
    kind: ActiveReblitBlsComponentKind,
) -> Result<(), ActiveReblitBlsRendererError> {
    let reason = if component.is_empty() {
        Some(ActiveReblitBlsComponentReason::Empty)
    } else if !component.is_ascii() {
        Some(ActiveReblitBlsComponentReason::NonAscii)
    } else if component.bytes().any(|byte| byte.is_ascii_control()) {
        Some(ActiveReblitBlsComponentReason::Control)
    } else if component == "." {
        Some(ActiveReblitBlsComponentReason::Dot)
    } else if component == ".." {
        Some(ActiveReblitBlsComponentReason::Parent)
    } else if component.len() > MAX_FAT_COMPONENT_BYTES {
        Some(ActiveReblitBlsComponentReason::TooLong)
    } else if component
        .bytes()
        .any(|byte| matches!(byte, b'/' | b'<' | b'>' | b':' | b'"' | b'\\' | b'|' | b'?' | b'*'))
    {
        Some(ActiveReblitBlsComponentReason::FatForbidden)
    } else if component.ends_with(['.', ' ']) {
        Some(ActiveReblitBlsComponentReason::FatTrailingDotOrSpace)
    } else if component.contains('~') {
        Some(ActiveReblitBlsComponentReason::FatShortNameMarker)
    } else if is_dos_reserved(component) {
        Some(ActiveReblitBlsComponentReason::FatReserved)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(ActiveReblitBlsRendererError::InvalidComponent { kind, reason }),
        None => Ok(()),
    }
}

fn build_relative_path(
    components: &[&str],
    budget: &mut RenderBudget,
) -> Result<PathBuf, ActiveReblitBlsRendererError> {
    let separators = components.len().saturating_sub(1);
    let length = components
        .iter()
        .try_fold(separators, |total, component| total.checked_add(component.len()))
        .unwrap_or(usize::MAX);
    budget.admit_path(length)?;
    let mut path = String::new();
    path.try_reserve_exact(length)
        .map_err(|source| allocation("BLS relative path", source))?;
    for (index, component) in components.iter().enumerate() {
        if index != 0 {
            path.push('/');
        }
        path.push_str(component);
    }
    Ok(PathBuf::from(path))
}

fn state_decimal(state: i32) -> Result<String, ActiveReblitBlsRendererError> {
    let mut rendered = String::new();
    rendered
        .try_reserve_exact(11)
        .map_err(|source| allocation("BLS state identifier", source))?;
    write!(&mut rendered, "{state}").expect("writing an i32 into a String cannot fail");
    Ok(rendered)
}

fn is_dos_reserved(component: &str) -> bool {
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
