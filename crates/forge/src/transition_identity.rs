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
    Installation, db, installation,
    linux_fs::{
        chmod_path_descriptor, controlled_resolution, openat2_file, renameat2_exchange_once, renameat2_noreplace,
        renameat2_noreplace_once, require_no_access_acl, require_no_default_acl,
    },
    state,
    transition_journal::{QuarantineName, TransitionJournalStore},
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

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

/// Attempt-local authority for the state slot created to archive the exact
/// previous tree. The slot is prepared and retained at a private parking name
/// before no-replace publication to the decimal state name. An ambient
/// directory at either name is never adopted, even when it is empty and safe.
#[derive(Debug)]
struct RetainedPreviousArchiveAttempt {
    name: std::ffi::CString,
    parking_name: std::ffi::CString,
    roots: RetainedDirectory,
    staging: RetainedDirectory,
    slot: RetainedDirectory,
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
/// the entire in-process activation and compensating recovery. The journal is
/// not created by this slice.
#[derive(Debug)]
pub(crate) struct StatefulTreeIdentity {
    journal: TransitionJournalStore,
    candidate: RetainedIdentity,
    previous: RetainedIdentity,
    quarantine_attempt: Mutex<Option<RetainedQuarantineAttempt>>,
    previous_archive_attempt: Mutex<Option<RetainedPreviousArchiveAttempt>>,
}

#[derive(Debug)]
struct RetainedIdentity {
    store: TreeMarkerStore,
    marker: RetainedTreeMarker,
}

impl StatefulTreeIdentity {
    /// Establish both permanent identities before the coordinator performs a
    /// trigger, exchange, archive, quarantine, or other transition effect.
    pub(crate) fn prepare(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
    ) -> Result<Self, Error> {
        let root = &installation.root;
        let previous_path = root.join("usr");
        // Lock ordering is installation lock (owned by Installation), state
        // database (already opened), then journal lock. Do not invent a second
        // lock for marker publication.
        installation.revalidate_root_directory()?;
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), root)?;
        require_clean_baseline(&journal, state_db)?;

        // Authenticate the materialized candidate and establish a strictly
        // empty, same-mount previous tree only when the retained root proves
        // that `usr` is genuinely absent.
        let candidate_store = TreeMarkerStore::open_path(candidate_path)?;
        let previous_store = open_or_synthesize_live_usr(installation)?;
        let candidate = RetainedIdentity::prepare(candidate_store)?;
        let previous = RetainedIdentity::prepare(previous_store)?;
        if candidate.marker.token() == previous.marker.token() {
            return Err(Error::DuplicateTreeToken {
                candidate: candidate_path.to_owned(),
                previous: previous_path,
                token: candidate.marker.token().as_str().to_owned(),
            });
        }

        candidate.revalidate_retained()?;
        previous.revalidate_retained()?;
        // A cooperating writer cannot pass either held flock. Repeating the
        // evidence audit after marker publication also makes the ordering an
        // executable invariant rather than a comment.
        require_clean_baseline(&journal, state_db)?;
        installation.revalidate_root_directory()?;

