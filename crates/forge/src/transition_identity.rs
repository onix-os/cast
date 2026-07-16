//! Durable `/usr` identity guard for the existing stateful coordinator.
//!
//! This is deliberately narrower than the eventual crash-reopen transition
//! state machine. It obtains the already-defined journal lock, proves that no
//! journal or transition-bearing database row exists, and only then permits
//! the tree-marker primitive to create or adopt permanent identities. Once
//! prepared, every named-tree check is recovery-style and read-only: it can
//! neither mint nor repair a marker.

use std::{
    ffi::CStr,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    sync::Mutex,
};

use thiserror::Error;

use crate::{
    Installation, db,
    linux_fs::{
        chmod_path_descriptor, controlled_resolution, openat2_file, renameat2_exchange_once, renameat2_noreplace,
        renameat2_noreplace_once, require_no_access_acl, require_no_default_acl,
    },
    state,
    transition_journal::{QuarantineName, TransitionJournalStore},
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

mod active_previous_slot_parking;
mod archived_candidate;
mod archived_state_prune;
mod archived_state_repair;
mod candidate_metadata;
mod candidate_quarantine;
mod error;
mod fault_injection;
#[allow(dead_code)] // contract-only until startup reconciliation can consume its records
mod journal_coordinator;
mod namespace_helpers;
mod prejournal_inventory;
mod previous_tree_move;
mod prune_residue;
mod reusable_previous_slot;
mod slot_link_recovery;
mod staging_wrapper_rotation;
mod state_slot_marker;
mod state_tree_metadata;
mod tree_lifecycle;

pub(crate) use candidate_metadata::{
    CandidateMetadataError, CandidateMetadataProof, CandidateMetadataPublication, RetainedCandidateUsr,
};
#[cfg(test)]
pub(crate) use candidate_metadata::{
    arm_after_first_publication as arm_after_candidate_metadata_first_publication,
    arm_applied_private_directory_publication_error as arm_applied_candidate_metadata_directory_publication_error,
    arm_before_publication as arm_before_candidate_metadata_publication, arm_candidate_usr_clone_fault,
    assert_candidate_usr_clone_fault_consumed,
};
pub(crate) use error::Error;
use fault_injection::{
    before_live_usr_mkdir, before_previous_archive_slot_reopen, before_previous_slot_retirement_rename,
    before_quarantine_slot_reopen, before_retained_exchange_rename, before_retained_previous_move_rename,
    quarantine_checkpoint, retained_exchange_checkpoint, retained_previous_move_checkpoint,
};
#[allow(unused_imports)] // contract-only surface for the later live coordinator integration
pub(crate) use journal_coordinator::{
    NewStatePrevious, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError, StatefulTransitionRequest,
};
use namespace_helpers::*;

#[allow(unused_imports)]
pub(crate) use prejournal_inventory::{
    CandidateInventoryBoundary, CandidateInventoryError, CandidateInventoryLimits, RetainedCandidateDurabilitySeal,
};

#[cfg(test)]
pub(crate) use active_previous_slot_parking::{
    RetainedActivePreviousSlotParkingFaultPoint, arm_active_previous_slot_parking_faults,
    arm_before_active_previous_slot_parking_rename,
};
pub(crate) use archived_candidate::{
    ArchivedCandidateError, RetainedArchivedCandidateMoveFailure, RetainedArchivedCandidateMoveOutcome,
};
#[cfg(test)]
pub(crate) use archived_candidate::{
    RetainedArchivedCandidateMoveFaultPoint, arm_before_archived_candidate_slot_marker_location,
    arm_before_retained_archived_candidate_exchange, arm_before_retired_archived_candidate_slot_move,
    arm_retained_archived_candidate_move_fault,
};
pub(crate) use archived_state_prune::{
    ArchivedStatePruneError, MAX_ARCHIVED_STATE_PRUNE_BATCH, RetainedArchivedStatePrune,
};
#[cfg(test)]
pub(crate) use archived_state_prune::{
    ArchivedStatePruneFaultPoint, ArchivedStatePruneLimits, archived_state_prune_quarantine_name,
    arm_archived_state_prune_fault, arm_before_archived_state_prune_child_unlink,
    arm_before_archived_state_prune_wrapper_move,
};
#[allow(unused_imports)]
pub(crate) use archived_state_repair::{
    ArchivedStateRepairError, ArchivedStateRepairFailure, ArchivedStateRepairIdentity, ArchivedStateRepairOutcome,
    ArchivedStateRepairPublication,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use archived_state_repair::{
    ArchivedStateRepairFaultPoint, ArchivedStateRepairNamespaceMove, archived_state_repair_namespace_syscall_count,
    arm_archived_state_repair_faults, arm_before_archived_state_repair_cleanup,
    arm_before_archived_state_repair_namespace_syscall, arm_before_archived_state_repair_preservation,
    arm_before_archived_state_repair_publication, arm_before_archived_state_repair_suffix_retry,
    arm_between_archived_state_repair_layout_reads,
};
#[cfg(test)]
pub(crate) use fault_injection::{
    arm_before_live_usr_mkdir, arm_before_previous_archive_slot_reopen, arm_before_previous_slot_retirement_rename,
    arm_before_quarantine_slot_reopen, arm_before_retained_exchange_rename, arm_before_retained_previous_move_rename,
    arm_quarantine_fault, arm_quarantine_faults, arm_retained_exchange_fault, arm_retained_previous_move_fault,
    arm_retained_previous_move_faults,
};
#[cfg(test)]
pub(crate) use prune_residue::arm_after_archived_state_prune_residue_first_scan;
pub(crate) use prune_residue::{
    ArchivedStatePruneResidueError, audit_archived_state_prune_residue, audit_archived_state_prune_residue_read_only,
};
pub(crate) use staging_wrapper_rotation::{
    RetainedStagingWrapperRotationFailure, RetainedStagingWrapperRotationOutcome,
};
#[cfg(test)]
pub(crate) use staging_wrapper_rotation::{
    RetainedStagingWrapperRotationFaultPoint, arm_before_staging_wrapper_exchange, arm_staging_wrapper_rotation_faults,
};
#[cfg(test)]
pub(crate) use tree_lifecycle::arm_after_candidate_mutable_namespace_preflight;

const LIVE_USR_NAME: &CStr = c"usr";
const TREE_MARKER_NAME: &[u8] = b".cast-tree-id";
const SYNTHESIZED_USR_MODE: u32 = 0o755;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const MAX_INTERRUPTED_DIRECTORY_CREATION_ATTEMPTS: usize = 8;
const MAX_PREVIOUS_SLOT_PARKING_CANDIDATES: usize = 256;
const ROOTS_RELATIVE: &CStr = c".cast/root";
const STAGING_RELATIVE: &CStr = c".cast/root/staging";
const QUARANTINE_RELATIVE: &CStr = c".cast/quarantine";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailedCandidateKind {
    NewState,
    ActiveReblit,
    ArchivedState,
}

impl FailedCandidateKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::NewState => "new-state",
            Self::ActiveReblit => "active-reblit",
            Self::ArchivedState => "archived-state",
        }
    }
}

