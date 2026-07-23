//! Direct negative proof for boot-repair capability pairing.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitBootRepairRequiredSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitBootRepairRequiredAdmission, UsrRollbackActiveReblitBootRepairRequiredAuthority,
        },
    },
    transition_journal::TransitionJournalStore,
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, WRAPPER_INDEX, assert_no_boot_synchronize_attempts,
        assert_no_candidate_effects, build_boot_sync_started, drive_boot_sync_started_to_candidate_preserved,
        reset_boot_synchronize_observer, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_boot_repair_required_authority_rejects_reopened_and_cross_root_journal_bindings() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let record =
        drive_boot_sync_started_to_candidate_preserved(&fixture, UsrRestoreOrigin::Applied, CandidateOrigin::Applied);
    let other = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    let _other_record = drive_boot_sync_started_to_candidate_preserved(
        &other,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let fixture_database = fixture.fixture.database_snapshot();
    let fixture_namespace = fixture.fixture.namespace_snapshot();
    let other_database = other.fixture.database_snapshot();
    let other_namespace = other.fixture.namespace_snapshot();
    let journal = open_journal(&fixture.fixture.installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    reset_candidate_effect_observers();

    let admission = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        &record,
    )
    .unwrap();
    let UsrRollbackActiveReblitBootRepairRequiredAdmission::ReadyAuthenticated(authority) = admission else {
        panic!("exact source-root CandidatePreserved boot evidence did not admit required routing");
    };
    assert_eq!(authority.wrapper_index(), WRAPPER_INDEX);
    authority.revalidate(&journal).unwrap();
    drop(journal);

    let reopened_journal = open_journal(&fixture.fixture.installation);
    let reopened_error = authority.revalidate(&reopened_journal).unwrap_err();
    assert_eq!(
        reopened_error.to_string(),
        "ActiveReblit boot-repair-required authority was paired with a different open journal store"
    );
    drop(reopened_journal);

    let other_journal = open_journal(&other.fixture.installation);
    let error = authority.revalidate(&other_journal).unwrap_err();
    assert_eq!(
        error.to_string(),
        "ActiveReblit boot-repair-required authority was paired with a different open journal store"
    );
    assert_eq!(fixture.fixture.canonical_record(), record);
    assert_eq!(fixture.fixture.database_snapshot(), fixture_database);
    assert_eq!(fixture.fixture.namespace_snapshot(), fixture_namespace);
    assert_eq!(other.fixture.database_snapshot(), other_database);
    assert_eq!(other.fixture.namespace_snapshot(), other_namespace);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

fn open_journal(installation: &crate::Installation) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap()
}