        Ok(Self {
            journal,
            candidate,
            previous,
            quarantine_attempt: Mutex::new(None),
            previous_archive_attempt: Mutex::new(None),
        })
    }

    /// Revalidate both retained inodes and their current pre-exchange names.
    pub(crate) fn verify_pre_exchange(&self, candidate_path: &Path, previous_path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(candidate_path)?;
        self.previous.verify_named_read_only(previous_path)?;
        Ok(())
    }

    /// Exchange the authenticated staged candidate with the authenticated live
    /// previous tree beneath retained parent descriptors.
    pub(crate) fn exchange_forward(&self, installation: &Installation) -> Result<(), RetainedExchangeFailure> {
        self.exchange_live_and_staged(installation, RetainedExchangeDirection::Forward)
    }

    /// Reverse an earlier forward exchange through the same retained
    /// capability namespace.
    pub(crate) fn exchange_reverse(&self, installation: &Installation) -> Result<(), RetainedExchangeFailure> {
        self.exchange_live_and_staged(installation, RetainedExchangeDirection::Reverse)
    }

    /// Finish durability after a reverse exchange which is already proven to
    /// have moved both exact trees.
    ///
    /// This path deliberately performs no rename. Retrying an exchange after
    /// an applied-but-not-yet-durable result would put the failed candidate
    /// back in the live namespace.
    pub(crate) fn finish_applied_reverse(&self, installation: &Installation) -> Result<(), Error> {
        self.require_no_journal()?;
        installation.revalidate_root_directory()?;
        let staging = self.open_exchange_staging(installation)?;
        staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            RetainedExchangeDirection::Reverse.after(),
        )?;
        self.finish_exchange(installation, &staging, RetainedExchangeDirection::Reverse.after())
    }

    fn exchange_live_and_staged(
        &self,
        installation: &Installation,
        direction: RetainedExchangeDirection,
    ) -> Result<(), RetainedExchangeFailure> {
        let not_applied = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::Ambiguous,
            source,
        };

        self.require_no_journal().map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;

        let staging = self.open_exchange_staging(installation).map_err(not_applied)?;

        staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)
            .map_err(not_applied)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            direction.before(),
        )
        .map_err(not_applied)?;

        before_retained_exchange_rename();
        self.require_no_journal().map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;
        staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)
            .map_err(not_applied)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            direction.before(),
        )
        .map_err(not_applied)?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::BeforeRename).map_err(not_applied)?;

        // Never retry this syscall: an EINTR or injected error may describe an
        // exchange which the kernel already completed.  Both retained parent
        // namespaces are reconciled below before the result is interpreted.
        let syscall_result = renameat2_exchange_once(
            &staging.file,
            LIVE_USR_NAME,
            installation.root_directory(),
            LIVE_USR_NAME,
        )
        .map_err(|source| retained_exchange_io("exchange staged and live /usr", &installation.root.join("usr"), source))
        .and_then(|()| retained_exchange_checkpoint(RetainedExchangeFaultPoint::AfterRename));

        let observed = self
            .exchange_layout(installation.root_directory(), &installation.root, &staging)
            .map_err(ambiguous)?;
        if observed == direction.before() {
            let source = match syscall_result {
                Err(source) => source,
                Ok(()) => Error::RetainedExchangeReportedSuccessWithoutMove {
                    direction: direction.as_str(),
                },
            };
            return Err(not_applied(source));
        }
        if observed != direction.after() {
            return Err(ambiguous(Error::RetainedExchangeUnexpectedLayout {
                direction: direction.as_str(),
                expected: direction.after().as_str(),
                actual: observed.as_str(),
            }));
        }

        // Once both exact trees prove the post-exchange layout, a raw syscall
        // error is merely an error-after-apply report.  Complete durability
        // through both retained parents instead of exchanging a second time.
        self.finish_exchange(installation, &staging, direction.after())
            .map_err(applied)
    }

    fn open_exchange_staging(&self, installation: &Installation) -> Result<RetainedDirectory, Error> {
        let staging_path = installation.staging_dir();
        let staging =
            RetainedDirectory::open_beneath(installation.root_directory(), STAGING_RELATIVE, staging_path.clone())?;
        let root_device = installation
            .root_directory()
            .metadata()
            .map_err(|source| retained_exchange_io("inspect retained installation root", &installation.root, source))?
            .dev();
        if root_device != staging.witness.device {
            return Err(Error::RetainedExchangeCrossDevice {
                live_parent: installation.root.clone(),
                staged_parent: staging_path,
            });
        }
        Ok(staging)
    }

    fn finish_exchange(
        &self,
        installation: &Installation,
        staging: &RetainedDirectory,
        expected: RetainedExchangeLayout,
    ) -> Result<(), Error> {
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::StagingParentSync)?;
        staging.sync("sync retained staging parent after /usr exchange")?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::InstallationRootSync)?;
        installation.root_directory().sync_all().map_err(|source| {
            retained_exchange_io(
                "sync retained installation root after /usr exchange",
                &installation.root,
                source,
            )
        })?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::FinalRevalidation)?;
        self.require_no_journal()?;
        installation.revalidate_root_directory()?;
        staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        self.require_exchange_layout(installation.root_directory(), &installation.root, staging, expected)
    }

    fn require_exchange_layout(
        &self,
        root: &std::fs::File,
        root_path: &Path,
        staging: &RetainedDirectory,
        expected: RetainedExchangeLayout,
    ) -> Result<(), Error> {
        let actual = self.exchange_layout(root, root_path, staging)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::RetainedExchangeUnexpectedLayout {
                direction: "preflight",
                expected: expected.as_str(),
                actual: actual.as_str(),
            })
        }
    }

    fn exchange_layout(
        &self,
        root: &std::fs::File,
        root_path: &Path,
        staging: &RetainedDirectory,
    ) -> Result<RetainedExchangeLayout, Error> {
        let live_path = root_path.join("usr");
        let staged_path = staging.path.join("usr");
        let live = open_retained_exchange_tree(root, &live_path)?;
        let staged = open_retained_exchange_tree(&staging.file, &staged_path)?;
        let live_role = self.retained_tree_role(&live)?;
        let staged_role = self.retained_tree_role(&staged)?;
        match (live_role, staged_role) {
            (RetainedTreeRole::Previous, RetainedTreeRole::Candidate) => Ok(RetainedExchangeLayout::CandidateStaged),
            (RetainedTreeRole::Candidate, RetainedTreeRole::Previous) => Ok(RetainedExchangeLayout::CandidateLive),
            (live, staged) => Err(Error::RetainedExchangeNamespaceMismatch {
                live: live.as_str(),
                staged: staged.as_str(),
            }),
        }
    }

    fn retained_tree_role(&self, named: &TreeMarkerStore) -> Result<RetainedTreeRole, Error> {
        match self.candidate.matches_store_read_only(named) {
            Ok(true) => return Ok(RetainedTreeRole::Candidate),
            Ok(false) => {}
            Err(source) => return Err(source),
        }
        match self.previous.matches_store_read_only(named) {
            Ok(true) => Ok(RetainedTreeRole::Previous),
            Ok(false) => Err(Error::RetainedExchangeUnknownTree),
            Err(source) => Err(source),
        }
    }

    /// Verify the forward layout after the atomic exchange.
    pub(crate) fn verify_forward_exchange(&self, live_path: &Path, previous_path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(live_path)?;
        self.previous.verify_named_read_only(previous_path)?;
        Ok(())
    }

    /// Verify the previous tree at staging or its archive using only the
    /// recovery reader.
    pub(crate) fn verify_previous_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.previous.verify_named_read_only(path)
    }

    /// Verify the candidate tree at live, staging, archive, or quarantine
    /// using only the recovery reader.
    pub(crate) fn verify_candidate_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(path)
    }

    /// Flush the filesystem containing the retained candidate and persist its
    /// authenticated root at the current name. The later crash coordinator
    /// still owns bounded descriptor-recursive inventory authentication; this
    /// barrier proves durability, not a stable descendant namespace.
    pub(crate) fn sync_candidate_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(path)?;
        self.candidate.store.sync_retained_tree()?;
        self.candidate.verify_named_read_only(path)
    }

    /// Move the exact staged previous tree into a freshly created state slot.
    ///
    /// The state slot is created beneath the retained roots directory and is
    /// never adopted from ambient pathname evidence. The move makes one
    /// `RENAME_NOREPLACE` attempt and reconciles both retained parents before
    /// interpreting its return value.
    pub(crate) fn archive_previous(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let result = self.move_previous(installation, state, RetainedPreviousMoveDirection::Archive);
        match result {
            Err(failure) if failure.outcome == RetainedPreviousMoveOutcome::NotApplied => {
                match self.finish_not_applied_previous_archive(installation, state) {
                    Ok(()) => Err(failure),
                    Err(cleanup) => Err(failure.with_abort_cleanup(cleanup)),
                }
            }
            result => result,
        }
    }

    /// Move the exact archived previous tree back into staging for the
    /// compensating exchange.
    pub(crate) fn restore_previous(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.move_previous(installation, state, RetainedPreviousMoveDirection::Restore)
    }

    /// Resume only the idempotent durability suffix of an archive already
    /// proven to have moved the exact previous tree.
    pub(crate) fn finish_applied_previous_archive(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        self.finish_applied_previous_move(installation, state, RetainedPreviousMoveDirection::Archive)
    }

    /// Resume only the idempotent durability suffix of a compensating restore
    /// already proven to have moved the exact previous tree.
    pub(crate) fn finish_applied_previous_restore(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        self.finish_applied_previous_move(installation, state, RetainedPreviousMoveDirection::Restore)
    }

    /// Retire only an exact empty state slot retained by this guard after an
    /// archive attempt which is proven not to have moved the previous tree.
    /// The slot is moved back to a non-state parking name rather than deleted,
    /// so ambient, replaced, moved, or populated directories are preserved.
    pub(crate) fn finish_not_applied_previous_archive(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| Error::PreviousArchiveAttemptLockPoisoned)?;
        let Some(attempt) = retained.as_ref() else {
            return Ok(());
        };
        let name = canonical_state_name(state)?;
        require_previous_attempt_name(attempt, state, &name)?;
        self.require_no_journal()?;
        self.revalidate_previous_move_base(installation, attempt)?;
        self.finish_previous_slot_retirement(installation, attempt)?;
        *retained = None;
        Ok(())
    }

    fn move_previous(
        &self,
        installation: &Installation,
        state: state::Id,
        direction: RetainedPreviousMoveDirection,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let not_applied = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::Ambiguous,
            source,
        };

        self.require_no_journal().map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;
        let name = canonical_state_name(state).map_err(not_applied)?;
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| not_applied(Error::PreviousArchiveAttemptLockPoisoned))?;

        let mut created_now = false;
        if retained.is_none() {
            if direction == RetainedPreviousMoveDirection::Restore {
                return Err(not_applied(Error::PreviousArchiveAttemptMissing {
                    state: i32::from(state),
                }));
            }
            *retained = Some(
                self.create_previous_archive_attempt(installation, state, &name)
                    .map_err(not_applied)?,
            );
            created_now = true;
        }
        let attempt = retained.as_ref().expect("previous archive attempt was established");
        let preflight = (|| -> Result<(), Error> {
            require_previous_attempt_name(attempt, state, &name)?;
            if direction == RetainedPreviousMoveDirection::Archive {
                self.finish_previous_archive_slot_creation(installation, attempt)?;
            }
            self.revalidate_previous_move_namespace(installation, attempt)?;
            self.require_previous_move_layout(attempt, direction.before())?;

            // A newly created archive attempt was already pre-synced
            // immediately before parking-slot creation. A resumed archive
            // attempt and every restore perform exactly one fresh pre-sync.
            if !created_now || direction == RetainedPreviousMoveDirection::Restore {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::PreviousPreSync)?;
                self.previous.store.sync_retained_tree()?;
                self.require_previous_move_layout(attempt, direction.before())?;
            }

            before_retained_previous_move_rename();
            self.require_no_journal()?;
            installation.revalidate_root_directory()?;
            self.revalidate_previous_move_namespace(installation, attempt)?;
            self.require_previous_move_layout(attempt, direction.before())?;
            retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeRename)
        })();
        if let Err(source) = preflight {
            let reconciled = self.reconcile_previous_pre_move_failure(installation, attempt, direction, source);
            if reconciled.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
                *retained = None;
            }
            return reconciled;
        }

        let (source, destination) = match direction {
            RetainedPreviousMoveDirection::Archive => (&attempt.staging, &attempt.slot),
            RetainedPreviousMoveDirection::Restore => (&attempt.slot, &attempt.staging),
        };
        // Never retry this syscall. An error, including EINTR, may describe a
        // move which the kernel already completed. Reconcile both names first.
        let syscall_result = renameat2_noreplace_once(&source.file, LIVE_USR_NAME, &destination.file, LIVE_USR_NAME)
            .map_err(|source| previous_move_io("move exact previous /usr", &destination.path.join("usr"), source))
            .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterRename));

        let observed = self.previous_move_layout(attempt).map_err(ambiguous)?;
        if observed == direction.before() {
            let source = match syscall_result {
                Err(source) => source,
                Ok(()) => Error::PreviousMoveReportedSuccessWithoutMove {
                    direction: direction.as_str(),
                },
            };
            return Err(not_applied(source));
        }
        if observed != direction.after() {
            return Err(ambiguous(Error::PreviousMoveUnexpectedLayout {
                direction: direction.as_str(),
                expected: direction.after().as_str(),
                actual: observed.as_str(),
            }));
        }

        // A syscall error is superseded by exact post-move identity evidence.
        // Durability faults remain Applied so callers can resume this suffix
        // without issuing a second rename.
        let finish = self.finish_previous_move(installation, attempt, direction);
        if finish.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
            *retained = None;
        }
        finish.map_err(applied)
    }

    fn reconcile_previous_pre_move_failure(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        direction: RetainedPreviousMoveDirection,
        source: Error,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let layout = self
            .revalidate_previous_move_base(installation, attempt)
            .and_then(|()| self.previous_move_layout(attempt));
        match layout {
            Ok(layout) if layout == direction.before() => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::NotApplied,
                source,
            }),
            Ok(layout) if layout == direction.after() => self
                .finish_previous_move(installation, attempt, direction)
                .map_err(|finish| RetainedPreviousMoveFailure {
                    outcome: RetainedPreviousMoveOutcome::Applied,
                    source: Error::PreviousMoveAppliedAfterPreflightFailure {
                        direction: direction.as_str(),
                        primary: Box::new(source),
                        finish: Box::new(finish),
                    },
                }),
            Ok(layout) => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::Ambiguous,
                source: Error::PreviousMoveUnexpectedLayout {
                    direction: direction.as_str(),
                    expected: direction.before().as_str(),
                    actual: layout.as_str(),
                },
            }),
            Err(reconciliation) => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::Ambiguous,
                source: Error::PreviousMovePreflightReconciliationFailed {
                    direction: direction.as_str(),
                    primary: Box::new(source),
                    reconciliation: Box::new(reconciliation),
                },
            }),
        }
    }

    fn create_previous_archive_attempt(
        &self,
        installation: &Installation,
        state: state::Id,
        name: &CStr,
    ) -> Result<RetainedPreviousArchiveAttempt, Error> {
        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())?;
        let staging = roots.open_child(c"staging", installation.staging_dir())?;
        let canonical_path = roots_path.join(name.to_string_lossy().as_ref());
        if roots.open_optional_child(name, canonical_path.clone())?.is_some() {
            return Err(Error::PreviousArchiveSlotExists {
                state: i32::from(state),
                path: canonical_path,
            });
        }
        self.previous_move_layout_without_slot(&staging)?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::PreviousPreSync)?;
        self.previous.store.sync_retained_tree()?;
        self.previous_move_layout_without_slot(&staging)?;

        // Prepare the slot at a bounded, non-state parking name first. Any
        // failure before the descriptor is retained can leave only inert
        // hidden residue; it can never poison the canonical decimal state
        // name. Publication into that name is a separate reconciled rename.
        let mut created = None;
        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let parking_name = previous_slot_parking_name(state, self.previous.marker.token().as_str(), index)?;
            let parking_path = roots_path.join(parking_name.to_string_lossy().as_ref());
            if roots.child_name_exists(&parking_name, parking_path.clone())? {
                continue;
            }
            match RetainedDirectory::create_private_previous_slot(&roots, &parking_name, parking_path) {
                Ok(slot) => {
                    created = Some((parking_name, slot));
                    break;
                }
                Err(Error::QuarantineSlotExists { .. }) => continue,
                Err(source) => return Err(source),
            }
        }
        let (parking_name, slot) = created.ok_or_else(|| Error::PreviousArchiveParkingExhausted {
            state: i32::from(state),
            limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
        })?;
        let attempt = RetainedPreviousArchiveAttempt {
            name: name.to_owned(),
            parking_name,
            roots,
            staging,
            slot,
        };
        Ok(attempt)
    }

    fn finish_previous_archive_slot_creation(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        attempt.slot.require_exact_entries(&[])?;
        match self.previous_slot_location(attempt)? {
            RetainedPreviousSlotLocation::Parked => {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeSlotPublish)?;
                let syscall_result = renameat2_noreplace_once(
                    &attempt.roots.file,
                    &attempt.parking_name,
                    &attempt.roots.file,
                    &attempt.name,
                )
                .map_err(|source| {
                    previous_move_io(
                        "publish exact previous-state slot",
                        &previous_slot_canonical_path(attempt),
                        source,
                    )
                })
                .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterSlotPublish));

                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Canonical => {}
                    RetainedPreviousSlotLocation::Parked => {
                        return Err(match syscall_result {
                            Err(source) => source,
                            Ok(()) => Error::PreviousArchiveSlotPublishReportedSuccessWithoutMove {
                                canonical: previous_slot_canonical_path(attempt),
                                parking: previous_slot_parking_path(attempt),
                            },
                        });
                    }
                }
            }
            RetainedPreviousSlotLocation::Canonical => {}
        }

        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::SlotSync)?;
        attempt.slot.sync("sync empty previous-state archive slot")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::RootsParentSync)?;
        attempt
            .roots
            .sync("sync roots directory after previous-state slot creation")?;
        self.revalidate_previous_move_namespace(installation, &attempt)?;
        self.require_previous_move_layout(&attempt, RetainedPreviousMoveLayout::Staged)?;
        Ok(())
    }

    fn finish_applied_previous_move(
        &self,
        installation: &Installation,
        state: state::Id,
        direction: RetainedPreviousMoveDirection,
    ) -> Result<(), Error> {
        let name = canonical_state_name(state)?;
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| Error::PreviousArchiveAttemptLockPoisoned)?;
        let attempt = retained.as_ref().ok_or(Error::PreviousArchiveAttemptMissing {
            state: i32::from(state),
        })?;
        require_previous_attempt_name(attempt, state, &name)?;
        self.revalidate_previous_move_base(installation, attempt)?;
        if direction == RetainedPreviousMoveDirection::Archive {
            self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
        }
        self.require_previous_move_layout(attempt, direction.after())?;
        let finish = self.finish_previous_move(installation, attempt, direction);
        if finish.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
            *retained = None;
        }
        finish
    }

    fn finish_previous_move(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        direction: RetainedPreviousMoveDirection,
    ) -> Result<(), Error> {
        let (source, destination) = match direction {
            RetainedPreviousMoveDirection::Archive => (&attempt.staging, &attempt.slot),
            RetainedPreviousMoveDirection::Restore => (&attempt.slot, &attempt.staging),
        };
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::SourceParentSync)?;
        source.sync("sync previous-tree source parent after move")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::DestinationParentSync)?;
        destination.sync("sync previous-tree destination parent after move")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::FinalRevalidation)?;
        self.require_no_journal()?;
        installation.revalidate_root_directory()?;
        self.revalidate_previous_move_base(installation, attempt)?;
        if direction == RetainedPreviousMoveDirection::Archive {
            self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
        }
        self.require_previous_move_layout(attempt, direction.after())?;
        if direction == RetainedPreviousMoveDirection::Restore {
            self.finish_previous_slot_retirement(installation, attempt)?;
        }
        Ok(())
    }

    fn revalidate_previous_move_namespace(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)
    }

    fn revalidate_previous_move_base(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        installation.revalidate_root_directory()?;
        attempt
            .roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)?;
        attempt.staging.revalidate_child(&attempt.roots, c"staging")?;
        if attempt.roots.witness.device != attempt.staging.witness.device
            || attempt.roots.witness.device != attempt.slot.witness.device
        {
            return Err(Error::PreviousMoveCrossDevice {
                staging: attempt.staging.path.clone(),
                archive: attempt.slot.path.clone(),
            });
        }
        Ok(())
    }

    fn previous_slot_location(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<RetainedPreviousSlotLocation, Error> {
        attempt.slot.require_retained()?;
        let canonical_path = previous_slot_canonical_path(attempt);
        let parking_path = previous_slot_parking_path(attempt);
        let canonical = attempt
            .roots
            .open_optional_child(&attempt.name, canonical_path.clone())?;
        let parking = attempt
            .roots
            .open_optional_child(&attempt.parking_name, parking_path.clone())?;
        let state = |named: Option<&RetainedDirectory>| match named {
            None => RetainedPreviousSlotNameState::Absent,
            Some(named) if named.witness == attempt.slot.witness => RetainedPreviousSlotNameState::Exact,
            Some(_) => RetainedPreviousSlotNameState::Foreign,
        };
        let canonical_state = state(canonical.as_ref());
        let parking_state = state(parking.as_ref());
        match (canonical_state, parking_state) {
            (RetainedPreviousSlotNameState::Exact, RetainedPreviousSlotNameState::Absent) => {
                Ok(RetainedPreviousSlotLocation::Canonical)
            }
            (RetainedPreviousSlotNameState::Absent, RetainedPreviousSlotNameState::Exact) => {
                Ok(RetainedPreviousSlotLocation::Parked)
            }
            _ => Err(Error::PreviousArchiveSlotNamespaceMismatch {
                canonical: canonical_path,
                canonical_state: canonical_state.as_str(),
                parking: parking_path,
                parking_state: parking_state.as_str(),
            }),
        }
    }

    fn require_previous_slot_location(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
        expected: RetainedPreviousSlotLocation,
    ) -> Result<(), Error> {
        let actual = self.previous_slot_location(attempt)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::PreviousArchiveSlotLocationMismatch {
                canonical: previous_slot_canonical_path(attempt),
                parking: previous_slot_parking_path(attempt),
                expected: match expected {
                    RetainedPreviousSlotLocation::Canonical => "canonical",
                    RetainedPreviousSlotLocation::Parked => "parked",
                },
                actual: match actual {
                    RetainedPreviousSlotLocation::Canonical => "canonical",
                    RetainedPreviousSlotLocation::Parked => "parked",
                },
            })
        }
    }

    /// Move an exact empty canonical slot back to its private parking name.
    ///
    /// This is intentionally non-destructive. A same-UID writer can replace a
    /// final pathname after it is checked, so `unlinkat` cannot safely remove
    /// the retained inode. A no-replace rename preserves every racing inode,
    /// and exact post-syscall reconciliation makes the durability suffix
    /// resumable without issuing a second rename.
    fn finish_previous_slot_retirement(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_move_layout(attempt, RetainedPreviousMoveLayout::Staged)?;
        attempt.slot.require_exact_entries(&[])?;
        match self.previous_slot_location(attempt)? {
            RetainedPreviousSlotLocation::Canonical => {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeSlotRetire)?;
                before_previous_slot_retirement_rename();
                let syscall_result = renameat2_noreplace_once(
                    &attempt.roots.file,
                    &attempt.name,
                    &attempt.roots.file,
                    &attempt.parking_name,
                )
                .map_err(|source| {
                    previous_move_io(
                        "retire exact empty previous-state slot",
                        &previous_slot_parking_path(attempt),
                        source,
                    )
                })
                .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterSlotRetire));

                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Parked => {}
                    RetainedPreviousSlotLocation::Canonical => {
                        return Err(match syscall_result {
                            Err(source) => source,
                            Ok(()) => Error::PreviousArchiveSlotRetireReportedSuccessWithoutMove {
                                canonical: previous_slot_canonical_path(attempt),
                                parking: previous_slot_parking_path(attempt),
                            },
                        });
                    }
                }
            }
            RetainedPreviousSlotLocation::Parked => {}
        }

        self.finish_parked_previous_slot_retirement(installation, attempt)
    }

    fn finish_parked_previous_slot_retirement(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Parked)?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync)?;
        attempt
            .roots
            .sync("sync roots directory after previous-state slot retirement")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation)?;
        self.require_no_journal()?;
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_move_layout(attempt, RetainedPreviousMoveLayout::Staged)?;
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Parked)
    }

    fn previous_move_layout_without_slot(
        &self,
        staging: &RetainedDirectory,
    ) -> Result<RetainedPreviousMoveLayout, Error> {
        let staged = open_optional_retained_tree(staging, &staging.path.join("usr"))?.ok_or_else(|| {
            Error::PreviousMoveTreeMissing {
                staged: staging.path.join("usr"),
                archived: PathBuf::from("<uncreated-state-slot>/usr"),
            }
        })?;
        self.previous.verify_store_read_only(&staged)?;
        Ok(RetainedPreviousMoveLayout::Staged)
    }

    fn previous_move_layout(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<RetainedPreviousMoveLayout, Error> {
        let staged_path = attempt.staging.path.join("usr");
        let archived_path = attempt.slot.path.join("usr");
        let staged = open_optional_retained_tree(&attempt.staging, &staged_path)?;
        let archived = open_optional_retained_tree(&attempt.slot, &archived_path)?;
        match (staged, archived) {
            (Some(staged), None) => {
                self.previous.verify_store_read_only(&staged)?;
                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Canonical | RetainedPreviousSlotLocation::Parked => {}
                }
                attempt.slot.require_exact_entries(&[])?;
                Ok(RetainedPreviousMoveLayout::Staged)
            }
            (None, Some(archived)) => {
                self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
                self.previous.verify_store_read_only(&archived)?;
                attempt.slot.require_exact_entries(&[b"usr"])?;
                Ok(RetainedPreviousMoveLayout::Archived)
            }
            (Some(_), Some(_)) => Err(Error::PreviousMoveBothNamesOccupied {
                staged: staged_path,
                archived: archived_path,
            }),
            (None, None) => Err(Error::PreviousMoveTreeMissing {
                staged: staged_path,
                archived: archived_path,
            }),
        }
    }

    fn require_previous_move_layout(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
        expected: RetainedPreviousMoveLayout,
    ) -> Result<(), Error> {
        let actual = self.previous_move_layout(attempt)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::PreviousMoveUnexpectedLayout {
                direction: "preflight",
                expected: expected.as_str(),
                actual: actual.as_str(),
            })
        }
    }

    /// Publish a failed candidate into one deterministic, token-derived
    /// quarantine slot and make the rename durable before its database
    /// correlation may be removed.
    pub(crate) fn quarantine_candidate(
        &self,
        installation: &Installation,
        candidate: state::Id,
        kind: FailedCandidateKind,
    ) -> Result<QuarantinedCandidate, Error> {
        self.require_no_journal()?;
        installation.revalidate_root_directory()?;

        let staging_path = installation.staging_dir();
        let source_path = installation.staging_path("usr");
        let quarantine_path = installation.state_quarantine_dir();
        let staging =
            RetainedDirectory::open_beneath(installation.root_directory(), STAGING_RELATIVE, staging_path.clone())?;
        let quarantine = RetainedDirectory::open_beneath(
            installation.root_directory(),
            QUARANTINE_RELATIVE,
            quarantine_path.clone(),
        )?;
        if staging.witness.device != quarantine.witness.device {
            return Err(Error::QuarantineCrossDevice {
                source_path: staging_path,
                destination: quarantine_path,
            });
        }

        let name = QuarantineName::parse(format!(
            "failed-{}-{candidate}-{}",
            kind.as_str(),
            self.candidate.marker.token().as_str()
        ))
        .map_err(Error::InvalidQuarantineName)?;
        let encoded_name = std::ffi::CString::new(name.as_str())
            .map_err(|source| quarantine_io("encode quarantine slot name", &quarantine.path, source.into()))?;
        let slot_path = quarantine.path.join(name.as_str());
        let destination_path = slot_path.join("usr");
        let mut retained_attempt = self
            .quarantine_attempt
            .lock()
            .map_err(|_| Error::QuarantineAttemptLockPoisoned)?;
        let existing_slot = quarantine.open_optional_child(&encoded_name, slot_path.clone())?;
        let (slot, already_moved) = match (retained_attempt.as_ref(), existing_slot) {
            (None, None) => {
                self.candidate.verify_named_read_only(&source_path)?;
                quarantine_checkpoint(QuarantineFaultPoint::CandidatePreSync)?;
                self.sync_candidate_for_recovery(&source_path)?;
                let slot = RetainedDirectory::create_private_child(&quarantine, &encoded_name, slot_path.clone())?;
                *retained_attempt = Some(RetainedQuarantineAttempt {
                    name: encoded_name.clone(),
                    slot: slot.clone_retained()?,
                });
                quarantine_checkpoint(QuarantineFaultPoint::SlotSync)?;
                slot.sync("sync empty failed-candidate quarantine slot")?;
                quarantine_checkpoint(QuarantineFaultPoint::QuarantineBaseSync)?;
                quarantine.sync("sync quarantine base after slot creation")?;
                (slot, false)
            }
            (None, Some(_)) => {
                return Err(Error::QuarantineSlotExists { path: slot_path });
            }
            (Some(_), None) => {
                return Err(Error::QuarantineDirectoryChanged { path: slot_path });
            }
            (Some(attempt), Some(slot)) => {
                if attempt.name != encoded_name {
                    return Err(Error::QuarantineAttemptChanged {
                        expected: attempt.name.to_string_lossy().into_owned(),
                        actual: name.as_str().to_owned(),
                    });
                }
                attempt.slot.require_same(&slot)?;
                if slot.witness.mode != PRIVATE_DIRECTORY_MODE {
                    return Err(Error::UnsafeQuarantineDirectory {
                        path: slot.path.clone(),
                        owner: slot.witness.owner,
                        mode: slot.witness.mode,
                    });
                }
                match slot.entries(2)?.as_slice() {
                    [] => {
                        self.candidate.verify_named_read_only(&source_path)?;
                        quarantine_checkpoint(QuarantineFaultPoint::CandidatePreSync)?;
                        self.sync_candidate_for_recovery(&source_path)?;
                        quarantine_checkpoint(QuarantineFaultPoint::SlotSync)?;
                        slot.sync("resync empty failed-candidate quarantine slot")?;
                        quarantine_checkpoint(QuarantineFaultPoint::QuarantineBaseSync)?;
                        quarantine.sync("resync quarantine base for resumed publication")?;
                        (slot, false)
                    }
                    [entry] if entry.as_slice() == b"usr" => {
                        staging.require_child_absent(LIVE_USR_NAME)?;
                        let moved = slot.open_child(LIVE_USR_NAME, destination_path.clone())?;
                        self.candidate
                            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &destination_path)?)?;
                        self.candidate.verify_named_read_only(&destination_path)?;
                        (slot, true)
                    }
                    entries => {
                        return Err(Error::UnexpectedQuarantineEntries {
                            path: slot.path.clone(),
                            entries: entries
                                .iter()
                                .map(|name| String::from_utf8_lossy(name).into_owned())
                                .collect(),
                        });
                    }
                }
            }
        };
        drop(retained_attempt);

        if !already_moved {
            staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
            quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
            slot.revalidate_child(&quarantine, &encoded_name)?;
            slot.require_child_absent(LIVE_USR_NAME)?;
            slot.require_exact_entries(&[])?;
            let source_usr = staging.open_child(LIVE_USR_NAME, source_path.clone())?;
            self.candidate
                .verify_store_read_only(&TreeMarkerStore::open(&source_usr.file, &source_path)?)?;
            self.candidate.verify_named_read_only(&source_path)?;

            quarantine_checkpoint(QuarantineFaultPoint::Rename)?;
            renameat2_noreplace(&staging.file, LIVE_USR_NAME, &slot.file, LIVE_USR_NAME)
                .map_err(|source| quarantine_io("move failed candidate into quarantine", &slot_path, source))?;
        }

        staging.require_child_absent(LIVE_USR_NAME)?;
        slot.require_exact_entries(&[b"usr"])?;
        let moved = slot.open_child(LIVE_USR_NAME, destination_path.clone())?;
        self.candidate
            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &destination_path)?)?;
        self.candidate.verify_named_read_only(&destination_path)?;

        quarantine_checkpoint(QuarantineFaultPoint::MovedCandidateSync)?;
        self.sync_candidate_for_recovery(&destination_path)?;
        quarantine_checkpoint(QuarantineFaultPoint::SourceParentSync)?;
        staging.sync("sync staging after failed-candidate removal")?;
        quarantine_checkpoint(QuarantineFaultPoint::DestinationParentSync)?;
        slot.sync("sync quarantine slot after failed-candidate publication")?;
        quarantine.sync("resync quarantine base after failed-candidate publication")?;

        let quarantined = QuarantinedCandidate {
            name: encoded_name,
            destination_path,
            staging,
            quarantine,
            slot,
        };
        quarantine_checkpoint(QuarantineFaultPoint::FinalRevalidation)?;
        self.revalidate_quarantined_candidate(installation, &quarantined)?;

        Ok(quarantined)
    }

    /// Repeat the complete durability and identity proof immediately before a
    /// fresh candidate's database correlation is removed.
    pub(crate) fn revalidate_quarantined_candidate(
        &self,
        installation: &Installation,
        quarantined: &QuarantinedCandidate,
    ) -> Result<(), Error> {
        self.require_no_journal()?;
        self.sync_candidate_for_recovery(&quarantined.destination_path)?;
        quarantined
            .staging
            .sync("resync staging before candidate invalidation")?;
        quarantined
            .slot
            .sync("resync quarantine slot before candidate invalidation")?;
        quarantined
            .quarantine
            .sync("resync quarantine base before candidate invalidation")?;
        installation.revalidate_root_directory()?;
        quarantined
            .staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        quarantined
            .quarantine
            .revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
        quarantined
            .slot
            .revalidate_child(&quarantined.quarantine, &quarantined.name)?;
        quarantined.staging.require_child_absent(LIVE_USR_NAME)?;
        quarantined.slot.require_exact_entries(&[b"usr"])?;
        let moved = quarantined
            .slot
            .open_child(LIVE_USR_NAME, quarantined.destination_path.clone())?;
        self.candidate
            .verify_store_read_only(&TreeMarkerStore::open(&moved.file, &quarantined.destination_path)?)?;
        self.candidate.verify_named_read_only(&quarantined.destination_path)
    }

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

