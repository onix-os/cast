//! Shared ActiveReblit durability after an applied or admitted wrapper exchange.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
            UsrRollbackCandidatePreserveFinishDurabilitySelection,
        },
        startup_recovery::{UsrRollbackCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal},
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveApplyReconciliation,
    UsrRollbackActiveReblitCandidatePreserveDurabilitySeal,
    UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority,
    arm_before_active_reblit_candidate_preserve_durable_trailing_evidence,
};
use super::{
    fixture::OperationKind,
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, active_reblit_wrapper_path},
};
use crate::client::startup_reconciliation::activation_namespace::{
    ActiveReblitCandidatePreserveExchangeFault, ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint,
    active_reblit_candidate_preserve_exchange_attempt_count, arm_active_reblit_candidate_preserve_exchange_fault,
    arm_active_reblit_candidate_preserve_post_exchange_durability_fault,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync,
    reset_active_reblit_candidate_preserve_exchange_attempt_count,
    reset_active_reblit_candidate_preserve_post_exchange_durability_events,
    take_active_reblit_candidate_preserve_post_exchange_durability_events,
};

const WRAPPER_INDEX: usize = 13;

fn staged(source: CandidateSource, outcome: RollbackActionOutcome) -> CandidatePreserveFixture {
    CandidatePreserveFixture::new(OperationKind::ActiveReblit, source, outcome, CandidateLayout::Staged)
        .with_active_reblit_wrapper_index(WRAPPER_INDEX)
}

fn preserved(source: CandidateSource, outcome: RollbackActionOutcome) -> CandidatePreserveFixture {
    CandidatePreserveFixture::new(OperationKind::ActiveReblit, source, outcome, CandidateLayout::Preserved)
        .with_active_reblit_wrapper_index(WRAPPER_INDEX)
}

fn reconcile_applied<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    fault: Option<ActiveReblitCandidatePreserveExchangeFault>,
) -> UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact staged ActiveReblit evidence did not admit Apply");
    };
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let lease = authority
        .into_active_reblit_effect_for_test(&effect_seal, journal)
        .unwrap();
    if let Some(fault) = fault {
        arm_active_reblit_candidate_preserve_exchange_fault(fault);
    }
    let UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(authority) =
        lease.reconcile(&effect_seal, journal).unwrap()
    else {
        panic!("ActiveReblit exchange did not reconcile as Applied");
    };
    authority
}

fn select_finish<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
        panic!("exact preserved ActiveReblit evidence did not admit Finish");
    };
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    authority
        .reconcile_active_reblit_finish_for_test(&effect_seal, journal)
        .unwrap()
}

fn expected_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<ActiveReblitCandidatePreservePostExchangeDurabilityEvent> {
    let target = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, WRAPPER_INDEX);
    let candidate = identity(&target.join("usr"));
    let candidate_wrapper = identity(&target);
    let reservation_wrapper = identity(&fixture.fixture.installation.staging_dir());
    let roots = identity(&fixture.fixture.installation.root.join(".cast/root"));
    let quarantine = identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateWrapperSynced {
            device: candidate_wrapper.0,
            inode: candidate_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::ReservationWrapperSynced {
            device: reservation_wrapper.0,
            inode: reservation_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::QuarantineParentSynced {
            device: quarantine.0,
            inode: quarantine.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::FinalPostProven,
    ]
}

fn complete_applied<'reservation>(
    authority: UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation>,
    journal: &TransitionJournalStore,
) -> UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation> {
    let seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();
    authority.complete_post_exchange_durability(&seal, journal).unwrap()
}

fn complete_finish<'reservation>(
    authority: UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>,
    journal: &TransitionJournalStore,
) -> UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation> {
    let seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();
    authority.complete_post_exchange_durability(&seal, journal).unwrap()
}

