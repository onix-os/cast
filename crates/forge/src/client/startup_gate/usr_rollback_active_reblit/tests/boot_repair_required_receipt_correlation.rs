//! Synthetic receipt-correlation contracts at the boot-repair boundary.
//!
//! These tests use private temporary roots only. They perform no ESP, `/boot`,
//! block-device, mount, reboot, or boot-publication operation.

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitBootRepairRequiredSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitBootRepairRequiredAdmission,
            UsrRollbackActiveReblitBootRepairRequiredAuthority,
            arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures,
        },
        startup_recovery::arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation,
    },
    db,
    state::TransitionId,
    transition_journal::{Phase, TransitionJournalStore, TransitionRecord},
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        BootRepairFixture, CandidateOrigin, Epoch, UsrRestoreOrigin,
        assert_boot_required_persistence_authority_error, assert_no_boot_synchronize_attempts,
        assert_no_candidate_effects, assert_pending_phase, build_boot_sync_started,
        build_legacy_boot_sync_started, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        expected_boot_repair_required, reset_boot_synchronize_observer, reset_candidate_effect_observers,
    },
};

#[derive(Clone, Copy)]
enum ReceiptMismatch {
    MissingPending,
    WrongTransition,
    WrongPending,
    WrongCommitted,
}

#[test]
fn startup_active_reblit_boot_repair_required_requires_exact_receipts_and_preserves_legacy_route() {
    for mismatch in [
        ReceiptMismatch::MissingPending,
        ReceiptMismatch::WrongTransition,
        ReceiptMismatch::WrongPending,
        ReceiptMismatch::WrongCommitted,
    ] {
        let epoch = if matches!(mismatch, ReceiptMismatch::WrongCommitted) {
            Epoch::Historical
        } else {
            Epoch::Current
        };
        let fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Post);
        let record = drive_boot_sync_started_to_candidate_preserved(
            &fixture,
            UsrRestoreOrigin::Applied,
            CandidateOrigin::Applied,
        );
        install_mismatch(&fixture, mismatch);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        reset_boot_synchronize_observer();

        assert_deferred(&fixture, &record);

        assert_eq!(fixture.fixture.canonical_record(), record);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();
    }

    for version in [1, 2] {
        for epoch in Epoch::ALL {
            let fixture = build_legacy_boot_sync_started(epoch, BootSyncStartedLayout::Post, version);
            let preserved = drive_boot_sync_started_to_candidate_preserved(
                &fixture,
                UsrRestoreOrigin::AlreadySatisfied,
                CandidateOrigin::AlreadySatisfied,
            );
            assert_eq!(preserved.version, version);
            assert_eq!(preserved.boot_publication_receipt_correlation().unwrap(), None);
            let receipt_head = fixture.fixture.database.boot_publication_receipt_head().unwrap();
            assert_eq!(
                receipt_head.committed(),
                (epoch == Epoch::Historical)
                    .then_some(BootPublicationReceiptFingerprint::from_bytes([0x22; 32]))
            );
            assert!(receipt_head.pending().is_none());
            assert_legacy_ready(&fixture, &preserved);
            let expected = expected_boot_repair_required(&preserved);
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = fixture.fixture.namespace_snapshot();
            reset_candidate_effect_observers();
            reset_boot_synchronize_observer();

            let error = enter_boot(&fixture);

            assert_pending_phase(&error, Phase::BootRepairRequired);
            assert_eq!(fixture.fixture.canonical_record(), expected);
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
            assert_no_candidate_effects();
            assert_no_boot_synchronize_attempts();
        }
    }
}

#[test]
fn startup_active_reblit_boot_repair_required_rejects_receipt_races_and_corruption_without_effects() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    let record = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let transition = record.transition_id.clone();
    arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures(move || {
        database
            .replace_boot_publication_receipt_head_for_test(
                None,
                Some((&transition, BootPublicationReceiptFingerprint::from_bytes([0x31; 32]))),
            )
            .unwrap();
    });
    reset_candidate_effect_observers();
    reset_boot_synchronize_observer();

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::CandidatePreserved);
    assert_eq!(fixture.fixture.canonical_record(), record);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();

    let fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    let record = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let transition = record.transition_id.clone();
    arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation(move || {
        database
            .replace_boot_publication_receipt_head_for_test(
                None,
                Some((&transition, BootPublicationReceiptFingerprint::from_bytes([0x32; 32]))),
            )
            .unwrap();
    });
    reset_candidate_effect_observers();
    reset_boot_synchronize_observer();

    let error = enter_boot(&fixture);

    assert_boot_required_persistence_authority_error(&error);
    assert_eq!(fixture.fixture.canonical_record(), record);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();

    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    let record = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::Applied,
        CandidateOrigin::Applied,
    );
    fixture
        .fixture
        .database
        .replace_boot_publication_receipt_head_raw_for_test(&db::state::BootPublicationReceiptHeadRawForTest {
            committed_receipt_sha256: None,
            pending_transition_id: Some(record.transition_id.as_str().to_owned()),
            pending_receipt_sha256: None,
        })
        .unwrap();
    assert_capture_fails_closed(&fixture, &record);

    let fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    let record = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    fixture.fixture.database.delete_boot_publication_receipt_head_for_test().unwrap();
    assert_capture_fails_closed(&fixture, &record);
}

fn install_mismatch(fixture: &BootRepairFixture, mismatch: ReceiptMismatch) {
    let record = &fixture.fixture.source;
    let pair = record
        .boot_publication_receipt_correlation()
        .unwrap()
        .expect("v3 fixture carries receipt correlation");
    let foreign = TransitionId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    match mismatch {
        ReceiptMismatch::MissingPending => fixture
            .fixture
            .database
            .clear_boot_publication_receipt_head_for_test()
            .unwrap(),
        ReceiptMismatch::WrongTransition => fixture
            .fixture
            .database
            .replace_boot_publication_receipt_head_for_test(None, Some((&foreign, pair.pending)))
            .unwrap(),
        ReceiptMismatch::WrongPending => fixture
            .fixture
            .database
            .replace_boot_publication_receipt_head_for_test(
                None,
                Some((&record.transition_id, BootPublicationReceiptFingerprint::from_bytes([0x41; 32]))),
            )
            .unwrap(),
        ReceiptMismatch::WrongCommitted => fixture
            .fixture
            .database
            .replace_boot_publication_receipt_head_for_test(
                Some(BootPublicationReceiptFingerprint::from_bytes([0x42; 32])),
                Some((&record.transition_id, pair.pending)),
            )
            .unwrap(),
    }
}

fn assert_deferred(fixture: &BootRepairFixture, record: &TransitionRecord) {
    let journal = open_journal(fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    let admission = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
    )
    .unwrap();
    assert!(matches!(
        admission,
        UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred
    ));
}

fn assert_legacy_ready(fixture: &BootRepairFixture, record: &TransitionRecord) {
    let journal = open_journal(fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    let admission = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
    )
    .unwrap();
    assert!(matches!(
        admission,
        UsrRollbackActiveReblitBootRepairRequiredAdmission::ReadyLegacyUnverified(_)
    ));
}

fn assert_capture_fails_closed(fixture: &BootRepairFixture, record: &TransitionRecord) {
    let canonical_before = fixture.fixture.canonical_bytes();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();
    reset_boot_synchronize_observer();
    let journal = open_journal(fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    let result = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
    );
    assert!(result.is_err());
    assert_eq!(fixture.fixture.canonical_bytes(), canonical_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

fn open_journal(fixture: &BootRepairFixture) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(
        fixture.fixture.installation.root_directory(),
        &fixture.fixture.installation.root,
    )
    .unwrap()
}