#[derive(Debug)]
struct OpenedLiveUsr {
    pinned: std::fs::File,
    readable: std::fs::File,
}

fn open_retained_exchange_tree(parent: &std::fs::File, path: &Path) -> Result<TreeMarkerStore, Error> {
    let tree = openat2_file(
        parent.as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| retained_exchange_io("open retained /usr exchange child", path, source))?;
    TreeMarkerStore::open(&tree, path).map_err(Error::from)
}

fn open_optional_retained_tree(parent: &RetainedDirectory, path: &Path) -> Result<Option<TreeMarkerStore>, Error> {
    let tree = match openat2_file(
        parent.file.as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    ) {
        Ok(tree) => tree,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(previous_move_io("open retained previous-tree child", path, source)),
    };
    TreeMarkerStore::open(&tree, path).map(Some).map_err(Error::from)
}

fn canonical_state_name(state: state::Id) -> Result<std::ffi::CString, Error> {
    let value = i32::from(state);
    if value <= 0 {
        return Err(Error::InvalidPreviousArchiveState { state: value });
    }
    let encoded = value.to_string();
    if encoded.starts_with('0') || !encoded.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::InvalidPreviousArchiveState { state: value });
    }
    std::ffi::CString::new(encoded).map_err(|_| Error::InvalidPreviousArchiveState { state: value })
}