fn reset_observations() {
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BarrierKind {
    Candidate,
    CandidateWrapper,
    ReservationWrapper,
    Roots,
    Quarantine,
    FinalPost,
}

fn barrier_order(events: &[ActiveReblitCandidatePreservePostExchangeDurabilityEvent]) -> Vec<BarrierKind> {
    events
        .iter()
        .map(|event| match event {
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateSynced { .. } => BarrierKind::Candidate,
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateWrapperSynced { .. } => {
                BarrierKind::CandidateWrapper
            }
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::ReservationWrapperSynced { .. } => {
                BarrierKind::ReservationWrapper
            }
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::RootsParentSynced { .. } => BarrierKind::Roots,
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::QuarantineParentSynced { .. } => {
                BarrierKind::Quarantine
            }
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent::FinalPostProven => BarrierKind::FinalPost,
        })
        .collect()
}

#[test]
fn startup_active_reblit_post_exchange_durability_orders_identical_events_for_applied_and_finish() {
    let exact_order = vec![
        BarrierKind::Candidate,
        BarrierKind::CandidateWrapper,
        BarrierKind::ReservationWrapper,
        BarrierKind::Roots,
        BarrierKind::Quarantine,
        BarrierKind::FinalPost,
    ];
    let applied_fixture = staged(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let applied_journal = applied_fixture.open_journal();
    let applied_reservation = ActiveStateReservation::acquire().unwrap();
    reset_observations();
    let applied = reconcile_applied(&applied_fixture, &applied_journal, &applied_reservation, None);
    let applied_expected = expected_events(&applied_fixture);
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();
    let durable = complete_applied(applied, &applied_journal);
    let applied_events = take_active_reblit_candidate_preserve_post_exchange_durability_events();
    assert_eq!(applied_events, applied_expected);
    assert_eq!(barrier_order(&applied_events), exact_order);
    assert_eq!(durable.origin_for_test(), RollbackActionOutcome::Applied);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    applied_fixture.assert_non_namespace_unchanged();
    drop(durable);
    drop(applied_reservation);

    let finish_fixture = preserved(CandidateSource::Exchanged, RollbackActionOutcome::AlreadySatisfied);
    let finish_journal = finish_fixture.open_journal();
    let finish_reservation = ActiveStateReservation::acquire().unwrap();
    reset_observations();
    let finish = select_finish(&finish_fixture, &finish_journal, &finish_reservation);
    let finish_expected = expected_events(&finish_fixture);
    let durable = complete_finish(finish, &finish_journal);
    let finish_events = take_active_reblit_candidate_preserve_post_exchange_durability_events();
    assert_eq!(finish_events, finish_expected);
    assert_eq!(barrier_order(&finish_events), exact_order);
    assert_eq!(durable.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    finish_fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_post_exchange_durability_faults_consume_exact_prefixes_and_fresh_finish_reruns_without_exchange()
 {
    let faults = [
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateSync,
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateWrapperSync,
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::ReservationWrapperSync,
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::RootsParentSync,
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::QuarantineParentSync,
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::FinalPostCapture,
    ];
    for (prefix_len, fault) in faults.into_iter().enumerate() {
        let fixture = staged(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_observations();
        let applied = reconcile_applied(&fixture, &journal, &reservation, None);
        let expected = expected_events(&fixture);
        reset_active_reblit_candidate_preserve_post_exchange_durability_events();
        arm_active_reblit_candidate_preserve_post_exchange_durability_fault(fault);
        let seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();
        assert!(applied.complete_post_exchange_durability(&seal, &journal).is_err());
        assert_eq!(
            take_active_reblit_candidate_preserve_post_exchange_durability_events(),
            expected[..prefix_len]
        );
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

        let finish = select_finish(&fixture, &journal, &reservation);
        reset_active_reblit_candidate_preserve_post_exchange_durability_events();
        let durable = complete_finish(finish, &journal);
        assert_eq!(
            take_active_reblit_candidate_preserve_post_exchange_durability_events(),
            expected
        );
        assert_eq!(durable.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_active_reblit_post_exchange_durability_rejects_namespace_public_name_inode_and_mode_races_at_every_boundary()
{
    for boundary in 0..6 {
        let fixture = staged(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_observations();
        let applied = reconcile_applied(&fixture, &journal, &reservation, None);
        let expected = expected_events(&fixture);
        reset_active_reblit_candidate_preserve_post_exchange_durability_events();
        arm_boundary_race(&fixture, boundary);
        let seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();

        assert!(applied.complete_post_exchange_durability(&seal, &journal).is_err());
        assert_eq!(
            take_active_reblit_candidate_preserve_post_exchange_durability_events(),
            expected[..boundary],
            "boundary {boundary}"
        );
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_active_reblit_post_exchange_durability_rejects_database_and_journal_drift_with_authority_withheld() {
    for race in [
        EvidenceRace::DatabaseBefore,
        EvidenceRace::DatabaseDuring,
        EvidenceRace::JournalDuring,
    ] {
        let fixture = preserved(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let before = fixture.evidence_snapshots();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_observations();
        let finish = select_finish(&fixture, &journal, &reservation);
        let expected = expected_events(&fixture);

        match race {
            EvidenceRace::DatabaseBefore => fixture
                .fixture
                .database
                .delete_metadata_provenance_for_test(fixture.fixture.candidate_state)
                .unwrap(),
            EvidenceRace::DatabaseDuring => {
                let database = fixture.fixture.database.clone();
                let candidate = fixture.fixture.candidate_state;
                arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
            }
            EvidenceRace::JournalDuring => {
                arm_before_active_reblit_candidate_preserve_durable_trailing_evidence(fixture.journal_change_hook());
            }
        }
        let seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();
        assert!(finish.complete_post_exchange_durability(&seal, &journal).is_err());
        let events = take_active_reblit_candidate_preserve_post_exchange_durability_events();
        match race {
            EvidenceRace::DatabaseBefore => assert!(events.is_empty()),
            EvidenceRace::DatabaseDuring | EvidenceRace::JournalDuring => assert_eq!(events, expected),
        }
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
        assert_eq!(fixture.fixture.namespace_snapshot(), before.2);
    }
}

#[test]
fn startup_active_reblit_post_exchange_durability_converges_success_error_after_apply_and_finish_independent_of_raw_status()
 {
    for fault in [None, Some(ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply)] {
        let fixture = staged(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_observations();
        let applied = reconcile_applied(&fixture, &journal, &reservation, fault);
        let expected = expected_events(&fixture);
        reset_active_reblit_candidate_preserve_post_exchange_durability_events();
        let durable = complete_applied(applied, &journal);
        assert_eq!(durable.origin_for_test(), RollbackActionOutcome::Applied);
        assert_eq!(
            take_active_reblit_candidate_preserve_post_exchange_durability_events(),
            expected
        );
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
        fixture.assert_non_namespace_unchanged();
    }

    let fixture = preserved(CandidateSource::Exchanged, RollbackActionOutcome::AlreadySatisfied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_observations();
    let finish = select_finish(&fixture, &journal, &reservation);
    let expected = expected_events(&fixture);
    let durable = complete_finish(finish, &journal);
    assert_eq!(durable.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
    assert_eq!(
        take_active_reblit_candidate_preserve_post_exchange_durability_events(),
        expected
    );
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_active_reblit_post_exchange_durability_production_selection_remains_unsupported_without_events_or_attempts()
{
    let staged_fixture = staged(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let staged_before = staged_fixture.evidence_snapshots();
    let staged_journal = staged_fixture.open_journal();
    let staged_reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) =
        staged_fixture.capture(&staged_journal, &staged_reservation)
    else {
        panic!("exact staged ActiveReblit evidence did not admit Apply");
    };
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    reset_observations();
    assert!(matches!(
        authority.into_effect_selection(&effect_seal, &staged_journal).unwrap(),
        UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty());
    staged_fixture.assert_evidence_unchanged(&staged_before);
    drop(staged_reservation);

    let finish_fixture = preserved(CandidateSource::Exchanged, RollbackActionOutcome::AlreadySatisfied);
    let finish_before = finish_fixture.evidence_snapshots();
    let finish_journal = finish_fixture.open_journal();
    let finish_reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) =
        finish_fixture.capture(&finish_journal, &finish_reservation)
    else {
        panic!("exact preserved ActiveReblit evidence did not admit Finish");
    };
    let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
    reset_observations();
    assert!(matches!(
        authority
            .into_post_move_durability_selection(&durability_seal, &finish_journal)
            .unwrap(),
        UsrRollbackCandidatePreserveFinishDurabilitySelection::Unsupported
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty());
    finish_fixture.assert_evidence_unchanged(&finish_before);
}

#[derive(Clone, Copy)]
enum EvidenceRace {
    DatabaseBefore,
    DatabaseDuring,
    JournalDuring,
}

fn arm_boundary_race(fixture: &CandidatePreserveFixture, boundary: usize) {
    let target = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, WRAPPER_INDEX);
    let staging = fixture.fixture.installation.staging_dir();
    let root = fixture.fixture.installation.root.clone();
    let quarantine = fixture.fixture.installation.state_quarantine_dir();
    match boundary {
        0 => arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync(move || {
            fs::write(target.join("usr/post-exchange-race"), b"changed").unwrap();
        }),
        1 => arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync(move || {
            let displaced = quarantine.join("active-reblit-target-inode-race");
            fs::rename(&target, displaced).unwrap();
            create_private_directory(&target);
        }),
        2 => arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync(move || {
            let displaced = quarantine.join("active-reblit-staging-inode-race");
            fs::rename(&staging, displaced).unwrap();
            create_private_directory(&staging);
        }),
        3 => arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync(move || {
            let roots = root.join(".cast/root");
            let displaced = root.join(".cast/root-inode-race");
            fs::rename(&roots, displaced).unwrap();
            create_private_directory(&roots);
        }),
        4 => arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync(move || {
            let displaced = root.join(".cast/quarantine-inode-race");
            fs::rename(&quarantine, displaced).unwrap();
            create_private_directory(&quarantine);
        }),
        5 => arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture(move || {
            fs::set_permissions(target, fs::Permissions::from_mode(0o750)).unwrap();
        }),
        _ => panic!("unknown ActiveReblit durability boundary {boundary}"),
    }
}

fn create_private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
