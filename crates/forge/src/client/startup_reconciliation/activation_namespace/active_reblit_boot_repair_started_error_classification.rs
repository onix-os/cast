//! Fail-closed classification for boot-repair namespace admission.
//!
//! A stable semantic mismatch may defer a phase-specific journal-only route to
//! a later startup. Operational failures must remain visible: permission and
//! I/O errors, deadlines, bounded-work exhaustion, retry exhaustion, journal
//! storage errors, and installation revalidation errors are never converted
//! into `Deferred`.

use std::io;

use crate::{transition_journal::RuntimeEvidenceError, tree_marker::TreeMarkerError};

use super::{
    active_reblit_boot_repair_complete_proof::UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
    active_reblit_boot_repair_start_proof::UsrRollbackActiveReblitBootRepairStartNamespaceError,
    active_reblit_boot_repair_started_proof::UsrRollbackActiveReblitBootRepairStartedNamespaceError,
    candidate_preserve_proof::UsrRollbackCandidatePreserveNamespaceError, capture::CaptureError,
};

pub(in crate::client::startup_reconciliation) fn start_namespace_error_is_structural(
    error: &UsrRollbackActiveReblitBootRepairStartNamespaceError,
) -> bool {
    match error {
        UsrRollbackActiveReblitBootRepairStartNamespaceError::Capture(source) => {
            capture_error_is_structural(source)
        }
        UsrRollbackActiveReblitBootRepairStartNamespaceError::Topology(source) => {
            topology_error_is_structural(source)
        }
        UsrRollbackActiveReblitBootRepairStartNamespaceError::Journal(_)
        | UsrRollbackActiveReblitBootRepairStartNamespaceError::Installation(_) => false,
        UsrRollbackActiveReblitBootRepairStartNamespaceError::JournalChanged
        | UsrRollbackActiveReblitBootRepairStartNamespaceError::NamespaceChanged
        | UsrRollbackActiveReblitBootRepairStartNamespaceError::WrapperIndexChanged { .. } => false,
    }
}

pub(in crate::client::startup_reconciliation) fn complete_namespace_error_is_structural(
    error: &UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
) -> bool {
    match error {
        UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Capture(source) => {
            capture_error_is_structural(source)
        }
        UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Topology(source) => {
            topology_error_is_structural(source)
        }
        UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Journal(_)
        | UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Installation(_) => false,
        UsrRollbackActiveReblitBootRepairCompleteNamespaceError::JournalChanged
        | UsrRollbackActiveReblitBootRepairCompleteNamespaceError::NamespaceChanged
        | UsrRollbackActiveReblitBootRepairCompleteNamespaceError::WrapperIndexChanged { .. } => false,
    }
}

pub(in crate::client::startup_reconciliation) fn started_namespace_error_is_structural(
    error: &UsrRollbackActiveReblitBootRepairStartedNamespaceError,
) -> bool {
    match error {
        UsrRollbackActiveReblitBootRepairStartedNamespaceError::Capture(source) => capture_error_is_structural(source),
        UsrRollbackActiveReblitBootRepairStartedNamespaceError::Topology(source) => {
            topology_error_is_structural(source)
        }
        UsrRollbackActiveReblitBootRepairStartedNamespaceError::Journal(_)
        | UsrRollbackActiveReblitBootRepairStartedNamespaceError::Installation(_) => false,
        UsrRollbackActiveReblitBootRepairStartedNamespaceError::JournalChanged
        | UsrRollbackActiveReblitBootRepairStartedNamespaceError::NamespaceChanged
        | UsrRollbackActiveReblitBootRepairStartedNamespaceError::WrapperIndexChanged { .. } => false,
    }
}

