//! Fail-closed classification for optional historical boot roots.
//!
//! Only stable namespace or authenticated metadata shape can exclude an
//! archive. Permission, storage, retry-exhaustion and other operational errors
//! remain hard failures so rollback history cannot silently disappear.

use std::io;

use crate::{transition_journal::RuntimeEvidenceError, tree_marker::TreeMarkerError};

use super::{
    super::{
        archived_state_identity::{ArchivedStateIdentityError, ArchivedStateIdentityStage},
        state_slot_marker,
    },
    ArchivedBootStateRootExclusionReason, IdentityError,
};

pub(super) fn archived_identity_exclusion(
    error: &ArchivedStateIdentityError,
) -> Option<ArchivedBootStateRootExclusionReason> {
    if !identity_error_is_structural(error.source_identity()) {
        return None;
    }
    Some(match error.stage() {
        ArchivedStateIdentityStage::StateName => return None,
        ArchivedStateIdentityStage::Wrapper => ArchivedBootStateRootExclusionReason::WrapperInexact,
        ArchivedStateIdentityStage::Usr => ArchivedBootStateRootExclusionReason::UsrInexact,
        ArchivedStateIdentityStage::TreeMarker => ArchivedBootStateRootExclusionReason::TreeMarkerInexact,
        ArchivedStateIdentityStage::StateId => ArchivedBootStateRootExclusionReason::StateIdInexact,
        ArchivedStateIdentityStage::SlotMarker => ArchivedBootStateRootExclusionReason::SlotMarkerInexact,
        ArchivedStateIdentityStage::WrapperLayout => ArchivedBootStateRootExclusionReason::WrapperLayoutInexact,
        ArchivedStateIdentityStage::FinalVerification => ArchivedBootStateRootExclusionReason::ChangedDuringPreparation,
    })
}

pub(super) fn identity_error_is_structural(error: &IdentityError) -> bool {
    match error {
        IdentityError::TreeMarker(source) => tree_marker_error_is_structural(source),
        IdentityError::StateSlotMarker(source) => state_slot_error_is_structural(source),
        IdentityError::Quarantine { source, .. } => namespace_io_error_is_structural(source),
        IdentityError::LiveUsr { operation, source, .. } => live_usr_error_is_structural(operation, source),
        IdentityError::UnsafeQuarantineDirectory { .. }
        | IdentityError::QuarantineDirectoryChanged { .. }
        | IdentityError::UnexpectedQuarantineEntries { .. } => true,
        _ => false,
    }
}

fn tree_marker_error_is_structural(error: &TreeMarkerError) -> bool {
    match error {
        TreeMarkerError::Io { source, .. } => {
            namespace_io_error_is_structural(source)
                || source.raw_os_error().is_none()
                    && matches!(source.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData)
        }
        TreeMarkerError::UnsafeDirectory { .. }
        | TreeMarkerError::UnsafeMarker { .. }
        | TreeMarkerError::Missing { .. }
        | TreeMarkerError::TemporaryPresent { .. }
        | TreeMarkerError::Decode { .. }
        | TreeMarkerError::TokenMismatch { .. }
        | TreeMarkerError::MarkerChanged { .. }
        | TreeMarkerError::DirectoryChanged { .. }
        | TreeMarkerError::TemporaryChanged { .. }
        | TreeMarkerError::UnauthorizedSlotLink { .. }
        | TreeMarkerError::SlotLinkPublicationCollision { .. }
        | TreeMarkerError::InvalidAuthorizedLinkCount { .. } => true,
    }
}

fn state_slot_error_is_structural(error: &state_slot_marker::Error) -> bool {
    match error {
        state_slot_marker::Error::Io { source, .. } => namespace_io_error_is_structural(source),
        state_slot_marker::Error::Directory { source, .. } => identity_error_is_structural(source),
        state_slot_marker::Error::TreeMarker { source, .. } => tree_marker_error_is_structural(source),
        state_slot_marker::Error::InvalidState { .. }
        | state_slot_marker::Error::InvalidName
        | state_slot_marker::Error::Changed { .. }
        | state_slot_marker::Error::PublicationCollision { .. }
        | state_slot_marker::Error::Missing { .. } => true,
    }
}

fn namespace_io_error_is_structural(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(nix::libc::ENOENT | nix::libc::ENOTDIR | nix::libc::ELOOP | nix::libc::EXDEV)
    )
}

fn live_usr_error_is_structural(operation: &'static str, source: &io::Error) -> bool {
    if namespace_io_error_is_structural(source) {
        return true;
    }
    if source.raw_os_error().is_some() {
        return false;
    }
    matches!(
        (operation, source.kind()),
        ("reject temporary state ID evidence", io::ErrorKind::Other)
            | ("read complete retained state ID", io::ErrorKind::UnexpectedEof)
            | ("validate retained state ID contents", io::ErrorKind::Other)
            | ("validate retained state ID inode", io::ErrorKind::PermissionDenied)
            | ("revalidate retained state ID", io::ErrorKind::Other)
    )
}

pub(super) fn runtime_error_is_structural(error: &RuntimeEvidenceError) -> bool {
    matches!(
        error,
        RuntimeEvidenceError::TreeIsNotDirectory
            | RuntimeEvidenceError::TreeChanged
            | RuntimeEvidenceError::ZeroTreeIdentity
    )
}
