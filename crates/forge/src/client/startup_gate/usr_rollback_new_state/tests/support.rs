use std::{
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
    path::Path,
};

use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            fresh_db_invalidation_removal_call_count, new_state_candidate_preserve_move_attempt_count,
            new_state_target_create_attempt_count, new_state_target_normalize_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count, reset_new_state_target_create_attempt_count,
            reset_new_state_target_normalize_attempt_count,
        },
        startup_recovery::{UsrRollbackCandidatePreserveDispatchError, UsrRollbackFreshDbInvalidationDispatchError},
    },
    db,
    installation::DatabaseKind,
    test_support::private_installation_tempdir,
    transition_journal::{ForwardPhase, Phase, RollbackActionOutcome, TransitionRecord, decode},
};

use super::super::{
    Error as NewStateDispatchError,
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path},
    invalidation_test_support::{
        CandidateOutcome as InvalidationCandidateOutcome, FreshDbInvalidationFixture, FreshRowLayout,
    },
    test_fixture::{OperationKind, create_private_directory},
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Dispatch Test\nID=rollback-dispatch-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-dispatch-test\" } in system\n";
const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TargetPrefix {
    Absent,
    Residue,
    Canonical,
    Preserved,
}

impl TargetPrefix {
    pub(super) const ALL: [Self; 4] = [Self::Absent, Self::Residue, Self::Canonical, Self::Preserved];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOutcome {
    Applied,
    AlreadySatisfied,
}

impl CandidateOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn journal(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    fn invalidation(self) -> InvalidationCandidateOutcome {
        match self {
            Self::Applied => InvalidationCandidateOutcome::Applied,
            Self::AlreadySatisfied => InvalidationCandidateOutcome::AlreadySatisfied,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshOutcome {
    Applied,
    AlreadySatisfied,
}

impl FreshOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn journal(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    pub(super) fn row(self) -> FreshRowLayout {
        match self {
            Self::Applied => FreshRowLayout::Present,
            Self::AlreadySatisfied => FreshRowLayout::JointlyAbsent,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EffectCounts {
    pub(super) create: usize,
    pub(super) normalize: usize,
    pub(super) candidate_move: usize,
    pub(super) fresh_removal: usize,
}

pub(super) fn reset_namespace_effect_counts() {
    reset_new_state_target_create_attempt_count();
    reset_new_state_target_normalize_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
}

pub(super) fn effect_counts() -> EffectCounts {
    EffectCounts {
        create: new_state_target_create_attempt_count(),
        normalize: new_state_target_normalize_attempt_count(),
        candidate_move: new_state_candidate_preserve_move_attempt_count(),
        fresh_removal: fresh_db_invalidation_removal_call_count(),
    }
}

pub(super) fn build_candidate(
    epoch: Epoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    prefix: TargetPrefix,
) -> CandidatePreserveFixture {
    let layout = if prefix == TargetPrefix::Preserved {
        CandidateLayout::Preserved
    } else {
        CandidateLayout::Staged
    };
    let fixture = match epoch {
        Epoch::Current => CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, layout),
        Epoch::Historical => CandidatePreserveFixture::historical(OperationKind::NewState, source, usr_outcome, layout),
    };
    if source == CandidateSource::Exchanged {
        install_live_root_abi(&fixture.fixture.installation);
    }
    if matches!(prefix, TargetPrefix::Residue | TargetPrefix::Canonical) {
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        create_private_directory(&target);
        if prefix == TargetPrefix::Residue {
            fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
        }
    }
    fixture
}

pub(super) fn build_non_new_state(kind: OperationKind) -> CandidatePreserveFixture {
    assert_ne!(kind, OperationKind::NewState);
    CandidatePreserveFixture::new(
        kind,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    )
}

pub(super) fn persist_candidate_preserved(
    fixture: &CandidatePreserveFixture,
    outcome: CandidateOutcome,
) -> TransitionRecord {
    let successor = fixture
        .candidate_intent
        .rollback_successor(Some(outcome.journal()))
        .unwrap();
    assert_eq!(successor.phase, Phase::CandidatePreserved);
    let journal = fixture.open_journal();
    journal.advance(&fixture.candidate_intent, &successor).unwrap();
    drop(journal);
    successor
}

pub(super) fn build_fresh_invalidation(
    epoch: Epoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    candidate_outcome: CandidateOutcome,
    fresh_outcome: FreshOutcome,
) -> FreshDbInvalidationFixture {
    match epoch {
        Epoch::Current => FreshDbInvalidationFixture::new(
            source,
            usr_outcome,
            candidate_outcome.invalidation(),
            fresh_outcome.row(),
        ),
        Epoch::Historical => FreshDbInvalidationFixture::historical(
            source,
            usr_outcome,
            candidate_outcome.invalidation(),
            fresh_outcome.row(),
        ),
    }
}

pub(super) fn persist_fresh_invalidated(
    fixture: &FreshDbInvalidationFixture,
    outcome: FreshOutcome,
) -> TransitionRecord {
    fixture.assert_exact_joint_absence();
    let successor = fixture.record.rollback_successor(Some(outcome.journal())).unwrap();
    assert_eq!(successor.phase, Phase::FreshDbInvalidated);
    let journal = fixture.open_journal();
    journal.advance(&fixture.record, &successor).unwrap();
    drop(journal);
    successor
}

pub(super) fn persist_rollback_complete(
    fixture: &FreshDbInvalidationFixture,
    invalidated: &TransitionRecord,
) -> TransitionRecord {
    let successor = invalidated.rollback_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::RollbackComplete);
    let journal = fixture.open_journal();
    journal.advance(invalidated, &successor).unwrap();
    drop(journal);
    if successor.rollback.as_ref().unwrap().source == ForwardPhase::UsrExchanged {
        install_live_root_abi(&fixture.fixture.fixture.installation);
    }
    successor
}

pub(super) fn enter(
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
) -> startup_gate::Error {
    match enter_result(installation, database, layout_database) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved transition"),
        Err(error) => error,
    }
}

pub(super) fn enter_candidate(fixture: &CandidatePreserveFixture) -> startup_gate::Error {
    enter(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        &fixture.fixture.layout_database,
    )
}

pub(super) fn enter_invalidation(fixture: &FreshDbInvalidationFixture) -> startup_gate::Error {
    enter(
        &fixture.fixture.fixture.installation,
        &fixture.fixture.fixture.database,
        &fixture.fixture.fixture.layout_database,
    )
}

pub(super) fn enter_clean(
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
) {
    drop(retain_clean(installation, database, layout_database));
}

pub(super) fn enter_clean_invalidation(fixture: &FreshDbInvalidationFixture) {
    enter_clean(
        &fixture.fixture.fixture.installation,
        &fixture.fixture.fixture.database,
        &fixture.fixture.fixture.layout_database,
    )
}

pub(super) fn enter_clean_candidate(fixture: &CandidatePreserveFixture) {
    enter_clean(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        &fixture.fixture.layout_database,
    )
}

pub(super) fn retain_clean_invalidation(fixture: &FreshDbInvalidationFixture) -> CleanSystemStartup {
    retain_clean(
        &fixture.fixture.fixture.installation,
        &fixture.fixture.fixture.database,
        &fixture.fixture.fixture.layout_database,
    )
}

pub(super) fn assert_pending_phase(error: &startup_gate::Error, phase: Phase) {
    match error {
        startup_gate::Error::RecoveryPending(pending) => assert_eq!(pending.phase(), phase),
        other => panic!("expected {phase:?} recovery-pending result, got {other:?}"),
    }
}

pub(super) fn assert_candidate_not_applied(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackNewStateDispatch(NewStateDispatchError::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::NotApplied
            ))
        ),
        "expected candidate NotApplied, got {error:?}"
    );
}

pub(super) fn assert_candidate_ambiguous(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackNewStateDispatch(NewStateDispatchError::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::Ambiguous
            ))
        ),
        "expected candidate Ambiguous, got {error:?}"
    );
}