/// Retains every namespace capability until database invalidation has either
/// completed or been refused. The path is diagnostic only; authority remains
/// in the open descriptors and the candidate identity guard.
#[derive(Debug)]
pub(crate) struct QuarantinedCandidate {
    name: std::ffi::CString,
    destination_path: PathBuf,
    staging: RetainedDirectory,
    quarantine: RetainedDirectory,
    slot: RetainedDirectory,
}

/// Attempt-local authority for one deterministic quarantine slot.
///
/// An empty directory at the token-derived name is not evidence that this
/// process created it: a same-UID writer can pre-create the same shape.  The
/// retained descriptor makes in-process fault retry possible without ever
/// adopting an unproven pathname.  Crash-reopen adoption belongs to the
/// durable transition journal rather than this narrower guard.
#[derive(Debug)]
struct RetainedQuarantineAttempt {
    name: std::ffi::CString,
    slot: RetainedDirectory,
}

/// Attempt-local authority for the state slot used to archive the exact
/// previous tree. A fresh slot is prepared and retained at a private parking
/// name. A wrapper retained from an earlier activation is reusable only when
/// its immutable state/tree marker and marker-only layout authenticate it.
/// Unmarked ambient directories are never adopted, even when empty and safe.
#[derive(Debug)]
struct RetainedPreviousArchiveAttempt {
    name: std::ffi::CString,
    parking_name: std::ffi::CString,
    roots: RetainedDirectory,
    staging: RetainedDirectory,
    slot: RetainedDirectory,
    state_slot_marker: Option<state_slot_marker::RetainedStateSlotMarker>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetainedDirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
}

