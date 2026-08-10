//! One-shot reservation of the absent ActiveReblit replacement wrapper.
//!
//! A crash before the forward reservation leaves the candidate staged with
//! nothing to preserve it into. These tests pin that the checkpoint reserves
//! exactly that wrapper, requires a restart, and that the restart reaches the
//! ordinary staged exchange rather than a second reservation.

use std::{fs, os::unix::fs::MetadataExt as _};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        ActiveReblitWrapperReservationFault, UsrRollbackActiveReblitCandidatePreserveApplyReconciliation,
        UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation,
        UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
        active_reblit_candidate_preserve_exchange_attempt_count, active_reblit_wrapper_reservation_attempt_count,
        arm_active_reblit_wrapper_reservation_fault, reset_active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_wrapper_reservation_attempt_count,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

use super::support::{CandidatePreserveFixture, active_reblit_wrapper_path};

macro_rules! reservation_lease {
    ($fixture:expr, $journal:expr, $reservation:expr) => {{
        let UsrRollbackCandidatePreserveAdmission::Apply(authority) = $fixture.capture($journal, $reservation) else {
            panic!("absent ActiveReblit reservation did not admit Apply authority");
        };
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
        let UsrRollbackCandidatePreserveApplyEffectSelection::ReserveActiveReblitWrapper(lease) =
            authority.into_effect_selection(&seal, $journal).unwrap()
        else {
            panic!("absent ActiveReblit reservation did not select its reservation lease");
        };
        lease
    }};
}

#[test]
fn startup_active_reblit_absent_reservation_reconciles_raw_reports_semantically_and_requires_restart() {
    let cases = [
        (None, true),
        (Some(ActiveReblitWrapperReservationFault::ErrorAfterApply), true),
        (Some(ActiveReblitWrapperReservationFault::ErrorWithoutApply), false),
        (Some(ActiveReblitWrapperReservationFault::SuccessWithoutApply), false),
    ];

    for (fault, expect_restart) in cases {
        let fixture = CandidatePreserveFixture::active_reblit_candidate_prepared();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = reservation_lease!(&fixture, &journal, &reservation);
        let wrapper = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, 0);
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
        reset_active_reblit_wrapper_reservation_attempt_count();
        reset_active_reblit_candidate_preserve_exchange_attempt_count();
        if let Some(fault) = fault {
            arm_active_reblit_wrapper_reservation_fault(fault);
        }

        let result = lease.reconcile(&seal, &journal).unwrap();

        assert_eq!(active_reblit_wrapper_reservation_attempt_count(), 1, "{fault:?}");
        match (expect_restart, result) {
            (true, UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::RestartRequired(_)) => {
                let metadata = fs::symlink_metadata(&wrapper).unwrap();
                assert!(metadata.file_type().is_dir());
                assert_eq!(metadata.uid(), nix::unistd::Uid::effective().as_raw());
                assert_eq!(metadata.mode() & 0o7777 & !0o700, 0);
                assert!(fs::read_dir(&wrapper).unwrap().next().is_none());
            }
            (false, UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::NotApplied) => {
                assert!(!wrapper.exists());
            }
            (_, UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::Ambiguous) => {
                panic!("stable semantic evidence was ambiguous for {fault:?}");
            }
            (true, UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::NotApplied) => {
                panic!("reserved wrapper was classified NotApplied for {fault:?}");
            }
            (false, UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::RestartRequired(_)) => {
                panic!("absent wrapper was classified RestartRequired for {fault:?}");
            }
        }
        // Reservation is the whole effect: the candidate stays put and no
        // exchange is attempted in this pass.
        assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_active_reblit_reserved_wrapper_restart_reaches_the_ordinary_staged_exchange() {
    let fixture = CandidatePreserveFixture::active_reblit_candidate_prepared();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_wrapper_reservation_attempt_count();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let lease = reservation_lease!(&fixture, &journal, &reservation);
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::RestartRequired(_)
    ));
    assert_eq!(active_reblit_wrapper_reservation_attempt_count(), 1);
    fixture.assert_non_namespace_unchanged();

    // The restart must classify the reserved shape, not reserve again.
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("restart after reservation did not admit Apply authority");
    };
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit(lease) =
        authority.into_effect_selection(&seal, &journal).unwrap()
    else {
        panic!("restart after reservation did not select the staged exchange");
    };
    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(_)
    ));
    assert_eq!(active_reblit_wrapper_reservation_attempt_count(), 1);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

    // The candidate now lives in the wrapper the reservation created.
    let wrapper = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, 0);
    assert!(wrapper.join("usr").is_dir());
    assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
}
