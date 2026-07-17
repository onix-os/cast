use crate::{
    client::{active_state_snapshot::ActiveStateReservation, startup_reconciliation::UsrRollbackFinalizationAdmission},
    transition_journal::{Operation, RollbackActionOutcome},
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source, capture_record};

#[test]
fn startup_usr_rollback_finalization_admits_exact_current_and_historical_terminal_evidence() {
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        let case = (historical, origin, source, usr_outcome, candidate_outcome);
                        let fixture = if historical {
                            FinalizationFixture::historical(origin, source, usr_outcome, candidate_outcome)
                        } else {
                            FinalizationFixture::new(origin, source, usr_outcome, candidate_outcome)
                        };
                        let canonical = fixture.canonical_bytes();
                        let namespace = fixture.namespace_snapshot();
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);

                        authority.revalidate(&journal).unwrap();
                        assert_eq!(authority.record(), &fixture.record, "{case:?}");
                        assert_eq!(
                            authority.installation().root,
                            fixture.fixture.fixture.fixture.installation.root
                        );
                        fixture.assert_terminal_unchanged(&canonical, &namespace);
                    }
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_finalization_rejects_inexact_phase_operation_plan_and_database() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::Applied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let canonical = fixture.canonical_bytes();
    let namespace = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();

    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &fixture.fixture.record,).unwrap(),
        UsrRollbackFinalizationAdmission::NotApplicable
    ));

    let mut other_operation = fixture.record.clone();
    other_operation.operation = Operation::ActivateArchived;
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &other_operation,).unwrap(),
        UsrRollbackFinalizationAdmission::NotApplicable
    ));

    let mut inexact_plan = fixture.record.clone();
    inexact_plan.rollback.as_mut().unwrap().external_effects_may_remain = false;
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &inexact_plan,).unwrap(),
        UsrRollbackFinalizationAdmission::Deferred
    ));
    fixture.assert_terminal_unchanged(&canonical, &namespace);
    drop(journal);
    drop(reservation);

    let present = FinalizationFixture::with_present_fresh_row(
        Source::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateResult::AlreadySatisfied,
    );
    let journal = present.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        present.capture(&journal, &reservation).unwrap(),
        UsrRollbackFinalizationAdmission::Deferred
    ));
    present.fixture.assert_exact_present();
    assert_eq!(present.canonical_record(), present.record);
}
