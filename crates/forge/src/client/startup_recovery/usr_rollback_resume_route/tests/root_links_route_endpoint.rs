use crate::{
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RecoveryDisposition},
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_root_links_complete_fresh_entries_stop_at_exact_reverse_intent_across_operations_and_epochs() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                Fixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                Fixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let case = format!("{kind:?} historical={historical}");
            fixture.assert_source_unchanged();
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();
            reset_retained_exchange_syscall_count();

            let decision_entry = fixture.enter();
            assert_eq!(pending(&decision_entry).phase(), Phase::RollbackDecided, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            drop(decision_entry);
            let decision = fixture.canonical_record();
            fixture.assert_exact_decision(&decision);
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let route_entry = fixture.enter();
            let reverse_intent = decision.rollback_successor(None).unwrap();
            assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(pending(&route_entry).phase(), Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(
                pending(&route_entry).disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::ReverseExchangeIntent,
                },
                "{case}"
            );
            assert!(pending(&route_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), reverse_intent, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let routed_bytes = fixture.canonical_bytes();
            drop(route_entry);
            let unresolved_entry = fixture.enter();
            assert_eq!(pending(&unresolved_entry).phase(), Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(fixture.canonical_record(), reverse_intent, "{case}");
            assert_eq!(fixture.canonical_bytes(), routed_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
        }
    }
}