fn previous_slot_parking_name(
    state: state::Id,
    previous_tree_token: &str,
    index: usize,
) -> Result<std::ffi::CString, Error> {
    let name = QuarantineName::parse(format!(
        ".previous-slot-{}-{previous_tree_token}-{index}",
        i32::from(state)
    ))
    .map_err(Error::InvalidPreviousArchiveParkingName)?;
    Ok(std::ffi::CString::new(name.as_str()).expect("validated previous-slot parking name contains no NUL"))
}

fn previous_slot_canonical_path(attempt: &RetainedPreviousArchiveAttempt) -> PathBuf {
    attempt.roots.path.join(attempt.name.to_string_lossy().as_ref())
}

fn previous_slot_parking_path(attempt: &RetainedPreviousArchiveAttempt) -> PathBuf {
    attempt.roots.path.join(attempt.parking_name.to_string_lossy().as_ref())
}

fn require_previous_attempt_name(
    attempt: &RetainedPreviousArchiveAttempt,
    state: state::Id,
    name: &CStr,
) -> Result<(), Error> {
    if attempt.name.as_c_str() == name {
        Ok(())
    } else {
        Err(Error::PreviousArchiveAttemptChanged {
            expected: attempt.name.to_string_lossy().into_owned(),
            actual: i32::from(state).to_string(),
        })
    }
}