pub(super) fn assert_fresh_not_applied(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackNewStateDispatch(NewStateDispatchError::FreshDbInvalidationDispatch(
                UsrRollbackFreshDbInvalidationDispatchError::NotApplied
            ))
        ),
        "expected fresh-database NotApplied, got {error:?}"
    );
}

pub(super) fn assert_fresh_ambiguous(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackNewStateDispatch(NewStateDispatchError::FreshDbInvalidationDispatch(
                UsrRollbackFreshDbInvalidationDispatchError::Ambiguous
            ))
        ),
        "expected fresh-database Ambiguous, got {error:?}"
    );
}

pub(super) fn assert_suffix_dispatch_error(error: &startup_gate::Error) {
    assert!(
        matches!(error, startup_gate::Error::UsrRollbackNewStateDispatch(_)),
        "expected typed NewState suffix error, got {error:?}"
    );
}

pub(super) fn target_path(fixture: &CandidatePreserveFixture) -> std::path::PathBuf {
    transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent)
}

pub(super) fn invalidation_target_path(
    fixture: &FreshDbInvalidationFixture,
    record: &TransitionRecord,
) -> std::path::PathBuf {
    transition_quarantine_path(&fixture.fixture.fixture, record)
}

pub(super) fn install_persistent_database(fixture: &mut CandidatePreserveFixture) {
    let database = open_state_database(&fixture.fixture.installation);
    let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
    let candidate = database
        .add_with_transition(
            &fixture.candidate_intent.transition_id,
            &[],
            Some("rollback fresh candidate"),
            None,
        )
        .unwrap()
        .id;
    assert_eq!(previous, fixture.fixture.previous_state);
    assert_eq!(candidate, fixture.fixture.candidate_state);
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate,
            &fixture.candidate_intent.transition_id,
            &provenance,
        )
        .unwrap();
    let old = std::mem::replace(&mut fixture.fixture.database, database);
    drop(old);
}

