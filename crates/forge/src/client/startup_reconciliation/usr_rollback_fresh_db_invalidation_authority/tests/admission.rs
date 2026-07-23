use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation, startup_reconciliation::UsrRollbackFreshDbInvalidationAdmission,
    },
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, RollbackActionOutcome,
    },
};

use super::support::{CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout, capture_record};

#[test]
fn startup_fresh_db_invalidation_admits_exact_present_apply_and_bound_joint_absence_finish_matrix() {
    let mut executions = 0;
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_FRESH_DB_INVALIDATED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    for row in [FreshRowLayout::Present, FreshRowLayout::JointlyAbsent] {
                        let fixture = if historical {
                            FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, row)
                        } else {
                            FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, row)
                        };
                        let canonical = fixture.canonical_bytes();
                        let namespace = fixture.namespace_snapshot();
                        let previous = fixture.previous_database_evidence();
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();

                        let admission = fixture.capture(&journal, &reservation).unwrap();
                        assert!(
                            matches!(
                                (row, admission),
                                (
                                    FreshRowLayout::Present,
                                    UsrRollbackFreshDbInvalidationAdmission::Apply(_)
                                ) | (
                                    FreshRowLayout::JointlyAbsent,
                                    UsrRollbackFreshDbInvalidationAdmission::Finish(_)
                                )
                            ),
                            "historical={historical} source={source:?} usr={usr_outcome:?} candidate={candidate_outcome:?} row={row:?}"
                        );
                        fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
                        executions += 1;
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}

#[test]
fn startup_fresh_db_invalidation_refuses_wrong_phase_operation_and_missing_rollback_before_effect() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshRowLayout::Present,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();

    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &fixture.candidate_preserved,).unwrap(),
        UsrRollbackFreshDbInvalidationAdmission::NotApplicable
    ));

    for operation in [Operation::ActivateArchived, Operation::ActiveReblit] {
        let mut changed = fixture.record.clone();
        changed.operation = operation;
        assert!(matches!(
            capture_record(&fixture.fixture, &journal, &reservation, &changed).unwrap(),
            UsrRollbackFreshDbInvalidationAdmission::NotApplicable
        ));
    }

    let mut missing_rollback = fixture.record.clone();
    missing_rollback.rollback = None;
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &missing_rollback,).unwrap(),
        UsrRollbackFreshDbInvalidationAdmission::Deferred
    ));
    fixture.assert_exact_present();
    assert_eq!(fixture.canonical_record(), fixture.record);
}

#[test]
fn startup_fresh_db_invalidation_plan_accepts_only_the_exact_new_state_pending_fresh_action() {
    for source in [
        ForwardPhase::UsrExchangeIntent,
        ForwardPhase::UsrExchanged,
        ForwardPhase::RootLinksComplete,
    ] {
        for usr_exchange in [RollbackAction::Applied, RollbackAction::AlreadySatisfied] {
            for candidate in [RollbackAction::Applied, RollbackAction::AlreadySatisfied] {
                let fixture = FreshDbInvalidationFixture::new(
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                    CandidateOutcome::Applied,
                    FreshRowLayout::Present,
                );
                let mut exact = fixture.record.clone();
                let plan = exact.rollback.as_mut().unwrap();
                plan.source = source;
                plan.usr_exchange = usr_exchange;
                plan.candidate.action = candidate;
                assert!(
                    super::super::fresh_db_invalidation_plan_is_exact(&exact),
                    "source={source:?} usr={usr_exchange:?} candidate={candidate:?}"
                );
            }
        }
    }

    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
        FreshRowLayout::Present,
    );
    let exact = fixture.record.clone();
    let mut cases = Vec::new();

    let mut changed = exact.clone();
    changed.phase = Phase::CandidatePreserved;
    cases.push(("phase", changed));
    let mut changed = exact.clone();
    changed.operation = Operation::ActivateArchived;
    cases.push(("operation", changed));
    let mut changed = exact.clone();
    changed.rollback = None;
    cases.push(("rollback", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().source = ForwardPhase::TransactionTriggersComplete;
    cases.push(("source", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().previous_archive = RollbackAction::Applied;
    cases.push(("previous_archive", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().usr_exchange = RollbackAction::Pending;
    cases.push(("usr_exchange", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().candidate.action = RollbackAction::Pending;
    cases.push(("candidate_action", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Rearchive;
    cases.push(("candidate_disposition", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().fresh_db = RollbackAction::AlreadySatisfied;
    cases.push(("fresh_db", changed));
    let mut changed = exact.clone();
    changed.rollback.as_mut().unwrap().boot = BootRollback::PendingUnverifiable;
    cases.push(("boot", changed));
    let mut changed = exact;
    changed.rollback.as_mut().unwrap().external_effects_may_remain = false;
    cases.push(("external_effects", changed));

    for (field, changed) in cases {
        assert!(
            !super::super::fresh_db_invalidation_plan_is_exact(&changed),
            "inexact {field} was accepted"
        );
    }
}