#[derive(Debug)]
struct RetainedDirectory {
    file: std::fs::File,
    path: PathBuf,
    witness: RetainedDirectoryWitness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuarantineFaultPoint {
    CandidatePreSync,
    SlotSync,
    QuarantineBaseSync,
    Rename,
    MovedCandidateSync,
    SourceParentSync,
    DestinationParentSync,
    FinalRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedExchangeFaultPoint {
    BeforeRename,
    AfterRename,
    StagingParentSync,
    InstallationRootSync,
    FinalRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedExchangeOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedPreviousMoveFaultPoint {
    PreviousPreSync,
    BeforeSlotPublish,
    AfterSlotPublish,
    SlotSync,
    RootsParentSync,
    BeforeRename,
    AfterRename,
    SourceParentSync,
    DestinationParentSync,
    FinalRevalidation,
    BeforeSlotRetire,
    AfterSlotRetire,
    RootsAfterSlotRetireSync,
    FinalSlotRetirementRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedPreviousMoveOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("retained previous-tree move outcome is {outcome:?}")]
pub(crate) struct RetainedPreviousMoveFailure {
    outcome: RetainedPreviousMoveOutcome,
    #[source]
    source: Error,
}

impl RetainedPreviousMoveFailure {
    pub(crate) fn outcome(&self) -> RetainedPreviousMoveOutcome {
        self.outcome
    }

    fn with_abort_cleanup(self, cleanup: Error) -> Self {
        Self {
            outcome: self.outcome,
            source: Error::PreviousArchiveAbortCleanupFailed {
                primary: Box::new(self.source),
                cleanup: Box::new(cleanup),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedPreviousMoveDirection {
    Archive,
    Restore,
}

impl RetainedPreviousMoveDirection {
    fn before(self) -> RetainedPreviousMoveLayout {
        match self {
            Self::Archive => RetainedPreviousMoveLayout::Staged,
            Self::Restore => RetainedPreviousMoveLayout::Archived,
        }
    }

    fn after(self) -> RetainedPreviousMoveLayout {
        match self {
            Self::Archive => RetainedPreviousMoveLayout::Archived,
            Self::Restore => RetainedPreviousMoveLayout::Staged,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedPreviousMoveLayout {
    Staged,
    Archived,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedPreviousSlotLocation {
    Canonical,
    Parked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedPreviousSlotNameState {
    Absent,
    Exact,
    Foreign,
}

impl RetainedPreviousSlotNameState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Exact => "exact",
            Self::Foreign => "foreign",
        }
    }
}

impl RetainedPreviousMoveLayout {
    fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Error)]
#[error("retained /usr exchange outcome is {outcome:?}")]
pub(crate) struct RetainedExchangeFailure {
    outcome: RetainedExchangeOutcome,
    #[source]
    source: Error,
}

impl RetainedExchangeFailure {
    pub(crate) fn outcome(&self) -> RetainedExchangeOutcome {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedExchangeDirection {
    Forward,
    Reverse,
}

impl RetainedExchangeDirection {
    fn before(self) -> RetainedExchangeLayout {
        match self {
            Self::Forward => RetainedExchangeLayout::CandidateStaged,
            Self::Reverse => RetainedExchangeLayout::CandidateLive,
        }
    }

    fn after(self) -> RetainedExchangeLayout {
        match self {
            Self::Forward => RetainedExchangeLayout::CandidateLive,
            Self::Reverse => RetainedExchangeLayout::CandidateStaged,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Reverse => "reverse",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedExchangeLayout {
    CandidateStaged,
    CandidateLive,
}

impl RetainedExchangeLayout {
    fn as_str(self) -> &'static str {
        match self {
            Self::CandidateStaged => "candidate-staged",
            Self::CandidateLive => "candidate-live",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedTreeRole {
    Candidate,
    Previous,
}

impl RetainedTreeRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Previous => "previous",
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_LIVE_USR_MKDIR: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_QUARANTINE_SLOT_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static QUARANTINE_FAULT: std::cell::RefCell<Option<(QuarantineFaultPoint, usize)>> =
        const { std::cell::RefCell::new(None) };
    static RETAINED_EXCHANGE_FAULT: std::cell::RefCell<Option<RetainedExchangeFaultPoint>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RETAINED_EXCHANGE_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static RETAINED_PREVIOUS_MOVE_FAULT: std::cell::RefCell<Vec<RetainedPreviousMoveFaultPoint>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_PREVIOUS_ARCHIVE_SLOT_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RETAINED_PREVIOUS_MOVE_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_PREVIOUS_SLOT_RETIREMENT_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Retained identities for the candidate and previous `/usr` trees.
///
/// Keeping the journal store in this value keeps its exclusive lock alive for
/// the entire in-process activation and compensating recovery. The unwired
/// durable-prefix coordinator consumes this guard when it creates a journal.
#[derive(Debug)]
pub(crate) struct StatefulTreeIdentity {
    journal: TransitionJournalStore,
    state_database: db::state::Database,
    candidate: RetainedIdentity,
    previous: RetainedIdentity,
    previous_classification: RetainedPreviousClassification,
    quarantine_attempt: Mutex<Option<RetainedQuarantineAttempt>>,
    previous_archive_attempt: Mutex<Option<RetainedPreviousArchiveAttempt>>,
    archived_candidate_attempt: Mutex<Option<archived_candidate::RetainedArchivedCandidateAttempt>>,
    active_reblit_rotation: Mutex<Option<staging_wrapper_rotation::RetainedStagingWrapperRotation>>,
    active_previous_slot_parking: Mutex<Option<active_previous_slot_parking::RetainedActivePreviousSlotParking>>,
}

/// Previous-tree facts retained while its exact `/usr` descriptor and the
/// installation-wide cooperating-writer authority are held. Unmanaged trees
/// are not representable because current preparation rejects them rather than
/// authenticating them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedPreviousClassification {
    Active(state::Id),
    SynthesizedEmpty,
}

#[derive(Debug)]
struct RetainedIdentity {
    store: TreeMarkerStore,
    marker: RetainedTreeMarker,
    state_id: Option<state_tree_metadata::RetainedTreeStateId>,
}

impl StatefulTreeIdentity {
    /// Verify the layout after a compensating reverse exchange.
    pub(crate) fn verify_restored(&self, live_previous_path: &Path, staged_candidate_path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.previous.verify_named_read_only(live_previous_path)?;
        self.candidate.verify_named_read_only(staged_candidate_path)?;
        Ok(())
    }

    fn require_no_journal(&self) -> Result<(), Error> {
        if let Some(record) = self.journal.load()? {
            return Err(Error::JournalAppeared {
                transition: record.transition_id.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

impl RetainedDirectory {
    fn open_beneath(root: &std::fs::File, relative: &CStr, path: PathBuf) -> Result<Self, Error> {
        Self::open_at(root, relative, path)
    }

    fn open_child(&self, name: &CStr, path: PathBuf) -> Result<Self, Error> {
        Self::open_at(&self.file, name, path)
    }

    fn clone_retained(&self) -> Result<Self, Error> {
        let clone = Self::open_at(&self.file, c".", self.path.clone())?;
        self.require_same(&clone)?;
        Ok(clone)
    }

    fn open_optional_child(&self, name: &CStr, path: PathBuf) -> Result<Option<Self>, Error> {
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        ) {
            Ok(probe) => {
                drop(probe);
                Self::open_at(&self.file, name, path).map(Some)
            }
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
            Err(source) => Err(quarantine_io("probe retained child directory", &path, source)),
        }
    }

    /// Probe a final component without following it or assuming its type.
    ///
    /// Private parking-name selection must skip every occupant, including a
    /// regular file, symlink, FIFO, mount point, or directory whose mode/ACL is
    /// unsafe to adopt. A directory-only probe would let the first hostile
    /// residue abort the bounded scan instead of continuing to the next
    /// candidate.
    fn child_name_exists(&self, name: &CStr, path: PathBuf) -> Result<bool, Error> {
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        ) {
            Ok(_) => Ok(true),
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(false),
            // RESOLVE_NO_XDEV reports EXDEV when the final component is a
            // mount point. For parking-name selection that is positive
            // occupancy evidence, not a failure: skip the name without
            // entering or adopting the mounted tree.
            Err(source) if source.raw_os_error() == Some(nix::libc::EXDEV) => Ok(true),
            Err(source) => Err(quarantine_io("probe retained child name", &path, source)),
        }
    }

    fn open_at(parent: &std::fs::File, name: &CStr, path: PathBuf) -> Result<Self, Error> {
        let pinned = openat2_file(
            parent.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| quarantine_io("pin retained directory", &path, source))?;
        let expected = retained_directory_witness(&pinned, &path)?;
        let file = openat2_file(
            parent.as_raw_fd(),
            name,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOATIME,
            0,
            controlled_resolution(),
        )
        .map_err(|source| quarantine_io("open retained directory", &path, source))?;
        if retained_directory_witness(&file, &path)? != expected {
            return Err(Error::QuarantineDirectoryChanged { path });
        }
        require_no_access_acl(&file, &path)
            .map_err(|source| quarantine_io("reject access ACL on retained directory", &path, source))?;
        require_no_default_acl(&file, &path)
            .map_err(|source| quarantine_io("reject default ACL on retained directory", &path, source))?;
        Ok(Self {
            file,
            path,
            witness: expected,
        })
    }

    fn create_private_child(parent: &Self, name: &CStr, path: PathBuf) -> Result<Self, Error> {
        Self::create_private_child_with(parent, name, path, before_quarantine_slot_reopen)
    }

    fn create_private_previous_slot(parent: &Self, name: &CStr, path: PathBuf) -> Result<Self, Error> {
        Self::create_private_child_with(parent, name, path, before_previous_archive_slot_reopen)
    }

    fn create_private_child_with(
        parent: &Self,
        name: &CStr,
        path: PathBuf,
        before_reopen: fn(),
    ) -> Result<Self, Error> {
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            // SAFETY: parent and the single validated child-name C string
            // remain live. mkdirat never follows or replaces the final name.
            if unsafe { nix::libc::mkdirat(parent.file.as_raw_fd(), name.as_ptr(), PRIVATE_DIRECTORY_MODE) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            match source.kind() {
                io::ErrorKind::Interrupted if attempts < MAX_INTERRUPTED_DIRECTORY_CREATION_ATTEMPTS => {}
                io::ErrorKind::AlreadyExists => return Err(Error::QuarantineSlotExists { path }),
                _ => return Err(quarantine_io("create private quarantine slot", &path, source)),
            }
        }

        let pinned = openat2_file(
            parent.file.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| quarantine_io("pin fresh quarantine slot", &path, source))?;
        let metadata = pinned
            .metadata()
            .map_err(|source| quarantine_io("inspect fresh quarantine slot", &path, source))?;
        let mode = metadata.permissions().mode() & 0o7777;
        // A just-created slot may expose only an owner-owned subset of 0700
        // under the process umask. Anything else is substitution evidence.
        if !metadata.file_type().is_dir()
            || metadata.uid() != unsafe { nix::libc::geteuid() }
            || mode & !PRIVATE_DIRECTORY_MODE != 0
        {
            return Err(Error::UnsafeQuarantineDirectory {
                path,
                owner: metadata.uid(),
                mode,
            });
        }
        chmod_path_descriptor(&pinned, PRIVATE_DIRECTORY_MODE)
            .map_err(|source| quarantine_io("normalize fresh quarantine slot mode", &path, source))?;
        let expected = retained_directory_witness(&pinned, &path)?;
        before_reopen();
        let slot = Self::open_at(&parent.file, name, path)?;
        if slot.witness != expected {
            return Err(Error::QuarantineDirectoryChanged {
                path: slot.path.clone(),
            });
        }
        if slot.witness.mode != PRIVATE_DIRECTORY_MODE {
            return Err(Error::UnsafeQuarantineDirectory {
                path: slot.path.clone(),
                owner: slot.witness.owner,
                mode: slot.witness.mode,
            });
        }
        slot.require_exact_entries(&[])?;
        Ok(slot)
    }

    fn sync(&self, operation: &'static str) -> Result<(), Error> {
        if retained_directory_witness(&self.file, &self.path)? != self.witness {
            return Err(Error::QuarantineDirectoryChanged {
                path: self.path.clone(),
            });
        }
        self.file
            .sync_all()
            .map_err(|source| quarantine_io(operation, &self.path, source))?;
        if retained_directory_witness(&self.file, &self.path)? != self.witness {
            return Err(Error::QuarantineDirectoryChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    fn require_retained(&self) -> Result<(), Error> {
        if retained_directory_witness(&self.file, &self.path)? != self.witness {
            return Err(Error::QuarantineDirectoryChanged {
                path: self.path.clone(),
            });
        }
        require_no_access_acl(&self.file, &self.path)
            .map_err(|source| quarantine_io("reject access ACL on retained directory", &self.path, source))?;
        require_no_default_acl(&self.file, &self.path)
            .map_err(|source| quarantine_io("reject default ACL on retained directory", &self.path, source))
    }

    fn revalidate_beneath(&self, root: &std::fs::File, relative: &CStr) -> Result<(), Error> {
        let named = Self::open_at(root, relative, self.path.clone())?;
        self.require_same(&named)
    }

    fn revalidate_child(&self, parent: &Self, name: &CStr) -> Result<(), Error> {
        let named = Self::open_at(&parent.file, name, self.path.clone())?;
        self.require_same(&named)
    }

    fn require_same(&self, named: &Self) -> Result<(), Error> {
        if retained_directory_witness(&self.file, &self.path)? == self.witness && named.witness == self.witness {
            Ok(())
        } else {
            Err(Error::QuarantineDirectoryChanged {
                path: self.path.clone(),
            })
        }
    }

    fn require_child_absent(&self, name: &CStr) -> Result<(), Error> {
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        ) {
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
            Ok(_) => Err(Error::QuarantineDestinationExists {
                path: self.path.join(name.to_string_lossy().as_ref()),
            }),
            Err(source) => Err(quarantine_io(
                "prove quarantine child absence",
                &self.path.join(name.to_string_lossy().as_ref()),
                source,
            )),
        }
    }

    fn require_exact_entries(&self, expected: &[&[u8]]) -> Result<(), Error> {
        let mut actual = self.entries(expected.len().saturating_add(1))?;
        let mut expected = expected.iter().map(|name| name.to_vec()).collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        if actual == expected {
            Ok(())
        } else {
            Err(Error::UnexpectedQuarantineEntries {
                path: self.path.clone(),
                entries: actual
                    .into_iter()
                    .map(|name| String::from_utf8_lossy(&name).into_owned())
                    .collect(),
            })
        }
    }

    fn entries(&self, limit: usize) -> Result<Vec<Vec<u8>>, Error> {
        retained_directory_entries(&self.file, &self.path, limit)
    }
}

fn quarantine_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Quarantine {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn retained_exchange_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::RetainedExchange {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn previous_move_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::PreviousMove {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn live_usr_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::LiveUsr {
        operation,
        path: path.to_owned(),
        source,
    }
}

impl RetainedIdentity {
    fn prepare(store: TreeMarkerStore, state: Option<state::Id>) -> Result<Self, Error> {
        let marker = store.adopt_or_create_before_journal_for_transition()?;
        Self::with_marker(store, marker, state)
    }

    /// Marker-only preparation for a fresh state whose database identity does
    /// not exist yet. The strict one-link marker path refuses archived-slot
    /// recovery evidence because no state ID can authorize such a link.
    fn prepare_unallocated(store: TreeMarkerStore) -> Result<Self, Error> {
        let marker = store.adopt_or_create_before_journal()?;
        Self::with_marker(store, marker, None)
    }

    /// Strict preparation for a newly materialized tree which cannot already
    /// own a state-slot hardlink. Archived repair must reject `nlink=2`
    /// transition markers rather than deferring their authorization.
    fn prepare_strict(store: TreeMarkerStore, state: state::Id) -> Result<Self, Error> {
        let marker = store.adopt_or_create_before_journal()?;
        Self::with_marker(store, marker, Some(state))
    }

    fn with_marker(
        store: TreeMarkerStore,
        marker: RetainedTreeMarker,
        state: Option<state::Id>,
    ) -> Result<Self, Error> {
        let state_id = state
            .map(|state| state_tree_metadata::RetainedTreeStateId::retain(&store, state))
            .transpose()?;
        Ok(Self {
            store,
            marker,
            state_id,
        })
    }

    /// This method is intentionally incapable of reaching marker creation.
    fn verify_named_read_only(&self, path: &Path) -> Result<(), Error> {
        self.revalidate_retained()?;
        let named_store = TreeMarkerStore::open_path(path)?;
        self.verify_store_read_only(&named_store)
    }

    fn verify_store_read_only(&self, named_store: &TreeMarkerStore) -> Result<(), Error> {
        self.revalidate_retained()?;
        self.store.require_same_directory(named_store)?;
        let named = self.marker.read_named_for_transition(named_store)?;
        self.marker.require_same_marker(&named)?;
        self.revalidate_retained()?;
        self.store.require_same_directory(named_store)?;
        self.marker.require_same_marker(&named).map_err(Error::from)
    }

    /// Strict candidate-only proof. Recovery movement deliberately continues
    /// to use marker-only verification so a trigger-corrupted `.stateID` can
    /// still be reversed and quarantined rather than stranding the bad tree
    /// live.
    fn verify_named_with_state_id(&self, path: &Path) -> Result<(), Error> {
        self.verify_named_read_only(path)?;
        let named_store = TreeMarkerStore::open_path(path)?;
        self.verify_store_with_state_id(&named_store)
    }

    /// Strict descriptor-bound candidate proof. Unlike the path convenience
    /// wrapper above, callers can bind this check to a wrapper directory they
    /// already retained before any trigger or namespace mutation.
    fn verify_store_with_state_id(&self, named_store: &TreeMarkerStore) -> Result<(), Error> {
        self.verify_store_read_only(named_store)?;
        self.store.require_same_directory(named_store)?;
        let state_id = self.state_id.as_ref().ok_or_else(|| Error::LiveUsr {
            operation: "load retained candidate state ID",
            path: named_store.display_path().to_owned(),
            source: io::Error::other("candidate state ID identity was not retained"),
        })?;
        state_id.revalidate(&self.store, &named_store)?;
        self.verify_store_read_only(&named_store)
    }

    fn matches_store_read_only(&self, named_store: &TreeMarkerStore) -> Result<bool, Error> {
        match self.store.require_same_directory(named_store) {
            Ok(()) => {
                self.verify_store_read_only(named_store)?;
                Ok(true)
            }
            Err(TreeMarkerError::DirectoryChanged { .. }) => Ok(false),
            Err(source) => Err(source.into()),
        }
    }

    fn revalidate_retained(&self) -> Result<(), Error> {
        self.marker.revalidate(&self.store).map_err(Error::from)
    }
}

fn require_clean_baseline(journal: &TransitionJournalStore, state_db: &db::state::Database) -> Result<(), Error> {
    if let Some(record) = journal.load()? {
        return Err(Error::UnresolvedJournal {
            transition: record.transition_id.as_str().to_owned(),
        });
    }
    if let Some(orphan) = state_db.audit_in_flight_transition()? {
        return Err(Error::OrphanTransitionRow {
            state: i32::from(orphan.state_id),
            transition: orphan.transition_id.as_str().to_owned(),
        });
    }
    Ok(())
}
