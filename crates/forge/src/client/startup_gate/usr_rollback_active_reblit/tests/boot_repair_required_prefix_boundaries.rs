//! Exact admission boundaries for every widened ActiveReblit boot prefix.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{
            UsrRollbackCandidatePreserveSeal, UsrRollbackDecisionSeal, UsrRollbackResumeRouteSeal,
            UsrRollbackReverseSeal,
        },
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority, UsrRollbackDecisionAdmission,
            UsrRollbackDecisionAuthority, UsrRollbackResumeRouteAdmission, UsrRollbackResumeRouteAuthority,
            UsrRollbackReverseAdmission, UsrRollbackReverseAuthority,
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            usr_rollback_candidate_preserve_plan_is_exact_for_test, usr_rollback_decision_source_is_supported_for_test,
            usr_rollback_resume_route_plan_is_exact_for_test, usr_rollback_reverse_plan_is_exact_for_test,
        },
    },
    transition_journal::{
        BootRollback, ForwardPhase, InitialRollbackAction, Operation, Phase, RollbackAction, RollbackActionOutcome,
        RollbackObservations, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    super::test_fixture::{BootSyncStartedLayout, Fixture, OperationKind, exchange_usr_layout},
    support::{
        BootRepairFixture, Epoch, assert_pending_phase, build_boot_sync_started, enter_boot,
        synthesize_boot_candidate_preserved_topology,
    },
};

#[test]
fn startup_active_reblit_boot_repair_required_prefix_boundaries_are_exact_and_effect_free() {
    decision_post_pre_and_sibling_boundaries();
    exact_active_reblit_prefix_admissions();
    sibling_and_legacy_plan_predicates_are_rejected();
}

fn decision_post_pre_and_sibling_boundaries() {
    let post_source = {
        let post = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
        let post_journal = open_journal(&post.fixture);
        let post_reservation = ActiveStateReservation::acquire().unwrap();
        let post_seal = UsrRollbackDecisionSeal::new_for_test();
        let post_in_flight = post.fixture.database.audit_in_flight_transition().unwrap();
        let post_admission = UsrRollbackDecisionAuthority::capture(
            &post_seal,
            &post.fixture.installation,
            &post_journal,
            &post.fixture.database,
            &post_reservation,
            &post.fixture.source,
            post_in_flight,
        )
        .unwrap();
        assert!(matches!(post_admission, UsrRollbackDecisionAdmission::Ready(_)));
        post.fixture.source.clone()
    };

    {
        let pre = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Pre);
        let pre_journal = open_journal(&pre.fixture);
        let pre_reservation = ActiveStateReservation::acquire().unwrap();
        let pre_seal = UsrRollbackDecisionSeal::new_for_test();
        let pre_in_flight = pre.fixture.database.audit_in_flight_transition().unwrap();
        let pre_admission = UsrRollbackDecisionAuthority::capture(
            &pre_seal,
            &pre.fixture.installation,
            &pre_journal,
            &pre.fixture.database,
            &pre_reservation,
            &pre.fixture.source,
            pre_in_flight,
        )
        .unwrap();
        assert!(matches!(pre_admission, UsrRollbackDecisionAdmission::Deferred(_)));
    }

    for kind in [OperationKind::NewState, OperationKind::Archived] {
        let sibling = Fixture::boot_sync_started(kind, BootSyncStartedLayout::Post, false);
        assert!(!usr_rollback_decision_source_is_supported_for_test(&sibling.source));
    }
    let mut wrong_phase = post_source;
    wrong_phase.phase = Phase::BootSyncComplete;
    assert!(!usr_rollback_decision_source_is_supported_for_test(&wrong_phase));
}

