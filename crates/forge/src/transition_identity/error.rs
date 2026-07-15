use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{db, installation, tree_marker::TreeMarkerError};

use super::state_slot_marker;
#[cfg(test)]
use super::{QuarantineFaultPoint, RetainedExchangeFaultPoint, RetainedPreviousMoveFaultPoint};

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("revalidate the retained installation root")]
    Installation(#[from] installation::Error),
    #[error("open or inspect the durable transition journal")]
    Journal(#[from] crate::transition_journal::StorageError),
    #[error("audit transition-bearing state rows")]
    StateEvidence(#[from] db::state::TransitionEvidenceError),
    #[error("prepare or authenticate a durable tree marker")]
    TreeMarker(#[from] TreeMarkerError),
    #[error("prepare or authenticate a reusable state-slot marker")]
    StateSlotMarker(#[from] state_slot_marker::Error),
    #[error("tree marker for state {state} has a second link but no exact authorized state-slot wrapper")]
    MissingAuthorizedStateSlotLink { state: i32 },
    #[error("tree marker for state {state} has more than one candidate authorized state-slot wrapper")]
    DuplicateAuthorizedStateSlotLinks { state: i32 },
    #[error("tree marker has a persistent state-slot link but the installation has no active state identity")]
    AuthorizedStateSlotLinkWithoutState,
    #[error("park or revalidate the retained active previous-state slot")]
    ActivePreviousSlotParking {
        #[source]
        source: Box<super::active_previous_slot_parking::ActivePreviousSlotParkingError>,
    },
    #[error("{operation} in retained /usr exchange namespace at `{}`", path.display())]
    RetainedExchange {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "retained /usr exchange parents are on different filesystems: live `{}` and staged `{}`",
        live_parent.display(),
        staged_parent.display()
    )]
    RetainedExchangeCrossDevice {
        live_parent: PathBuf,
        staged_parent: PathBuf,
    },
    #[error("retained /usr exchange namespace contains an unrecognized tree")]
    RetainedExchangeUnknownTree,
    #[error("retained /usr exchange namespace mismatch: live={live}, staged={staged}")]
    RetainedExchangeNamespaceMismatch { live: &'static str, staged: &'static str },
    #[error("{direction} retained /usr exchange expected {expected}, found {actual}")]
    RetainedExchangeUnexpectedLayout {
        direction: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{direction} retained /usr exchange reported success without changing either exact name")]
    RetainedExchangeReportedSuccessWithoutMove { direction: &'static str },
    #[cfg(test)]
    #[error("injected retained /usr exchange fault at {point:?}")]
    InjectedRetainedExchangeFault { point: RetainedExchangeFaultPoint },
    #[error("state ID {state} is not a canonical positive-decimal archive name")]
    InvalidPreviousArchiveState { state: i32 },
    #[error("{operation} in retained previous-tree namespace at `{}`", path.display())]
    PreviousMove {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("retained previous-state archive attempt lock is poisoned")]
    PreviousArchiveAttemptLockPoisoned,
    #[error("no retained previous-state archive attempt exists for state {state}")]
    PreviousArchiveAttemptMissing { state: i32 },
    #[error("retained previous-state archive attempt changed from `{expected}` to `{actual}`")]
    PreviousArchiveAttemptChanged { expected: String, actual: String },
    #[error("refusing to adopt pre-existing previous-state archive slot for state {state} at `{}`", path.display())]
    PreviousArchiveSlotExists { state: i32, path: PathBuf },
    #[error("construct a bounded private previous-state slot parking name")]
    InvalidPreviousArchiveParkingName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} private previous-state slot parking names are occupied for state {state}")]
    PreviousArchiveParkingExhausted { state: i32, limit: usize },
    #[error("multiple authenticated reusable archived-candidate slots exist for state {state}")]
    DuplicateReusableArchivedCandidateSlots { state: i32 },
    #[error("construct a bounded reusable archived-candidate slot name")]
    InvalidReusableArchivedCandidateParkingName(#[source] crate::transition_journal::CodecError),
    #[error(
        "previous-state archive failed before application and exact-slot retirement also failed: primary: {primary}; retirement: {cleanup}"
    )]
    PreviousArchiveAbortCleanupFailed { primary: Box<Error>, cleanup: Box<Error> },
    #[error(
        "retained previous-state slot namespace mismatch: canonical `{}` is {canonical_state}, parking `{}` is {parking_state}",
        canonical.display(),
        parking.display()
    )]
    PreviousArchiveSlotNamespaceMismatch {
        canonical: PathBuf,
        canonical_state: &'static str,
        parking: PathBuf,
        parking_state: &'static str,
    },
    #[error(
        "retained previous-state slot location mismatch between canonical `{}` and parking `{}`: expected {expected}, found {actual}",
        canonical.display(),
        parking.display()
    )]
    PreviousArchiveSlotLocationMismatch {
        canonical: PathBuf,
        parking: PathBuf,
        expected: &'static str,
        actual: &'static str,
    },
    #[error(
        "previous-state slot publication reported success but the exact slot remained parked (canonical `{}`, parking `{}`)",
        canonical.display(),
        parking.display()
    )]
    PreviousArchiveSlotPublishReportedSuccessWithoutMove { canonical: PathBuf, parking: PathBuf },
    #[error(
        "previous-state slot retirement reported success but the exact slot remained canonical (canonical `{}`, parking `{}`)",
        canonical.display(),
        parking.display()
    )]
    PreviousArchiveSlotRetireReportedSuccessWithoutMove { canonical: PathBuf, parking: PathBuf },
    #[error(
        "retained previous-tree parents are on different filesystems: staging `{}` and archive `{}`",
        staging.display(),
        archive.display()
    )]
    PreviousMoveCrossDevice { staging: PathBuf, archive: PathBuf },
    #[error("retained previous tree is present at both staging `{}` and archive `{}`", staged.display(), archived.display())]
    PreviousMoveBothNamesOccupied { staged: PathBuf, archived: PathBuf },
    #[error("retained previous tree is absent from both staging `{}` and archive `{}`", staged.display(), archived.display())]
    PreviousMoveTreeMissing { staged: PathBuf, archived: PathBuf },
    #[error("{direction} retained previous-tree move expected {expected}, found {actual}")]
    PreviousMoveUnexpectedLayout {
        direction: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{direction} retained previous-tree move reported success without changing either exact name")]
    PreviousMoveReportedSuccessWithoutMove { direction: &'static str },
    #[error(
        "{direction} retained previous-tree preflight failed ({primary}) and the exact namespace could not be reconciled ({reconciliation})"
    )]
    PreviousMovePreflightReconciliationFailed {
        direction: &'static str,
        primary: Box<Error>,
        reconciliation: Box<Error>,
    },
    #[error(
        "{direction} retained previous-tree preflight failed ({primary}) after the exact move was applied, and its durability suffix failed ({finish})"
    )]
    PreviousMoveAppliedAfterPreflightFailure {
        direction: &'static str,
        primary: Box<Error>,
        finish: Box<Error>,
    },
    #[cfg(test)]
    #[error("injected retained previous-tree move fault at {point:?}")]
    InjectedRetainedPreviousMoveFault { point: RetainedPreviousMoveFaultPoint },
    #[error("construct a bounded deterministic failed-candidate quarantine name")]
    InvalidQuarantineName(#[source] crate::transition_journal::CodecError),
    #[error("{operation} at failed-candidate quarantine path `{}`", path.display())]
    Quarantine {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe retained quarantine directory `{}` (uid={owner}, mode={mode:04o})", path.display())]
    UnsafeQuarantineDirectory { path: PathBuf, owner: u32, mode: u32 },
    #[error("retained quarantine directory changed at `{}`", path.display())]
    QuarantineDirectoryChanged { path: PathBuf },
    #[error("failed-candidate quarantine attempt lock is poisoned")]
    QuarantineAttemptLockPoisoned,
    #[error("failed-candidate quarantine attempt changed from `{expected}` to `{actual}`")]
    QuarantineAttemptChanged { expected: String, actual: String },
    #[error("deterministic failed-candidate quarantine slot already exists at `{}`", path.display())]
    QuarantineSlotExists { path: PathBuf },
    #[error(
        "failed candidate source `{}` and quarantine destination `{}` are on different filesystems",
        source_path.display(),
        destination.display()
    )]
    QuarantineCrossDevice { source_path: PathBuf, destination: PathBuf },
    #[error("failed-candidate quarantine destination already exists at `{}`", path.display())]
    QuarantineDestinationExists { path: PathBuf },
    #[error("unexpected entries in failed-candidate quarantine directory `{}`: {entries:?}", path.display())]
    UnexpectedQuarantineEntries { path: PathBuf, entries: Vec<String> },
    #[cfg(test)]
    #[error("injected failed-candidate quarantine fault at {point:?}")]
    InjectedQuarantineFault { point: QuarantineFaultPoint },
    #[error("{operation} at `{}`", path.display())]
    LiveUsr {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("live /usr appeared while synthesizing the proven-absent name at `{}`", path.display())]
    LiveUsrAppeared { path: PathBuf },
    #[error("newly created live /usr disappeared at `{}`", path.display())]
    LiveUsrDisappeared { path: PathBuf },
    #[error("live /usr name changed while retained at `{}`", path.display())]
    LiveUsrChanged { path: PathBuf },
    #[error("synthesized live /usr is unsafe at `{}` (uid={owner}, mode={mode:04o})", path.display())]
    UnsafeSynthesizedUsr { path: PathBuf, owner: u32, mode: u32 },
    #[error("live /usr cannot be adopted as an empty baseline at `{}`; found `{entry}`", path.display())]
    LiveUsrNotEmpty { path: PathBuf, entry: String },
    #[error("unresolved transition journal {transition} blocks tree-marker publication")]
    UnresolvedJournal { transition: String },
    #[error("transition journal {transition} appeared while its exclusive lock was retained")]
    JournalAppeared { transition: String },
    #[error("orphan transition row for state {state} and transition {transition} blocks tree-marker publication")]
    OrphanTransitionRow { state: i32, transition: String },
    #[error("load retained active-reblit state row {state}")]
    ActiveReblitStateLookup {
        state: i32,
        #[source]
        source: db::Error,
    },
    #[error("retained active-reblit state row {state} changed during the guarded operation")]
    ActiveReblitStateChanged { state: i32 },
    #[error("active state changed during reblit: expected {expected}, found {actual:?}")]
    ActiveReblitSelectionChanged { expected: i32, actual: Option<i32> },
    #[error(
        "candidate tree `{}` and previous tree `{}` carry duplicate permanent token {token}",
        candidate.display(),
        previous.display()
    )]
    DuplicateTreeToken {
        candidate: PathBuf,
        previous: PathBuf,
        token: String,
    },
}
