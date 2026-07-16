use crate::{
    client::startup_reconciliation::RecoveryBlocker,
    transition_journal::{Phase, RecoveryDisposition},
};

use super::{
    assert_success_events,
    fixture::{Fixture, OperationKind, SourceCase, pending},
    reset_events, take_events,
};

#[test]
fn startup_usr_exchange_parent_durability_intent_post_matrix_persists_exact_pending_reverse_plan() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::IntentPost);
        reset_events();

        let error = fixture.enter();
        let pending = pending(&error);

        assert_eq!(pending.phase(), Phase::RollbackDecided, "{kind:?}");
        assert_eq!(
            pending.disposition(),
            RecoveryDisposition::ResumeRollback {
                phase: Phase::RollbackDecided,
            },
            "{kind:?}"
        );
        assert!(pending.blockers().is_empty(), "{kind:?}: {:?}", pending.blockers());
        fixture.assert_exact_pending_reverse_decision(&fixture.canonical_record());
        assert_success_events(&fixture);
    }
}

#[test]
fn startup_usr_exchange_parent_durability_bypasses_non_intent_post_sources() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPre, SourceCase::ExchangedPost] {
            let fixture = Fixture::new(kind, source);
            reset_events();

            let error = fixture.enter();

            assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?} {source:?}");
            fixture.assert_exact_decision(&fixture.canonical_record());
            assert!(take_events().is_empty(), "{kind:?} {source:?}");
        }

        let fixture = Fixture::new(kind, SourceCase::ExchangedPre);
        let source = fixture.source.clone();
        reset_events();

        let error = fixture.enter();
        let pending = pending(&error);

        assert_eq!(pending.phase(), Phase::UsrExchanged, "{kind:?}");
        assert!(
            pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict),
            "{kind:?}: {:?}",
            pending.blockers()
        );
        assert_eq!(fixture.canonical_record(), source, "{kind:?}");
        assert!(take_events().is_empty(), "{kind:?}");
    }
}

#[test]
fn startup_usr_exchange_parent_durability_changes_only_parent_durability_and_canonical_journal() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::IntentPost);
        let namespace_before = fixture.namespace_snapshot();
        let database_before = fixture.database_snapshot();
        let journal_before = fixture.canonical_bytes();
        reset_events();

        let error = fixture.enter();

        assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?}");
        assert_ne!(fixture.canonical_bytes(), journal_before, "{kind:?}");
        fixture.assert_exact_pending_reverse_decision(&fixture.canonical_record());
        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?}");
        assert_eq!(fixture.database_snapshot(), database_before, "{kind:?}");
        assert_success_events(&fixture);
    }
}