pub(in crate::client::startup_reconciliation::activation_namespace) fn capture_error_is_structural(
    error: &CaptureError,
) -> bool {
    match error {
        CaptureError::Installation(_)
        | CaptureError::RuntimeEpoch(_)
        | CaptureError::Deadline
        | CaptureError::OperationLimit { .. }
        | CaptureError::EntryLimit { .. }
        | CaptureError::NameByteLimit { .. } => false,
        CaptureError::RuntimeEpochChanged => false,
        CaptureError::RuntimeTree { source, .. } => runtime_tree_error_is_structural(source),
        CaptureError::TreeMarker(source) => tree_marker_error_is_structural(source),
        CaptureError::Io { source, .. } => namespace_io_error_is_structural(source),
        CaptureError::NameContainsNul
        | CaptureError::DuplicateDirectoryName { .. }
        | CaptureError::UnsafeDirectory { .. }
        | CaptureError::FixedWrapperMissing { .. }
        | CaptureError::UnexpectedRootName { .. }
        | CaptureError::UnexpectedQuarantineName { .. }
        | CaptureError::UnexpectedWrapperEntry { .. }
        | CaptureError::UnexpectedIsolationEntry { .. }
        | CaptureError::RequiredTreeMissing { .. }
        | CaptureError::InvalidSlotName { .. }
        | CaptureError::UnsafeSlotLink { .. }
        | CaptureError::DuplicateSlotLink { .. }
        | CaptureError::SlotWrongWrapper { .. }
        | CaptureError::ParkingWrapperContainsTree { .. }
        | CaptureError::SlotTokenMismatch { .. }
        | CaptureError::StateWrapperMismatch { .. }
        | CaptureError::SlotAuthorizationCount { .. }
        | CaptureError::SlotWrongTransitionState { .. }
        | CaptureError::OrphanSlotLink { .. }
        | CaptureError::DuplicateTreeToken { .. }
        | CaptureError::RootAbiTemporary { .. }
        | CaptureError::RootAbiType { .. }
        | CaptureError::RootAbiTarget { .. }
        | CaptureError::RootAbiTargetTooLong { .. } => true,
        CaptureError::InodeChanged { .. } | CaptureError::DirectoryContentsChanged { .. } => false,
    }
}

fn topology_error_is_structural(error: &UsrRollbackCandidatePreserveNamespaceError) -> bool {
    match error {
        UsrRollbackCandidatePreserveNamespaceError::Capture(source) => capture_error_is_structural(source),
        UsrRollbackCandidatePreserveNamespaceError::NewStateEffect(_)
        | UsrRollbackCandidatePreserveNamespaceError::ActiveReblitEffect(_)
        | UsrRollbackCandidatePreserveNamespaceError::ArchivedEffect(_)
        | UsrRollbackCandidatePreserveNamespaceError::Journal(_)
        | UsrRollbackCandidatePreserveNamespaceError::Installation(_) => false,
        UsrRollbackCandidatePreserveNamespaceError::Policy(_)
        | UsrRollbackCandidatePreserveNamespaceError::WrongPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongCandidatePreservedPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitCompleteRoutePhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitBootRepairRequiredPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitBootRepairStartedPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitBootRepairCompletePhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitFinalizationPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongFreshDbInvalidationPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongFreshDbInvalidatedPhase
        | UsrRollbackCandidatePreserveNamespaceError::WrongRollbackCompletePhase
        | UsrRollbackCandidatePreserveNamespaceError::NewStateRequired
        | UsrRollbackCandidatePreserveNamespaceError::ActiveReblitRequired
        | UsrRollbackCandidatePreserveNamespaceError::ActivateArchivedRequired
        | UsrRollbackCandidatePreserveNamespaceError::CandidateMissing
        | UsrRollbackCandidatePreserveNamespaceError::CandidateStateMissing
        | UsrRollbackCandidatePreserveNamespaceError::PreviousStateMissing
        | UsrRollbackCandidatePreserveNamespaceError::StagingMissing
        | UsrRollbackCandidatePreserveNamespaceError::CandidateWrapperMissing
        | UsrRollbackCandidatePreserveNamespaceError::ActiveReblitWrapperMissing
        | UsrRollbackCandidatePreserveNamespaceError::DuplicateWrapper
        | UsrRollbackCandidatePreserveNamespaceError::UnexpectedParkingWrapper
        | UsrRollbackCandidatePreserveNamespaceError::UnexpectedCurrentStateWrapper { .. }
        | UsrRollbackCandidatePreserveNamespaceError::MarkerLinks { .. }
        | UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch => true,
        UsrRollbackCandidatePreserveNamespaceError::JournalChanged
        | UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged
        | UsrRollbackCandidatePreserveNamespaceError::TopologyChanged => false,
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
        | TreeMarkerError::UnauthorizedSlotLink { .. }
        | TreeMarkerError::SlotLinkPublicationCollision { .. }
        | TreeMarkerError::InvalidAuthorizedLinkCount { .. } => true,
        TreeMarkerError::MarkerChanged { .. }
        | TreeMarkerError::DirectoryChanged { .. }
        | TreeMarkerError::TemporaryChanged { .. } => false,
    }
}

