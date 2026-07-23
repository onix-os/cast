use std::fs;

use crate::{
    boot_publication::{BootPublicationReceiptFingerprint, BootPublicationReceiptPair},
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{UsrRollbackCompleteRouteAdmission, fresh_db_invalidation_removal_call_count},
    },
    transition_journal::{BootRollback, ForwardPhase, Phase, RollbackAction, RollbackActionOutcome, encode},
};

use super::support::{CandidateResult, FreshDbOutcome, RouteFixture, Source, canonical_journal, capture_record};

#[test]
fn startup_usr_rollback_complete_route_admits_exact_current_and_historical_joint_absence() {
    let mut cases = 0;
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::THROUGH_ROLLBACK_COMPLETE {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        cases += 1;
                        let case = (historical, origin, source, usr_outcome, candidate_outcome);
                        let fixture = if historical {
                            RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
                        } else {
                            RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
                        };
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);

                        authority.revalidate(&journal).unwrap();
                        assert_eq!(fixture.canonical_record(), fixture.source, "{case:?}");
                        fixture.assert_no_second_removal();
                    }
                }
            }
        }
    }
    assert_eq!(cases, 48);
}

#[test]
fn startup_usr_rollback_complete_route_defers_inexact_phase_plan_and_non_absent_database() {
    let fixture = RouteFixture::new(
        FreshDbOutcome::Applied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &fixture.fixture.record,).unwrap(),
        UsrRollbackCompleteRouteAdmission::NotApplicable
    ));
    let successor = fixture.expected_successor();
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &successor,).unwrap(),
        UsrRollbackCompleteRouteAdmission::NotApplicable
    ));
    fixture.assert_no_second_removal();
    drop(journal);
    drop(reservation);

    let present = RouteFixture::with_present_fresh_row(
        Source::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateResult::AlreadySatisfied,
    );
    let removals_before = fresh_db_invalidation_removal_call_count();
    let journal = present.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        present.capture(&journal, &reservation).unwrap(),
        UsrRollbackCompleteRouteAdmission::Deferred
    ));
    present.fixture.assert_exact_present();
    assert_eq!(present.canonical_record(), present.source);
    assert_eq!(fresh_db_invalidation_removal_call_count(), removals_before);
    drop(journal);
    drop(reservation);

    let boot_fixture = RouteFixture::new(
        FreshDbOutcome::AlreadySatisfied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let mut boot_repair = boot_fixture.source.clone();
    let rollback = boot_repair.rollback.as_mut().unwrap();
    rollback.source = ForwardPhase::BootSyncStarted;
    rollback.previous_archive = RollbackAction::AlreadySatisfied;
    rollback.boot = BootRollback::PendingUnverifiable;
    rollback.external_effects_may_remain = true;
    boot_repair.boot_publication_receipts = Some(BootPublicationReceiptPair {
        committed: None,
        pending: BootPublicationReceiptFingerprint::from_bytes([0x52; 32]),
    });
    assert_eq!(
        boot_repair.rollback_successor(None).unwrap().phase,
        Phase::BootRepairRequired
    );
    fs::write(
        canonical_journal(&boot_fixture.fixture.fixture.fixture.installation.root),
        encode(&boot_repair).unwrap(),
    )
    .unwrap();
    let journal = boot_fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let admission = capture_record(&boot_fixture.fixture, &journal, &reservation, &boot_repair).unwrap();
    assert!(matches!(
        admission,
        UsrRollbackCompleteRouteAdmission::NotApplicable | UsrRollbackCompleteRouteAdmission::Deferred
    ));
    assert_eq!(boot_fixture.canonical_record(), boot_repair);
    boot_fixture.assert_no_second_removal();
}