fn open_or_synthesize_live_usr(installation: &Installation) -> Result<TreeMarkerStore, Error> {
    let path = installation.root.join("usr");
    installation.revalidate_root_directory()?;
    if let Some(opened) = open_live_usr(installation, &path)? {
        require_named_live_usr(installation, &opened.pinned, &path)?;
        let store = TreeMarkerStore::open(&opened.readable, &path)?;
        // With no active-state evidence, an existing nonempty tree is neither
        // the synthesized empty baseline nor an authenticated legacy active
        // tree. Refuse to bless it with a permanent token.
        if installation.active_state.is_none() {
            require_no_access_acl(&opened.readable, &path)
                .map_err(|source| live_usr_io("reject access ACL on unowned live /usr", &path, source))?;
            let marker_only = require_empty_or_marker_only_directory(&opened.readable, &path)?;
            if marker_only {
                // A failed first-install attempt may already have durably
                // published the baseline marker. Validate and adopt that exact
                // evidence rather than permanently making the next attempt
                // reject its own marker.
                store.read_for_recovery()?;
            }
            opened
                .readable
                .sync_all()
                .map_err(|source| live_usr_io("sync pre-existing empty live /usr", &path, source))?;
            installation
                .root_directory()
                .sync_all()
                .map_err(|source| live_usr_io("sync pre-existing live /usr name", &path, source))?;
        }
        require_named_live_usr(installation, &opened.pinned, &path)?;
        installation.revalidate_root_directory()?;
        return Ok(store);
    }

    before_live_usr_mkdir();
    loop {
        // SAFETY: the retained root descriptor and static component remain
        // live. mkdirat never follows or replaces the final component.
        if unsafe {
            nix::libc::mkdirat(
                installation.root_directory().as_raw_fd(),
                LIVE_USR_NAME.as_ptr(),
                SYNTHESIZED_USR_MODE,
            )
        } == 0
        {
            break;
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::AlreadyExists => return Err(Error::LiveUsrAppeared { path }),
            _ => return Err(live_usr_io("create empty live /usr", &path, source)),
        }
    }

    let opened = open_live_usr(installation, &path)?.ok_or_else(|| Error::LiveUsrDisappeared { path: path.clone() })?;
    require_fresh_synthesized_usr(&opened.readable, &path)?;
    chmod_path_descriptor(&opened.pinned, SYNTHESIZED_USR_MODE)
        .map_err(|source| live_usr_io("normalize empty live /usr mode", &path, source))?;
    require_exact_synthesized_usr(&opened.readable, &path)?;

    // Persist the empty child and its name before a marker can be generated.
    opened
        .readable
        .sync_all()
        .map_err(|source| live_usr_io("sync empty live /usr", &path, source))?;
    installation
        .root_directory()
        .sync_all()
        .map_err(|source| live_usr_io("sync installation root after live /usr creation", &path, source))?;
    require_named_live_usr(installation, &opened.pinned, &path)?;
    let reopened =
        open_live_usr(installation, &path)?.ok_or_else(|| Error::LiveUsrDisappeared { path: path.clone() })?;
    require_same_directory(&opened.pinned, &reopened.pinned, &path)?;
    require_exact_synthesized_usr(&reopened.readable, &path)?;
    reopened
        .readable
        .sync_all()
        .map_err(|source| live_usr_io("resync authenticated empty live /usr", &path, source))?;
    installation
        .root_directory()
        .sync_all()
        .map_err(|source| live_usr_io("resync authenticated installation root", &path, source))?;
    installation.revalidate_root_directory()?;

    let store = TreeMarkerStore::open(&reopened.readable, &path)?;
    require_named_live_usr(installation, &opened.pinned, &path)?;
    Ok(store)
}