fn exact_active_reblit_prefix_admissions() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);

    let decision_error = enter_boot(&fixture);
    assert_pending_phase(&decision_error, Phase::RollbackDecided);
    let decision = fixture.fixture.canonical_record();
    assert!(usr_rollback_resume_route_plan_is_exact_for_test(&decision));
    assert_resume_ready(&fixture, &decision);

    let route_error = enter_boot(&fixture);
    assert_pending_phase(&route_error, Phase::ReverseExchangeIntent);
    let reverse = fixture.fixture.canonical_record();
    assert!(usr_rollback_reverse_plan_is_exact_for_test(&reverse));
    let namespace_before = fixture.fixture.namespace_snapshot();
    assert_reverse_admission(&fixture, &reverse, true);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);

    exchange_usr_layout(&fixture.fixture.installation.root);
    let namespace_before = fixture.fixture.namespace_snapshot();
    assert_reverse_admission(&fixture, &reverse, false);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);

    let reverse_error = enter_boot(&fixture);
    assert_pending_phase(&reverse_error, Phase::UsrRestored);
    let restored = fixture.fixture.canonical_record();
    assert!(usr_rollback_resume_route_plan_is_exact_for_test(&restored));
    assert_resume_ready(&fixture, &restored);

    let candidate_route_error = enter_boot(&fixture);
    assert_pending_phase(&candidate_route_error, Phase::CandidatePreserveIntent);
    let candidate_intent = fixture.fixture.canonical_record();
    assert!(usr_rollback_candidate_preserve_plan_is_exact_for_test(
        &candidate_intent
    ));
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let namespace_before = fixture.fixture.namespace_snapshot();
    assert_candidate_admission(&fixture, &candidate_intent, false);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);

    synthesize_boot_candidate_preserved_topology(&fixture);
    let namespace_before = fixture.fixture.namespace_snapshot();
    assert_candidate_admission(&fixture, &candidate_intent, true);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}

fn sibling_and_legacy_plan_predicates_are_rejected() {
    for kind in [OperationKind::NewState, OperationKind::Archived] {
        let fixture = Fixture::boot_sync_started(kind, BootSyncStartedLayout::Post, false);
        let prefixes = sibling_prefixes(&fixture.source, kind);
        assert!(!usr_rollback_resume_route_plan_is_exact_for_test(&prefixes.decision));
        assert!(!usr_rollback_reverse_plan_is_exact_for_test(&prefixes.reverse));
        assert!(!usr_rollback_resume_route_plan_is_exact_for_test(&prefixes.restored));
        assert!(!usr_rollback_candidate_preserve_plan_is_exact_for_test(
            &prefixes.candidate_intent
        ));
    }

    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    let decision_error = enter_boot(&fixture);
    assert_pending_phase(&decision_error, Phase::RollbackDecided);
    let decision = fixture.fixture.canonical_record();
    let route_error = enter_boot(&fixture);
    assert_pending_phase(&route_error, Phase::ReverseExchangeIntent);
    let reverse = fixture.fixture.canonical_record();
    exchange_usr_layout(&fixture.fixture.installation.root);
    let reverse_error = enter_boot(&fixture);
    assert_pending_phase(&reverse_error, Phase::UsrRestored);
    let restored = fixture.fixture.canonical_record();
    let candidate_route_error = enter_boot(&fixture);
    assert_pending_phase(&candidate_route_error, Phase::CandidatePreserveIntent);
    let candidate = fixture.fixture.canonical_record();

    for operation in [Operation::NewState, Operation::ActivateArchived] {
        for record in [&decision, &reverse, &restored, &candidate] {
            let mut wrong_operation = record.clone();
            wrong_operation.operation = operation;
            assert_prefix_plan_refused(&wrong_operation);
        }
    }

    for source in [ForwardPhase::UsrExchangeIntent, ForwardPhase::UsrExchanged] {
        for record in [&decision, &reverse, &restored, &candidate] {
            let mut legacy_pending_boot = record.clone();
            let rollback = legacy_pending_boot.rollback.as_mut().unwrap();
            rollback.source = source;
            rollback.boot = BootRollback::PendingUnverifiable;
            assert_prefix_plan_refused(&legacy_pending_boot);
        }
    }

    for boot in [BootRollback::NotRequired, BootRollback::Unverified] {
        for record in [&decision, &reverse, &restored, &candidate] {
            let mut wrong_boot = record.clone();
            wrong_boot.rollback.as_mut().unwrap().boot = boot;
            assert_prefix_plan_refused(&wrong_boot);
        }
    }

    for record in [&decision, &reverse, &restored, &candidate] {
        let mut wrong_action = record.clone();
        wrong_action.rollback.as_mut().unwrap().candidate.action = RollbackAction::NotRequired;
        assert_prefix_plan_refused(&wrong_action);

        let mut wrong_phase = record.clone();
        wrong_phase.phase = Phase::BootRepairRequired;
        assert_prefix_plan_refused(&wrong_phase);
    }
}