fn runtime_tree_error_is_structural(error: &RuntimeEvidenceError) -> bool {
    match error {
        RuntimeEvidenceError::InspectTree(source) | RuntimeEvidenceError::ReadTreeMountId(source) => {
            namespace_io_error_is_structural(source)
        }
        RuntimeEvidenceError::TreeIsNotDirectory | RuntimeEvidenceError::ZeroTreeIdentity => true,
        RuntimeEvidenceError::TreeChanged => false,
        RuntimeEvidenceError::OpenProcfs(_)
        | RuntimeEvidenceError::OpenBootId(_)
        | RuntimeEvidenceError::AuthenticateBootId(_)
        | RuntimeEvidenceError::ReadBootId(_)
        | RuntimeEvidenceError::NoncanonicalBootIdFile
        | RuntimeEvidenceError::InvalidBootId(_)
        | RuntimeEvidenceError::OpenCurrentThreadProcfs(_)
        | RuntimeEvidenceError::OpenNamespaceDirectory(_)
        | RuntimeEvidenceError::AuthenticateNamespaceDirectory(_)
        | RuntimeEvidenceError::OpenMountNamespace(_)
        | RuntimeEvidenceError::AuthenticateMountNamespace(_)
        | RuntimeEvidenceError::ZeroMountNamespaceIdentity => false,
    }
}

fn namespace_io_error_is_structural(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(nix::libc::ENOENT | nix::libc::ENOTDIR | nix::libc::ELOOP | nix::libc::EXDEV)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_shape_mismatches_are_structural_but_changed_evidence_is_not() {
        assert!(started_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairStartedNamespaceError::Topology(
                UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch,
            ),
        ));
        assert!(!started_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairStartedNamespaceError::JournalChanged,
        ));
        assert!(!started_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairStartedNamespaceError::NamespaceChanged,
        ));
        assert!(!started_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairStartedNamespaceError::Capture(CaptureError::RuntimeEpochChanged),
        ));
    }

    #[test]
    fn missing_shapes_may_defer_but_operational_io_never_does() {
        let missing = CaptureError::Io {
            operation: "test missing namespace shape",
            path: std::path::PathBuf::from("/test"),
            source: io::Error::from_raw_os_error(nix::libc::ENOENT),
        };
        let denied = CaptureError::Io {
            operation: "test denied namespace capture",
            path: std::path::PathBuf::from("/test"),
            source: io::Error::from_raw_os_error(nix::libc::EACCES),
        };
        assert!(capture_error_is_structural(&missing));
        assert!(!capture_error_is_structural(&denied));
    }

    #[test]
    fn complete_route_uses_the_same_fail_closed_operational_boundary() {
        assert!(complete_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Topology(
                UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch,
            ),
        ));
        assert!(!complete_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairCompleteNamespaceError::JournalChanged,
        ));
        assert!(!complete_namespace_error_is_structural(
            &UsrRollbackActiveReblitBootRepairCompleteNamespaceError::Capture(CaptureError::Deadline),
        ));
    }
}
