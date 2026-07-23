//! Database, provenance, journal, and namespace races at the boot boundary.

use std::fs;

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_active_reblit_boot_repair_required_fresh_namespace_capture,
            arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures,
        },
        startup_recovery::arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation,
    },
    transition_journal::encode,
};

use super::{
    super::test_fixture::{BootSyncStartedLayout, canonical_journal},
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_boot_required_persistence_authority_error,
        assert_no_boot_synchronize_attempts, assert_no_candidate_effects, boot_active_wrapper_path,
        build_boot_sync_started, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        reset_boot_synchronize_observer, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_boot_repair_required_rejects_database_provenance_journal_and_namespace_races() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let source =
        drive_boot_sync_started_to_candidate_preserved(&fixture, UsrRestoreOrigin::Applied, CandidateOrigin::Applied);
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let database_error = enter_boot(&fixture);

    assert_pending_blocker(&database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(fixture.fixture.database.get(candidate).is_err());
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();

    let fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let source = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    let provenance_error = enter_boot(&fixture);

    assert_pending_blocker(&provenance_error, RecoveryBlocker::MetadataProvenanceConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(
        fixture
            .fixture
            .database
            .metadata_provenance(candidate)
            .unwrap()
            .is_none()
    );
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();

    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let source = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let changed = source.rollback_successor(None).unwrap();
    let canonical = canonical_journal(&fixture.fixture.installation.root);
    let bytes = encode(&changed).unwrap();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation(move || {
        fs::write(canonical, bytes).unwrap();
    });

    let journal_error = enter_boot(&fixture);

    assert_boot_required_persistence_authority_error(&journal_error);
    assert_eq!(fixture.fixture.canonical_record(), changed);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();

    let fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let source = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let database_before = fixture.fixture.database_snapshot();
    let inserted = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("active-reblit-boot-repair-required-race");
    let inserted_by_hook = inserted.clone();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_boot_repair_required_fresh_namespace_capture(move || {
        super::super::test_fixture::create_private_directory(&inserted_by_hook);
    });

    let namespace_error = enter_boot(&fixture);

    assert_boot_required_persistence_authority_error(&namespace_error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert!(inserted.is_dir());
    assert!(boot_active_wrapper_path(&fixture).join("usr").is_dir());
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

fn assert_pending_blocker(error: &startup_gate::Error, blocker: RecoveryBlocker) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending blocker {blocker:?}, got {error:?}");
    };
    assert!(
        pending.blockers().contains(&blocker),
        "expected blocker {blocker:?}, got {:?}",
        pending.blockers()
    );
}