fn open_live_usr(installation: &Installation, path: &Path) -> Result<Option<OpenedLiveUsr>, Error> {
    let pinned = match openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(live_usr_io("pin live /usr", path, source)),
    };
    let readable = openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("open live /usr", path, source))?;
    require_same_directory(&pinned, &readable, path)?;
    Ok(Some(OpenedLiveUsr { pinned, readable }))
}

fn require_named_live_usr(installation: &Installation, retained: &std::fs::File, path: &Path) -> Result<(), Error> {
    installation.revalidate_root_directory()?;
    let named = openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("revalidate live /usr name", path, source))?;
    require_same_directory(retained, &named, path)
}

fn require_fresh_synthesized_usr(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| live_usr_io("inspect fresh empty live /usr", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode & !SYNTHESIZED_USR_MODE != 0 {
        return Err(Error::UnsafeSynthesizedUsr {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }
    require_no_access_acl(file, path)
        .map_err(|source| live_usr_io("reject access ACL on empty live /usr", path, source))?;
    require_no_default_acl(file, path)
        .map_err(|source| live_usr_io("reject default ACL on empty live /usr", path, source))?;
    require_empty_directory(file, path)
}

fn require_exact_synthesized_usr(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| live_usr_io("inspect normalized empty live /usr", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode != SYNTHESIZED_USR_MODE {
        return Err(Error::UnsafeSynthesizedUsr {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }
    require_no_access_acl(file, path)
        .map_err(|source| live_usr_io("reject access ACL on normalized live /usr", path, source))?;
    require_no_default_acl(file, path)
        .map_err(|source| live_usr_io("reject default ACL on normalized live /usr", path, source))?;
    require_empty_directory(file, path)
}

fn require_empty_directory(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    inspect_baseline_directory(file, path, false).map(drop)
}

/// Return true only for the exact marker-only retry baseline. Every other
/// entry, including marker temporaries and a marker plus foreign content,
/// fails closed without cleanup.
fn require_empty_or_marker_only_directory(file: &std::fs::File, path: &Path) -> Result<bool, Error> {
    inspect_baseline_directory(file, path, true)
}

fn inspect_baseline_directory(file: &std::fs::File, path: &Path, allow_marker: bool) -> Result<bool, Error> {
    // SAFETY: fcntl receives one live directory descriptor and returns a fresh
    // close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(live_usr_io(
            "duplicate empty live /usr for enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // dup shares a directory offset with the retained descriptor. Reset it so
    // repeated emptiness proofs never mistake a prior EOF for a new scan.
    // SAFETY: duplicate is one fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: duplicate is still uniquely owned here.
        unsafe { nix::libc::close(duplicate) };
        return Err(live_usr_io("rewind empty live /usr enumeration", path, source));
    }
    // SAFETY: fdopendir consumes the fresh duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(live_usr_io("enumerate empty live /usr", path, source));
    }

    let mut marker_seen = false;
    let result = loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(marker_seen)
            } else {
                Err(live_usr_io("enumerate empty live /usr", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for this live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = name.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if allow_marker && name == TREE_MARKER_NAME && !marker_seen {
            marker_seen = true;
        } else {
            break Err(Error::LiveUsrNotEmpty {
                path: path.to_owned(),
                entry: String::from_utf8_lossy(name).into_owned(),
            });
        }
    };
    // SAFETY: stream was returned by fdopendir and remains open.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(live_usr_io(
            "close empty live /usr enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

fn require_same_directory(expected: &std::fs::File, actual: &std::fs::File, path: &Path) -> Result<(), Error> {
    let expected = expected
        .metadata()
        .map_err(|source| live_usr_io("inspect retained live /usr", path, source))?;
    let actual = actual
        .metadata()
        .map_err(|source| live_usr_io("inspect reopened live /usr", path, source))?;
    if (expected.dev(), expected.ino()) == (actual.dev(), actual.ino()) {
        Ok(())
    } else {
        Err(Error::LiveUsrChanged { path: path.to_owned() })
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
                | nix::libc::O_NONBLOCK,
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

fn retained_directory_witness(file: &std::fs::File, path: &Path) -> Result<RetainedDirectoryWitness, Error> {
    let metadata = file
        .metadata()
        .map_err(|source| quarantine_io("inspect retained directory", path, source))?;
    let witness = RetainedDirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
    };
    if metadata.file_type().is_dir()
        && witness.owner == unsafe { nix::libc::geteuid() }
        && witness.mode & 0o7000 == 0
        && witness.mode & 0o022 == 0
        && witness.mode & 0o700 == 0o700
    {
        Ok(witness)
    } else {
        Err(Error::UnsafeQuarantineDirectory {
            path: path.to_owned(),
            owner: witness.owner,
            mode: witness.mode,
        })
    }
}

fn retained_directory_entries(file: &std::fs::File, path: &Path, limit: usize) -> Result<Vec<Vec<u8>>, Error> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(quarantine_io(
            "duplicate retained directory for enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: duplicate is a fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: duplicate remains uniquely owned here.
        unsafe { nix::libc::close(duplicate) };
        return Err(quarantine_io("rewind retained directory enumeration", path, source));
    }
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(quarantine_io("enumerate retained directory", path, source));
    }

    let mut entries = Vec::new();
    let result = loop {
        // SAFETY: errno is thread-local on Linux.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(entries)
            } else {
                Err(quarantine_io("enumerate retained directory", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for the returned dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        entries.push(name.to_vec());
        if entries.len() > limit {
            break Err(Error::UnexpectedQuarantineEntries {
                path: path.to_owned(),
                entries: entries
                    .into_iter()
                    .map(|name| String::from_utf8_lossy(&name).into_owned())
                    .collect(),
            });
        }
    };
    // SAFETY: stream was returned by fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(quarantine_io(
            "close retained directory enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

#[cfg(test)]
pub(crate) fn arm_quarantine_fault(point: QuarantineFaultPoint) {
    arm_quarantine_faults(point, 1);
}

#[cfg(test)]
pub(crate) fn arm_quarantine_faults(point: QuarantineFaultPoint, count: usize) {
    assert!(count > 0, "quarantine fault count must be nonzero");
    QUARANTINE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some((point, count))).is_none(),
            "quarantine fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_retained_exchange_fault(point: RetainedExchangeFaultPoint) {
    RETAINED_EXCHANGE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "retained exchange fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_retained_previous_move_fault(point: RetainedPreviousMoveFaultPoint) {
    arm_retained_previous_move_faults(&[point]);
}

#[cfg(test)]
pub(crate) fn arm_retained_previous_move_faults(points: &[RetainedPreviousMoveFaultPoint]) {
    assert!(
        !points.is_empty(),
        "retained previous-tree fault sequence must not be empty"
    );
    RETAINED_PREVIOUS_MOVE_FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_empty(), "retained previous-tree move fault already armed");
        slot.extend_from_slice(points);
    });
}

#[cfg(test)]
pub(crate) fn arm_before_previous_archive_slot_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_PREVIOUS_ARCHIVE_SLOT_REOPEN.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "previous archive slot reopen hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retained_previous_move_rename(hook: impl FnOnce() + 'static) {
    BEFORE_RETAINED_PREVIOUS_MOVE_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "retained previous-tree move hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_previous_slot_retirement_rename(hook: impl FnOnce() + 'static) {
    BEFORE_PREVIOUS_SLOT_RETIREMENT_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "previous-state slot retirement hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retained_exchange_rename(hook: impl FnOnce() + 'static) {
    BEFORE_RETAINED_EXCHANGE_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "retained exchange hook already armed"
        );
    });
}

#[cfg(test)]
fn quarantine_checkpoint(point: QuarantineFaultPoint) -> Result<(), Error> {
    QUARANTINE_FAULT.with(|slot| {
        let mut armed = slot.borrow_mut();
        match armed.as_mut() {
            Some((armed_point, remaining)) if *armed_point == point => {
                *remaining -= 1;
                if *remaining == 0 {
                    *armed = None;
                }
                Err(Error::InjectedQuarantineFault { point })
            }
            _ => Ok(()),
        }
    })
}

#[cfg(test)]
fn retained_exchange_checkpoint(point: RetainedExchangeFaultPoint) -> Result<(), Error> {
    RETAINED_EXCHANGE_FAULT.with(|slot| {
        if slot.borrow().as_ref() == Some(&point) {
            slot.replace(None);
            Err(Error::InjectedRetainedExchangeFault { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
fn retained_previous_move_checkpoint(point: RetainedPreviousMoveFaultPoint) -> Result<(), Error> {
    RETAINED_PREVIOUS_MOVE_FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.first() == Some(&point) {
            slot.remove(0);
            Err(Error::InjectedRetainedPreviousMoveFault { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn retained_exchange_checkpoint(point: RetainedExchangeFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
}

#[cfg(not(test))]
fn retained_previous_move_checkpoint(point: RetainedPreviousMoveFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
}

#[cfg(not(test))]
fn quarantine_checkpoint(point: QuarantineFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
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

#[cfg(test)]
pub(crate) fn arm_before_live_usr_mkdir(hook: impl FnOnce() + 'static) {
    BEFORE_LIVE_USR_MKDIR.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "live /usr hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_quarantine_slot_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_QUARANTINE_SLOT_REOPEN.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "quarantine slot reopen hook already armed"
        );
    });
}

#[cfg(test)]
fn before_retained_exchange_rename() {
    BEFORE_RETAINED_EXCHANGE_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn before_retained_previous_move_rename() {
    BEFORE_RETAINED_PREVIOUS_MOVE_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn before_previous_slot_retirement_rename() {
    BEFORE_PREVIOUS_SLOT_RETIREMENT_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_previous_slot_retirement_rename() {}

#[cfg(not(test))]
fn before_retained_previous_move_rename() {}

#[cfg(not(test))]
fn before_retained_exchange_rename() {}

#[cfg(test)]
fn before_live_usr_mkdir() {
    BEFORE_LIVE_USR_MKDIR.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_live_usr_mkdir() {}

#[cfg(test)]
fn before_quarantine_slot_reopen() {
    BEFORE_QUARANTINE_SLOT_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_quarantine_slot_reopen() {}

#[cfg(test)]
fn before_previous_archive_slot_reopen() {
    BEFORE_PREVIOUS_ARCHIVE_SLOT_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_previous_archive_slot_reopen() {}

fn live_usr_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::LiveUsr {
        operation,
        path: path.to_owned(),
        source,
    }
}

impl RetainedIdentity {
    fn prepare(store: TreeMarkerStore) -> Result<Self, Error> {
        let marker = store.adopt_or_create_before_journal()?;
        Ok(Self { store, marker })
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
        let named = named_store.read_expected_for_recovery(self.marker.token())?;
        self.marker.require_same_marker(&named)?;
        named.revalidate(named_store)?;
        self.revalidate_retained()?;
        self.store.require_same_directory(named_store)?;
        self.marker.require_same_marker(&named).map_err(Error::from)
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
