use crate::transition_identity::staging_wrapper_rotation::RetainedActiveReblitReservationEvidenceFailure;

fn assert_active_reblit_reservation_evidence(
    source: &StatefulTransitionCoordinatorError,
) {
    assert!(matches!(
        source,
        StatefulTransitionCoordinatorError::ActiveReblitReservation(
            RetainedActiveReblitReservationEvidenceFailure::Replacement(_)
        )
    ));
}

fn assert_active_reblit_slot_evidence(source: &StatefulTransitionCoordinatorError) {
    assert!(matches!(
        source,
        StatefulTransitionCoordinatorError::ActiveReblitReservation(
            RetainedActiveReblitReservationEvidenceFailure::PreviousSlot(_)
        )
    ));
}

#[test]
fn journal_coordinator_active_reblit_tamper_before_started_runs_no_effect() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let record = coordinator.record().clone();
    let replacement = active_reblit_replacement_path(&fixture, &record, 0);
    let displaced = replacement.with_extension("retained-displaced");
    fs::rename(&replacement, &displaced).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(directory_identity(&replacement), directory_identity(&displaced));
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    let StatefulTransactionTriggerFailure::Preflight { source, .. } = &failure else {
        panic!("pre-Started reservation tamper crossed the intent boundary: {failure:#?}")
    };
    assert_active_reblit_reservation_evidence(source);
    assert_eq!(calls.get(), 0);
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_parked_slot_substitution_before_started_runs_no_effect() {
    let (fixture, prepared) = active_reblit_reservation_candidate(true);
    let record = prepared.record().clone();
    let coordinator = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap()
        .prepare_for_transaction_triggers(&fixture.installation)
        .unwrap();
    let parked = active_reblit_parked_slot_path(&fixture, &record, 0);
    let displaced = parked.with_extension("retained-displaced");
    fs::rename(&parked, &displaced).unwrap();
    fs::create_dir(&parked).unwrap();
    fs::set_permissions(&parked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(directory_identity(&parked), directory_identity(&displaced));
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    let StatefulTransactionTriggerFailure::Preflight { source, .. } = &failure else {
        panic!("parked-slot substitution crossed the trigger intent boundary: {failure:#?}")
    };
    assert_active_reblit_slot_evidence(source);
    assert_eq!(calls.get(), 0);
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_full_state_snapshot_change_before_started_runs_no_effect() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    fixture
        .database
        .change_summary_for_test(fixture.candidate_state, Some("mutated after reservation"))
        .unwrap();
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    let StatefulTransactionTriggerFailure::Preflight { source, .. } = &failure else {
        panic!("full-state snapshot change crossed the trigger intent boundary: {failure:#?}")
    };
    assert!(matches!(
        source,
        StatefulTransitionCoordinatorError::ActiveReblitReservation(
            RetainedActiveReblitReservationEvidenceFailure::Identity {
                source: crate::transition_identity::Error::ActiveReblitStateChanged { state },
                ..
            }
        ) if *state == i32::from(fixture.candidate_state)
    ));
    assert_eq!(calls.get(), 0);
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_tamper_after_started_stops_before_callback() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let record = coordinator.record().clone();
    let replacement = active_reblit_replacement_path(&fixture, &record, 0);
    transaction_triggers::arm_before_transaction_trigger_effect_evidence(move || {
        fs::remove_dir(replacement).unwrap();
    });
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    let StatefulTransactionTriggerFailure::PreEffectEvidence { source, .. } = &failure else {
        panic!("post-Started reservation tamper reached the callback: {failure:#?}")
    };
    assert_active_reblit_reservation_evidence(source);
    assert_eq!(calls.get(), 0);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::TransactionTriggersStarted,
        4,
    );
}

#[test]
fn journal_coordinator_active_reblit_tamper_during_effect_preserves_started() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let record = coordinator.record().clone();
    let replacement = active_reblit_replacement_path(&fixture, &record, 0);
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            fs::remove_dir(&replacement).unwrap();
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    let StatefulTransactionTriggerFailure::PostEffectEvidence { source, .. } = &failure else {
        panic!("effect-time reservation tamper crossed trigger completion: {failure:#?}")
    };
    assert_active_reblit_reservation_evidence(source);
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::TransactionTriggersStarted,
        4,
    );
}

#[test]
fn journal_coordinator_active_reblit_tamper_before_exchange_intent_preserves_trigger_complete() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let record = coordinator.record().clone();
    let complete = coordinator
        .run_transaction_triggers(|_| Ok::<(), TriggerEffectError>(()))
        .unwrap();
    fs::remove_dir(active_reblit_replacement_path(&fixture, &record, 0)).unwrap();

    let failure = complete.begin_usr_exchange_intent().unwrap_err();

    let UsrExchangeIntentFailure::Preflight { source, .. } = &failure else {
        panic!("reservation tamper crossed /usr exchange intent: {failure:#?}")
    };
    assert_active_reblit_reservation_evidence(source);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::TransactionTriggersComplete,
        5,
    );
}

#[test]
fn journal_coordinator_active_reblit_reservation_survives_intent_and_exchange_direction_flip() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
    let candidate = directory_identity(&fixture.candidate_path);
    let previous = directory_identity(&fixture.installation.root.join("usr"));
    let (fixture, intent, authority) =
        coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
    let record = intent.record().clone();
    let replacement = active_reblit_replacement_path(&fixture, &record, 0);
    assert_empty_private_reservation(&replacement);
    assert_exchange_layout(&fixture, false, candidate, previous);
    reset_retained_exchange_syscall_count();

    let exchanged = intent.execute_usr_exchange(authority).unwrap();

    assert_eq!(exchanged.record().phase, Phase::UsrExchanged);
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_exchange_layout(&fixture, true, candidate, previous);
    assert_empty_private_reservation(&replacement);
    exchanged.revalidate_retained_authorities().unwrap();
}
