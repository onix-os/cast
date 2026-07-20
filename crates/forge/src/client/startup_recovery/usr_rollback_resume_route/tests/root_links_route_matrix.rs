use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackResumeRouteSeal,
        startup_reconciliation::{
            RecoveryBlocker, UsrRollbackResumeRouteAdmission, UsrRollbackResumeRouteAuthority,
        },
    },
    transition_journal::{
        InitialRollbackAction, Phase, RecoveryDisposition, RollbackObservations, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::{
    fixture::{Fixture, OperationKind, SourceCase, pending},
    support::RouteFixture,
};

#[test]
#[should_panic(expected = "a pre-exchange source requiring a pending reverse exchange cannot form a route fixture")]
fn root_links_complete_pre_layout_cannot_construct_a_route_fixture() {
    let _fixture = RouteFixture::new(OperationKind::Archived, SourceCase::RootLinksCompletePre);
}

#[test]
fn startup_root_links_complete_post_routes_exactly_across_operations_and_epochs() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                RouteFixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                RouteFixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let epoch = fixture.source.creation_epoch.clone();
            let namespace_before = fixture.fixture.namespace_snapshot();
            let database_before = fixture.fixture.database_snapshot();

            let error = fixture.enter();
            let pending = pending(&error);

            assert_eq!(pending.phase(), Phase::ReverseExchangeIntent, "{kind:?} historical={historical}");
            assert_eq!(
                pending.disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::ReverseExchangeIntent,
                },
                "{kind:?} historical={historical}"
            );
            assert!(
                pending.blockers().is_empty(),
                "{kind:?} historical={historical}: {:?}",
                pending.blockers()
            );
            let actual = fixture.canonical_record();
            fixture.assert_exact_route(&actual);
            assert_eq!(actual.creation_epoch, epoch, "{kind:?} historical={historical}");
            assert_eq!(
                fixture.fixture.namespace_snapshot(),
                namespace_before,
                "{kind:?} historical={historical}"
            );
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} historical={historical}"
            );
        }
    }
}

#[test]
fn startup_root_links_complete_pre_layout_defers_pending_reverse_plan_across_operations_and_epochs() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                Fixture::historical(kind, SourceCase::RootLinksCompletePre)
            } else {
                Fixture::new(kind, SourceCase::RootLinksCompletePre)
            };
            let decision = persist_root_links_decision(&fixture, InitialRollbackAction::Pending);
            assert_route_deferred(&fixture, &decision);
            let before = fixture.canonical_bytes();
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();

            let error = fixture.enter();
            let pending = pending(&error);

            assert_eq!(pending.phase(), Phase::RollbackDecided, "{kind:?} historical={historical}");
            assert!(
                pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict),
                "{kind:?} historical={historical}: {:?}",
                pending.blockers()
            );
            assert_eq!(fixture.canonical_bytes(), before, "{kind:?} historical={historical}");
            assert_eq!(fixture.canonical_record(), decision, "{kind:?} historical={historical}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?} historical={historical}");
            assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} historical={historical}");
        }
    }
}

#[test]
fn startup_root_links_complete_post_layout_defers_codec_valid_wrong_plans_across_operations_and_epochs() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let cases = [
                (
                    "usr-satisfied",
                    InitialRollbackAction::AlreadySatisfied,
                    InitialRollbackAction::Pending,
                    InitialRollbackAction::Pending,
                ),
                (
                    "candidate-satisfied",
                    InitialRollbackAction::Pending,
                    InitialRollbackAction::AlreadySatisfied,
                    InitialRollbackAction::Pending,
                ),
                (
                    "fresh-db-satisfied",
                    InitialRollbackAction::Pending,
                    InitialRollbackAction::Pending,
                    InitialRollbackAction::AlreadySatisfied,
                ),
            ];
            for (name, usr_exchange, candidate, fresh_db) in cases {
                if name == "fresh-db-satisfied" && kind != OperationKind::NewState {
                    continue;
                }
                let fixture = if historical {
                    Fixture::historical(kind, SourceCase::RootLinksCompletePost)
                } else {
                    Fixture::new(kind, SourceCase::RootLinksCompletePost)
                };
                let decision = persist_root_links_decision_with_actions(
                    &fixture,
                    usr_exchange,
                    candidate,
                    fresh_db,
                );
                assert_route_deferred(&fixture, &decision);
                let before = fixture.canonical_bytes();
                let namespace_before = fixture.namespace_snapshot();
                let database_before = fixture.database_snapshot();

                let error = fixture.enter();
                let pending = pending(&error);

                assert_eq!(
                    pending.phase(),
                    Phase::RollbackDecided,
                    "{kind:?} historical={historical} {name}"
                );
                assert_eq!(
                    fixture.canonical_bytes(),
                    before,
                    "{kind:?} historical={historical} {name}"
                );
                assert_eq!(
                    fixture.canonical_record(),
                    decision,
                    "{kind:?} historical={historical} {name}"
                );
                assert_eq!(
                    fixture.namespace_snapshot(),
                    namespace_before,
                    "{kind:?} historical={historical} {name}"
                );
                assert_eq!(
                    fixture.database_snapshot(),
                    database_before,
                    "{kind:?} historical={historical} {name}"
                );
            }
        }
    }
}

fn persist_root_links_decision(
    fixture: &Fixture,
    usr_exchange: InitialRollbackAction,
) -> TransitionRecord {
    persist_root_links_decision_with_actions(
        fixture,
        usr_exchange,
        InitialRollbackAction::Pending,
        InitialRollbackAction::Pending,
    )
}

fn persist_root_links_decision_with_actions(
    fixture: &Fixture,
    usr_exchange: InitialRollbackAction,
    candidate: InitialRollbackAction,
    fresh_db: InitialRollbackAction,
) -> TransitionRecord {
    let decision = fixture
        .source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: None,
            usr_exchange: Some(usr_exchange),
            candidate,
            fresh_db: (fixture.kind == OperationKind::NewState).then_some(fresh_db),
        })
        .unwrap();
    let journal = TransitionJournalStore::open_retained(
        fixture.installation.root_directory(),
        &fixture.installation.root,
    )
    .unwrap();
    journal.advance(&fixture.source, &decision).unwrap();
    drop(journal);
    assert_eq!(fixture.canonical_record(), decision);
    decision
}

fn assert_route_deferred(fixture: &Fixture, decision: &TransitionRecord) {
    let journal = TransitionJournalStore::open_retained(
        fixture.installation.root_directory(),
        &fixture.installation.root,
    )
    .unwrap();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackResumeRouteSeal::new_for_test();
    let in_flight = fixture.database.audit_in_flight_transition().unwrap();
    let admission = UsrRollbackResumeRouteAuthority::capture(
        &seal,
        &fixture.installation,
        &journal,
        &fixture.database,
        &reservation,
        decision,
        in_flight,
    )
    .unwrap();
    assert!(matches!(admission, UsrRollbackResumeRouteAdmission::Deferred));
}
