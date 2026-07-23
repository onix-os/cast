//! Semantic binding between a publication role, its source and destination.
//!
//! Payload destinations are checksum-addressed with the already-sealed XXH3
//! digest and exact byte length. This is deterministic immutable naming, not
//! cryptographic authentication.

use std::path::Path;

use super::{
    ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH, ACTIVE_REBLIT_LOADER_CONTROL_PATH, ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH,
    ActiveReblitBootPublicationPlanError, ActiveReblitBootPublicationRequest, ActiveReblitBootPublicationRequestSource,
    ActiveReblitBootPublicationRole,
};

const CHECKSUM_PREFIX: &str = "xxh3-";
const LENGTH_SEPARATOR: &str = "-l";
const DIGEST_HEX_WIDTH: usize = 32;
const LENGTH_HEX_WIDTH: usize = 16;

pub(super) fn require_role_binding(
    request: &ActiveReblitBootPublicationRequest,
    path: &Path,
) -> Result<(), ActiveReblitBootPublicationPlanError> {
    let expected_root = request.role.root();
    if request.root != expected_root {
        return Err(ActiveReblitBootPublicationPlanError::RoleRootMismatch {
            role: request.role,
            expected: expected_root,
            actual: request.root,
        });
    }
    let expected_phase = request.role.phase();
    if request.phase != expected_phase {
        return Err(ActiveReblitBootPublicationPlanError::RolePhaseMismatch {
            role: request.role,
            expected: expected_phase,
            actual: request.phase,
        });
    }
    if request.source.is_sealed() != request.role.requires_sealed_source() {
        return Err(ActiveReblitBootPublicationPlanError::RoleSourceMismatch { role: request.role });
    }
    if !role_path_matches(request, path) {
        return Err(ActiveReblitBootPublicationPlanError::RolePathMismatch {
            role: request.role,
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn role_path_matches(request: &ActiveReblitBootPublicationRequest, path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    match request.role {
        ActiveReblitBootPublicationRole::Payload => payload_path_matches(text, &request.source),
        ActiveReblitBootPublicationRole::Entry => {
            let mut components = text.split('/');
            matches!(
                (components.next(), components.next(), components.next(), components.next()),
                (Some("loader"), Some("entries"), Some(filename), None)
                    if filename.len() > ".conf".len() && filename.ends_with(".conf")
            )
        }
        ActiveReblitBootPublicationRole::LoaderControl => text == ACTIVE_REBLIT_LOADER_CONTROL_PATH,
        ActiveReblitBootPublicationRole::FallbackBootloader => text == ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH,
        ActiveReblitBootPublicationRole::SystemdBootloader => text == ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH,
    }
}

fn payload_path_matches(text: &str, source: &ActiveReblitBootPublicationRequestSource) -> bool {
    let ActiveReblitBootPublicationRequestSource::SealedSnapshot { digest, length, .. } = source else {
        return false;
    };
    let mut components = text.split('/');
    let (Some("EFI"), Some(namespace), Some(identity), Some(leaf), None) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) else {
        return false;
    };
    !namespace.is_empty()
        && (leaf == "vmlinuz" || leaf.ends_with(".initrd"))
        && checksum_identity_matches(identity, *digest, *length)
}

fn checksum_identity_matches(identity: &str, expected_digest: u128, expected_length: u64) -> bool {
    let Some(encoded) = identity.strip_prefix(CHECKSUM_PREFIX) else {
        return false;
    };
    let Some((digest, length)) = encoded.split_once(LENGTH_SEPARATOR) else {
        return false;
    };
    digest.len() == DIGEST_HEX_WIDTH
        && length.len() == LENGTH_HEX_WIDTH
        && digest.bytes().all(is_lower_hex)
        && length.bytes().all(is_lower_hex)
        && u128::from_str_radix(digest, 16) == Ok(expected_digest)
        && u64::from_str_radix(length, 16) == Ok(expected_length)
}

const fn is_lower_hex(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}
