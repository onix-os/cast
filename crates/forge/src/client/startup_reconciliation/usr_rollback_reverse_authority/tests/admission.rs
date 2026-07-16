//! Focused POST/PRE reverse-admission contracts.

use crate::{
    client::{active_state_snapshot::ActiveStateReservation, startup_reconciliation::UsrRollbackReverseAdmission},
    transition_journal::{InitialRollbackAction, Phase, RollbackAction, RollbackObservations, TransitionJournalStore},
};

use super::{
    fixture::{Fixture, OperationKind, SourceCase},
    support::{ReverseFixture, ReverseLayout, capture_record},
};

#[test]
fn startup_usr_rollback_reverse_admission_splits_post_apply_from_pre_finish() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPost, SourceCase::ExchangedPost] {
            for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
                let fixture = ReverseFixture::from_source(kind, source, layout);
                let before = fixture.evidence_snapshots();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                match (layout, fixture.capture(&journal, &reservation)) {
                    (ReverseLayout::Post, UsrRollbackReverseAdmission::Apply(authority)) => {
                        authority.revalidate(&journal).unwrap();
                    }
                    (ReverseLayout::Pre, UsrRollbackReverseAdmission::Finish(authority)) => {
                        authority.revalidate(&journal).unwrap();
                    }
                    _ => panic!("exact {kind:?} {source:?} {layout:?} evidence selected the wrong reverse typestate"),
                }
                fixture.assert_evidence_unchanged(&before);
            }
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_admission_accepts_historical_runtime_evidence() {
    for kind in OperationKind::ALL {
        for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
            let fixture = ReverseFixture::historical(kind, layout);
            let creation_epoch = fixture.record.creation_epoch.clone();
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            match (layout, fixture.capture(&journal, &reservation)) {
                (ReverseLayout::Post, UsrRollbackReverseAdmission::Apply(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                (ReverseLayout::Pre, UsrRollbackReverseAdmission::Finish(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                _ => panic!("historical {kind:?} {layout:?} evidence did not admit"),
            }
            assert_eq!(fixture.record.creation_epoch, creation_epoch);
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_admission_bypasses_usr_restored_and_other_phases() {
    for kind in OperationKind::ALL {
        let forward = Fixture::new(kind, SourceCase::ExchangedPost);
        let journal =
            TransitionJournalStore::open_retained(forward.installation.root_directory(), &forward.installation.root)
                .unwrap();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            capture_record(&forward, &journal, &reservation, &forward.source),
            UsrRollbackReverseAdmission::NotApplicable
        ));
        drop(reservation);
        drop(journal);

        let decided = Fixture::new(kind, SourceCase::IntentPost);
        let decision = decided
            .source
            .rollback_decision(RollbackObservations {
                allocated_candidate_id: None,
                previous_archive: None,
                usr_exchange: Some(InitialRollbackAction::Pending),
                candidate: InitialRollbackAction::Pending,
                fresh_db: (kind == OperationKind::NewState).then_some(InitialRollbackAction::Pending),
            })
            .unwrap();
        let journal =
            TransitionJournalStore::open_retained(decided.installation.root_directory(), &decided.installation.root)
                .unwrap();
        journal.advance(&decided.source, &decision).unwrap();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert_eq!(decision.phase, Phase::RollbackDecided);
        assert!(matches!(
            capture_record(&decided, &journal, &reservation, &decision),
            UsrRollbackReverseAdmission::NotApplicable
        ));
        drop(reservation);
        drop(journal);

        let restored = ReverseFixture::restored(kind);
        assert_eq!(restored.record.phase, Phase::UsrRestored);
        let journal = restored.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            restored.capture(&journal, &reservation),
            UsrRollbackReverseAdmission::NotApplicable
        ));
    }
}

#[test]
fn startup_usr_rollback_reverse_plan_requires_exact_pending_usr_action() {
    let fixture = ReverseFixture::new(OperationKind::NewState, ReverseLayout::Post);
    assert!(super::super::reverse_plan_is_exact(&fixture.reverse_intent));

    for action in [
        RollbackAction::NotRequired,
        RollbackAction::Applied,
        RollbackAction::AlreadySatisfied,
    ] {
        let mut changed = fixture.reverse_intent.clone();
        changed.rollback.as_mut().unwrap().usr_exchange = action;
        assert!(!super::super::reverse_plan_is_exact(&changed), "{action:?}");
    }

    let mut changed = fixture.reverse_intent.clone();
    changed.phase = Phase::UsrRestored;
    assert!(!super::super::reverse_plan_is_exact(&changed));

    let mut changed = fixture.reverse_intent.clone();
    changed.rollback.as_mut().unwrap().candidate.action = RollbackAction::Applied;
    assert!(!super::super::reverse_plan_is_exact(&changed));
}