pub(super) fn install_persistent_joint_absence_database(fixture: &mut FreshDbInvalidationFixture) {
    install_persistent_database(&mut fixture.fixture);
    let database = &fixture.fixture.fixture.database;
    let candidate = fixture.fixture.fixture.candidate_state;
    let observation = database
        .inspect_exact_fresh_transition(candidate, &fixture.record.transition_id)
        .unwrap();
    let db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
        panic!("persistent terminal rollback fixture expected one complete fresh-state preimage");
    };
    database.remove_exact_fresh_transition(preimage).unwrap();
    fixture.assert_exact_joint_absence();
}

pub(super) fn reopen_persistent_state_database(installation: &Installation) -> db::state::Database {
    open_state_database(installation)
}

pub(super) fn enter_fresh_handles(root: &Path) -> startup_gate::Error {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    enter(&installation, &database, &layout_database)
}

pub(super) fn enter_fresh_clean_handles(root: &Path) {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    enter_clean(&installation, &database, &layout_database);
}

pub(super) fn release_candidate_handles(mut fixture: CandidatePreserveFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(&mut fixture.fixture._temporary, private_installation_tempdir());
    drop(fixture);
    retained
}

pub(super) fn release_invalidation_fixture_handles(mut fixture: FreshDbInvalidationFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(&mut fixture.fixture.fixture._temporary, private_installation_tempdir());
    drop(fixture);
    retained
}

fn open_state_database(installation: &Installation) -> db::state::Database {
    let location = installation.mutable_database_location(DatabaseKind::State).unwrap();
    let (url, anchor) = location.parts();
    let database = db::state::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}

pub(super) fn open_layout_database(installation: &Installation) -> db::layout::Database {
    let location = installation.mutable_database_location(DatabaseKind::Layout).unwrap();
    let (url, anchor) = location.parts();
    let database = db::layout::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}

fn enter_result(
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
) -> Result<CleanSystemStartup, startup_gate::Error> {
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(installation, database, layout_database, &reservation)
}

fn retain_clean(
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
) -> CleanSystemStartup {
    enter_result(installation, database, layout_database)
        .unwrap_or_else(|error| panic!("expected clean startup, got {error:?}"))
}

pub(super) fn canonical_record(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}

pub(super) fn assert_canonical_absent(root: &Path) {
    assert!(!root.join(".cast/journal/state-transition").exists());
}

fn install_live_root_abi(installation: &Installation) {
    for (name, target) in ROOT_ABI {
        symlink(target, installation.root.join(name)).unwrap();
    }
}
