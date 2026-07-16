use crate::transition_journal::{Phase, RecoveryDisposition};

use super::{
    fixture::{OperationKind, SourceCase, pending},
    support::RouteFixture,
};

#[test]
fn startup_usr_rollback_resume_route_pending_matrix_persists_reverse_exchange_intent() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPost, SourceCase::ExchangedPost] {
            let fixture = RouteFixture::new(kind, source);

            let error = fixture.enter();
            let pending = pending(&error);

            assert_eq!(pending.phase(), Phase::ReverseExchangeIntent, "{kind:?} {source:?}");
            assert_eq!(
                pending.disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::ReverseExchangeIntent,
                },
                "{kind:?} {source:?}"
            );
            assert!(
                pending.blockers().is_empty(),
                "{kind:?} {source:?}: {:?}",
                pending.blockers()
            );
            fixture.assert_exact_route(&fixture.canonical_record());
        }
    }
}

#[test]
fn startup_usr_rollback_resume_route_satisfied_matrix_skips_reverse_exchange() {
    for kind in OperationKind::ALL {
        let fixture = RouteFixture::new(kind, SourceCase::IntentPre);

        let error = fixture.enter();
        let pending = pending(&error);

        assert_eq!(pending.phase(), Phase::CandidatePreserveIntent, "{kind:?}");
        assert_eq!(
            pending.disposition(),
            RecoveryDisposition::ResumeRollback {
                phase: Phase::CandidatePreserveIntent,
            },
            "{kind:?}"
        );
        assert!(pending.blockers().is_empty(), "{kind:?}: {:?}", pending.blockers());
        fixture.assert_exact_route(&fixture.canonical_record());
    }
}

#[test]
fn startup_usr_rollback_resume_route_routes_only_and_preserves_exact_plan() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPre, SourceCase::IntentPost, SourceCase::ExchangedPost] {
            let fixture = RouteFixture::new(kind, source);
            let namespace_before = fixture.fixture.namespace_snapshot();
            let database_before = fixture.fixture.database_snapshot();
            let decision_before = fixture.fixture.canonical_bytes();

            let error = fixture.enter();

            assert_eq!(pending(&error).phase(), fixture.expected_phase(), "{kind:?} {source:?}");
            assert_ne!(
                fixture.fixture.canonical_bytes(),
                decision_before,
                "{kind:?} {source:?}"
            );
            fixture.assert_exact_route(&fixture.canonical_record());
            assert_eq!(
                fixture.fixture.namespace_snapshot(),
                namespace_before,
                "{kind:?} {source:?}"
            );
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} {source:?}"
            );
        }
    }
}
