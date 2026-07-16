use std::fs;

use crate::client::startup_reconciliation::RecoveryBlocker;
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::UsrRollbackDecisionSeal,
    startup_reconciliation::{UsrRollbackDecisionAdmission, UsrRollbackDecisionAuthority},
};
use crate::transition_journal::{Phase, RecoveryDisposition, TransitionJournalStore};

use super::{
    super::{UsrRollbackDecisionPersistenceError, persist_usr_rollback_decision_and_reopen},
    fixture::{Fixture, OperationKind, SourceCase, canonical_journal, pending},
};

#[test]
fn startup_usr_rollback_decision_admitted_matrix_persists_exact_plan() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPre, SourceCase::ExchangedPost] {
            let fixture = Fixture::new(kind, source);
            let error = fixture.enter();
            let pending = pending(&error);
            assert_eq!(pending.phase(), Phase::RollbackDecided, "{kind:?} {source:?}");
            assert_eq!(
                pending.disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::RollbackDecided,
                },
                "{kind:?} {source:?}"
            );
            assert!(
                pending.blockers().is_empty(),
                "{kind:?} {source:?}: {:?}",
                pending.blockers()
            );
            fixture.assert_exact_decision(&fixture.canonical_record());
        }
    }
}

#[test]
fn startup_usr_rollback_decision_exchanged_pre_remains_incompatible() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::ExchangedPre);
        let before = fixture.canonical_bytes();
        let error = fixture.enter();
        let pending = pending(&error);
        assert_eq!(pending.phase(), Phase::UsrExchanged, "{kind:?}");
        assert!(
            pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict),
            "{kind:?}: {:?}",
            pending.blockers()
        );
        assert_eq!(fixture.canonical_bytes(), before, "{kind:?}");
        fixture.assert_source_unchanged();
    }
}

#[test]
fn startup_usr_rollback_decision_changes_only_the_canonical_journal() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::ExchangedPost);
        let namespace_before = fixture.namespace_snapshot();
        let database_before = fixture.database_snapshot();
        let canonical_before = fixture.canonical_bytes();

        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?}");

        assert_ne!(fixture.canonical_bytes(), canonical_before, "{kind:?}");
        fixture.assert_exact_decision(&fixture.canonical_record());
        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?}");
        assert_eq!(fixture.database_snapshot(), database_before, "{kind:?}");
    }

    let first = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let second = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    fs::write(canonical_journal(&second.installation.root), first.canonical_bytes()).unwrap();
    assert_eq!(second.canonical_record(), first.source);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let first_journal =
        TransitionJournalStore::open_retained(first.installation.root_directory(), &first.installation.root).unwrap();
    let seal = UsrRollbackDecisionSeal::new_for_test();
    let authority = UsrRollbackDecisionAuthority::capture(
        &seal,
        &first.installation,
        &first_journal,
        &first.database,
        &reservation,
        &first.source,
        first.database.audit_in_flight_transition().unwrap(),
    )
    .unwrap();
    let UsrRollbackDecisionAdmission::Ready(authority) = authority else {
        panic!("exact first-root evidence did not admit rollback-decision authority");
    };
    let second_journal =
        TransitionJournalStore::open_retained(second.installation.root_directory(), &second.installation.root).unwrap();
    let error = persist_usr_rollback_decision_and_reopen(second_journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackDecisionPersistenceError::Authority(_)));
    assert_eq!(first_journal.load().unwrap(), Some(first.source.clone()));
    assert_eq!(first.canonical_record(), first.source);
    assert_eq!(second.canonical_record(), first.source);
}