fn assert_resume_ready(fixture: &BootRepairFixture, record: &TransitionRecord) {
    let journal = open_journal(&fixture.fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackResumeRouteSeal::new_for_test();
    let in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    let admission = UsrRollbackResumeRouteAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
        in_flight,
    )
    .unwrap();
    assert!(matches!(admission, UsrRollbackResumeRouteAdmission::Ready(_)));
}

fn assert_reverse_admission(fixture: &BootRepairFixture, record: &TransitionRecord, apply: bool) {
    let journal = open_journal(&fixture.fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackReverseSeal::new_for_test();
    let in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    let admission = UsrRollbackReverseAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
        in_flight,
    )
    .unwrap();
    assert!(matches!(
        (apply, admission),
        (true, UsrRollbackReverseAdmission::Apply(_)) | (false, UsrRollbackReverseAdmission::Finish(_))
    ));
}

fn assert_candidate_admission(fixture: &BootRepairFixture, record: &TransitionRecord, finish: bool) {
    let journal = open_journal(&fixture.fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackCandidatePreserveSeal::new_for_test();
    let in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    let admission = UsrRollbackCandidatePreserveAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        record,
        in_flight,
    )
    .unwrap();
    assert!(matches!(
        (finish, admission),
        (false, UsrRollbackCandidatePreserveAdmission::Apply(_))
            | (true, UsrRollbackCandidatePreserveAdmission::Finish(_))
    ));
}

struct SiblingPrefixes {
    decision: TransitionRecord,
    reverse: TransitionRecord,
    restored: TransitionRecord,
    candidate_intent: TransitionRecord,
}

fn sibling_prefixes(source: &TransitionRecord, kind: OperationKind) -> SiblingPrefixes {
    let decision = source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: Some(InitialRollbackAction::Pending),
            usr_exchange: Some(InitialRollbackAction::Pending),
            candidate: InitialRollbackAction::Pending,
            fresh_db: (kind == OperationKind::NewState).then_some(InitialRollbackAction::Pending),
        })
        .unwrap();
    let previous_intent = decision.rollback_successor(None).unwrap();
    assert_eq!(previous_intent.phase, Phase::PreviousRestoreIntent);
    let previous_restored = previous_intent
        .rollback_successor(Some(RollbackActionOutcome::Applied))
        .unwrap();
    let reverse = previous_restored.rollback_successor(None).unwrap();
    assert_eq!(reverse.phase, Phase::ReverseExchangeIntent);
    let restored = reverse
        .rollback_successor(Some(RollbackActionOutcome::Applied))
        .unwrap();
    let candidate_intent = restored.rollback_successor(None).unwrap();
    SiblingPrefixes {
        decision,
        reverse,
        restored,
        candidate_intent,
    }
}

fn assert_prefix_plan_refused(record: &TransitionRecord) {
    match record.phase {
        Phase::RollbackDecided | Phase::UsrRestored => {
            assert!(!usr_rollback_resume_route_plan_is_exact_for_test(record));
        }
        Phase::ReverseExchangeIntent => assert!(!usr_rollback_reverse_plan_is_exact_for_test(record)),
        Phase::CandidatePreserveIntent => {
            assert!(!usr_rollback_candidate_preserve_plan_is_exact_for_test(record));
        }
        _ => {
            assert!(!usr_rollback_resume_route_plan_is_exact_for_test(record));
            assert!(!usr_rollback_reverse_plan_is_exact_for_test(record));
            assert!(!usr_rollback_candidate_preserve_plan_is_exact_for_test(record));
        }
    }
}

fn open_journal(fixture: &Fixture) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root).unwrap()
}
