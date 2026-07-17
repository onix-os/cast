use crate::{
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RecoveryDisposition, RollbackActionOutcome},
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_usr_rollback_resume_route_decision_route_and_reverse_use_one_persistence_boundary_per_entry() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPre, SourceCase::IntentPost, SourceCase::ExchangedPost] {
            let fixture = Fixture::new(kind, source);
            fixture.assert_source_unchanged();
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();
            reset_retained_exchange_syscall_count();

            let first = fixture.enter();
            assert_eq!(pending(&first).phase(), Phase::RollbackDecided, "{kind:?} {source:?}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?} {source:?}");
            drop(first);
            let decision = fixture.canonical_record();
            if source == SourceCase::IntentPost {
                fixture.assert_exact_pending_reverse_decision(&decision);
            } else {
                fixture.assert_exact_decision(&decision);
            }

            let second = fixture.enter();
            let pending_transition = pending(&second);
            let expected = decision.rollback_successor(None).unwrap();
            assert_eq!(pending_transition.phase(), expected.phase, "{kind:?} {source:?}");
            assert_eq!(
                pending_transition.disposition(),
                RecoveryDisposition::ResumeRollback { phase: expected.phase },
                "{kind:?} {source:?}"
            );
            assert!(pending_transition.blockers().is_empty(), "{kind:?} {source:?}");
            assert_eq!(fixture.canonical_record(), expected, "{kind:?} {source:?}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?} {source:?}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?} {source:?}");
            assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} {source:?}");

            drop(second);
            let third = fixture.enter();
            if expected.phase == Phase::ReverseExchangeIntent {
                let restored = expected
                    .rollback_successor(Some(RollbackActionOutcome::Applied))
                    .unwrap();
                assert_eq!(pending(&third).phase(), Phase::UsrRestored, "{kind:?} {source:?}");
                assert_eq!(fixture.canonical_record(), restored, "{kind:?} {source:?}");
                assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} {source:?}");
                assert_ne!(fixture.namespace_snapshot(), namespace_before, "{kind:?} {source:?}");
                assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} {source:?}");

                drop(third);
                let fourth = fixture.enter();
                assert_eq!(pending(&fourth).phase(), Phase::UsrRestored, "{kind:?} {source:?}");
                assert_eq!(fixture.canonical_record(), restored, "{kind:?} {source:?}");
                assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} {source:?}");
            } else {
                assert_eq!(pending(&third).phase(), expected.phase, "{kind:?} {source:?}");
                assert_eq!(fixture.canonical_record(), expected, "{kind:?} {source:?}");
                assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?} {source:?}");
                assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?} {source:?}");
                assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} {source:?}");
            }
        }
    }
}
