//! Focused staged/preserved candidate-admission contracts.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveTopology},
    },
    transition_journal::{AbortDisposition, BootRollback, ForwardPhase, Phase, RollbackAction, RollbackActionOutcome},
};

use super::{
    fixture::OperationKind,
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, capture_record},
};

#[test]
fn startup_candidate_preserve_admission_splits_every_exact_staged_and_preserved_matrix_case() {
    for kind in OperationKind::ALL {
        for source in CandidateSource::ALL {
            for usr_reverse_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
                    let fixture = CandidatePreserveFixture::new(kind, source, usr_reverse_outcome, layout);
                    let before = fixture.evidence_snapshots();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    match (layout, fixture.capture(&journal, &reservation)) {
                        (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                            require_expected_topology(kind, layout, authority.topology());
                            authority.revalidate(&journal).unwrap();
                        }
                        (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                            require_expected_topology(kind, layout, authority.topology());
                            authority.revalidate(&journal).unwrap();
                        }
                        _ => panic!(
                            "exact {kind:?} {source:?} {usr_reverse_outcome:?} {layout:?} evidence selected the wrong typestate"
                        ),
                    }
                    fixture.assert_evidence_unchanged(&before);
                }
            }
        }
    }
}

#[test]
fn startup_candidate_preserve_admission_accepts_new_state_empty_quarantine_prefix() {
    let fixture = CandidatePreserveFixture::with_new_state_empty_quarantine_prefix();
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("exact empty-quarantine crash prefix did not admit staged authority");
    };
    assert_eq!(
        authority.topology(),
        UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine
    );
    authority.revalidate(&journal).unwrap();
    fixture.assert_evidence_unchanged(&before);
}

#[test]
fn startup_candidate_preserve_admission_accepts_historical_runtime_evidence() {
    for kind in OperationKind::ALL {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture = CandidatePreserveFixture::historical(
                kind,
                CandidateSource::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                layout,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            match (layout, fixture.capture(&journal, &reservation)) {
                (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                _ => panic!("historical {kind:?} {layout:?} evidence did not admit"),
            }
        }
    }
}

#[test]
fn startup_candidate_preserve_admission_bypasses_other_phases_and_sources() {
    let fixture = CandidatePreserveFixture::new(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &fixture.fixture.source),
        UsrRollbackCandidatePreserveAdmission::NotApplicable
    ));

    let mut unsupported = fixture.candidate_intent.clone();
    unsupported.rollback.as_mut().unwrap().source = ForwardPhase::TransactionTriggersComplete;
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &unsupported),
        UsrRollbackCandidatePreserveAdmission::NotApplicable
    ));
}

#[test]
fn startup_candidate_preserve_plan_requires_the_exact_operation_matrix() {
    for kind in OperationKind::ALL {
        let fixture = CandidatePreserveFixture::new(
            kind,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        );
        assert!(super::super::candidate_preserve_plan_is_exact(
            &fixture.candidate_intent
        ));

        let mut changed = fixture.candidate_intent.clone();
        changed.phase = Phase::CandidatePreserved;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().previous_archive = RollbackAction::Applied;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        for action in [RollbackAction::NotRequired, RollbackAction::Pending] {
            let mut changed = fixture.candidate_intent.clone();
            changed.rollback.as_mut().unwrap().usr_exchange = action;
            assert!(!super::super::candidate_preserve_plan_is_exact(&changed));
        }

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().boot = BootRollback::Unverified;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().fresh_db = match kind {
            OperationKind::NewState => RollbackAction::NotRequired,
            OperationKind::Archived | OperationKind::ActiveReblit => RollbackAction::Pending,
        };
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().candidate.disposition = match kind {
            OperationKind::Archived => AbortDisposition::Quarantine,
            OperationKind::NewState | OperationKind::ActiveReblit => AbortDisposition::Rearchive,
        };
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        let rollback = changed.rollback.as_mut().unwrap();
        rollback.external_effects_may_remain = !rollback.external_effects_may_remain;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));
    }
}

fn require_expected_topology(
    kind: OperationKind,
    layout: CandidateLayout,
    topology: UsrRollbackCandidatePreserveTopology,
) {
    match (kind, layout, topology) {
        (OperationKind::NewState, CandidateLayout::Staged, UsrRollbackCandidatePreserveTopology::NewStateStaged)
        | (
            OperationKind::NewState,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::NewStatePreserved,
        )
        | (
            OperationKind::Archived,
            CandidateLayout::Staged,
            UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot,
        )
        | (
            OperationKind::Archived,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::ArchivedPreserved,
        )
        | (
            OperationKind::ActiveReblit,
            CandidateLayout::Staged,
            UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index: 0 },
        )
        | (
            OperationKind::ActiveReblit,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index: 0 },
        ) => {}
        _ => panic!("unexpected topology {topology:?} for {kind:?} {layout:?}"),
    }
}
