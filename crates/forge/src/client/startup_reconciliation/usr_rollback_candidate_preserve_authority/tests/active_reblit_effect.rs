//! Focused test-sealed ActiveReblit whole-wrapper effect contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::{
    UsrRollbackActiveReblitCandidatePreserveApplyReconciliation, UsrRollbackActiveReblitCandidatePreserveEffectLease,
};
use super::{
    fixture::OperationKind,
    support::{
        CandidateLayout, CandidatePreserveFixture, CandidateSource, active_reblit_wrapper_path,
        reserved_active_reblit_wrapper_path,
    },
};
use crate::client::startup_reconciliation::activation_namespace::{
    ActiveReblitCandidatePreserveExchangeFault, active_reblit_candidate_preserve_exchange_attempt_count,
    arm_active_reblit_candidate_preserve_exchange_fault,
    arm_before_active_reblit_candidate_preserve_reconciliation_capture,
    reset_active_reblit_candidate_preserve_exchange_attempt_count,
};

fn staged() -> CandidatePreserveFixture {
    CandidatePreserveFixture::new(
        OperationKind::ActiveReblit,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    )
}

fn apply_lease<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackActiveReblitCandidatePreserveEffectLease<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact staged ActiveReblit evidence did not admit Apply")
    };
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    authority.into_active_reblit_effect_for_test(&seal, journal).unwrap()
}

fn reconcile<'reservation>(
    lease: UsrRollbackActiveReblitCandidatePreserveEffectLease<'reservation>,
    journal: &TransitionJournalStore,
) -> UsrRollbackActiveReblitCandidatePreserveApplyReconciliation<'reservation> {
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    lease.reconcile(&seal, journal).unwrap()
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_preserves_the_original_wrapper_and_non_namespace_evidence() {
    let fixture = staged();
    fs::set_permissions(
        fixture.fixture.installation.staging_dir(),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let target = reserved_active_reblit_wrapper_path(&fixture, CandidateLayout::Staged);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let result = reconcile(apply_lease(&fixture, &journal, &reservation), &journal);
    let UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(authority) = result else {
        panic!("exact wrapper exchange was not classified Applied")
    };
    drop(authority);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    assert_eq!(
        fs::metadata(fixture.fixture.installation.staging_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o750);
    assert!(target.join("usr").is_dir());
    assert_eq!(
        fs::read_dir(fixture.fixture.installation.staging_dir())
            .unwrap()
            .count(),
        0
    );
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_classifies_both_unapplied_raw_reports_from_fresh_pre() {
    for fault in [
        ActiveReblitCandidatePreserveExchangeFault::ErrorWithoutApply,
        ActiveReblitCandidatePreserveExchangeFault::SuccessWithoutApply,
    ] {
        let fixture = staged();
        let before = fixture.evidence_snapshots();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_active_reblit_candidate_preserve_exchange_attempt_count();
        arm_active_reblit_candidate_preserve_exchange_fault(fault);

        let result = reconcile(apply_lease(&fixture, &journal, &reservation), &journal);
        assert!(matches!(
            result,
            UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::NotApplied
        ));
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
        fixture.assert_evidence_unchanged(&before);
    }
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_classifies_applied_error_from_fresh_post() {
    let fixture = staged();
    let target = reserved_active_reblit_wrapper_path(&fixture, CandidateLayout::Staged);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_active_reblit_candidate_preserve_exchange_fault(ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply);

    let result = reconcile(apply_lease(&fixture, &journal, &reservation), &journal);
    assert!(matches!(
        result,
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(_)
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    assert!(target.join("usr").is_dir());
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_classifies_changed_post_evidence_as_ambiguous() {
    let fixture = staged();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_before_active_reblit_candidate_preserve_reconciliation_capture(
        fixture.namespace_change_hook("active-reblit-post-race".to_owned()),
    );

    let result = reconcile(apply_lease(&fixture, &journal, &reservation), &journal);
    assert!(matches!(
        result,
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Ambiguous
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_refuses_a_rebound_fixed_name_before_attempt() {
    let fixture = staged();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    let target = reserved_active_reblit_wrapper_path(&fixture, CandidateLayout::Staged);
    let displaced = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("active-reblit-rebound-reservation");
    fs::rename(&target, displaced).unwrap();
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_refuses_a_rebound_staging_name_before_attempt() {
    let fixture = staged();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    let staging = fixture.fixture.installation.staging_dir();
    let displaced = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("active-reblit-displaced-staging");
    fs::rename(&staging, displaced).unwrap();
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_refuses_post_lease_database_drift_before_attempt() {
    let fixture = staged();
    let journal_before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    fixture
        .fixture
        .database
        .delete_metadata_provenance_for_test(fixture.fixture.candidate_state)
        .unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert_eq!(fixture.fixture.canonical_bytes(), journal_before);
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_refuses_post_lease_journal_drift_before_attempt() {
    let fixture = staged();
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    fixture.journal_change_hook()();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert_eq!(fixture.fixture.database_snapshot(), before.1);
    assert_eq!(fixture.fixture.namespace_snapshot(), before.2);
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_refuses_reservation_mode_drift_before_attempt() {
    let fixture = staged();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    fs::set_permissions(
        reserved_active_reblit_wrapper_path(&fixture, CandidateLayout::Staged),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_whole_wrapper_exchange_preserves_a_nonzero_private_index() {
    let fixture = staged().with_active_reblit_wrapper_index(7);
    let target = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, 7);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let result = reconcile(apply_lease(&fixture, &journal, &reservation), &journal);
    assert!(matches!(
        result,
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(_)
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    assert!(target.join("usr").is_dir());
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_finish_reconciles_exact_post_without_an_exchange() {
    let fixture = CandidatePreserveFixture::new(
        OperationKind::ActiveReblit,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateLayout::Preserved,
    )
    .with_active_reblit_wrapper_index(9);
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
        panic!("exact preserved ActiveReblit evidence did not admit Finish")
    };
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    let _authority = authority
        .reconcile_active_reblit_finish_for_test(&seal, &journal)
        .unwrap();
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_evidence_unchanged(&before);
}

#[test]
fn startup_active_reblit_production_selection_remains_fieldless_unsupported() {
    let fixture = staged();
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("exact staged ActiveReblit evidence did not admit Apply")
    };
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        authority.into_effect_selection(&seal, &journal).unwrap(),
        UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_evidence_unchanged(&before);
}
