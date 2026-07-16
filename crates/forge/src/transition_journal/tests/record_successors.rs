#[test]
fn production_forward_successor_inserts_a_state_id_only_at_allocation_completion() {
    let preparing = creation_record();
    assert!(matches!(
        preparing.forward_successor(Some(42)),
        Err(CodecError::CandidateStateChangedIllegally)
    ));

    let allocating = preparing.forward_successor(None).unwrap();
    assert_eq!(allocating.phase, Phase::FreshStateAllocating);
    assert_eq!(allocating.candidate.id, None);
    assert!(matches!(
        allocating.forward_successor(None),
        Err(CodecError::CandidateStateLayout)
    ));

    let allocated = allocating.forward_successor(Some(42)).unwrap();
    assert_eq!(allocated.phase, Phase::FreshStateAllocated);
    assert_eq!(allocated.candidate.id, Some(42));
    assert_eq!(allocated.generation, preparing.generation + 2);

    let prepared_intent = allocated.forward_successor(None).unwrap();
    assert_eq!(prepared_intent.phase, Phase::CandidatePrepareStarted);
    assert_eq!(prepared_intent.candidate.id, Some(42));

    let mut archived = archived_record(Phase::Preparing);
    archived.generation = 1;
    let archived_next = archived.forward_successor(None).unwrap();
    assert_eq!(archived_next.phase, Phase::CandidatePrepareStarted);
    assert_eq!(archived_next.candidate.id, Some(42));
}

#[test]
fn production_rollback_decision_derives_requirements_from_exact_observations() {
    let source = new_state_record(Phase::PreviousArchiveIntent);
    let observations = RollbackObservations {
        allocated_candidate_id: None,
        previous_archive: Some(InitialRollbackAction::Pending),
        usr_exchange: Some(InitialRollbackAction::AlreadySatisfied),
        candidate: InitialRollbackAction::Pending,
        fresh_db: Some(InitialRollbackAction::Pending),
    };
    let decided = source.rollback_decision(observations).unwrap();
    let plan = decided.rollback.as_ref().unwrap();
    assert_eq!(decided.phase, Phase::RollbackDecided);
    assert_eq!(plan.source, ForwardPhase::PreviousArchiveIntent);
    assert_eq!(plan.previous_archive, RollbackAction::Pending);
    assert_eq!(plan.usr_exchange, RollbackAction::AlreadySatisfied);
    assert_eq!(plan.candidate.action, RollbackAction::Pending);
    assert_eq!(plan.fresh_db, RollbackAction::Pending);
    assert_eq!(plan.boot, BootRollback::NotRequired);
    validate_advance(&source, &decided).unwrap();

    let mut missing_required = observations;
    missing_required.previous_archive = None;
    assert!(matches!(
        source.rollback_decision(missing_required),
        Err(CodecError::InvalidRollbackRequirement {
            action: "previous-archive",
            possible: true,
            ..
        })
    ));

    let allocating = new_state_record(Phase::FreshStateAllocating);
    let absent = allocating
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: None,
            usr_exchange: None,
            candidate: InitialRollbackAction::AlreadySatisfied,
            fresh_db: Some(InitialRollbackAction::AlreadySatisfied),
        })
        .unwrap();
    assert_eq!(absent.candidate.id, None);
    assert_eq!(
        absent.rollback.as_ref().unwrap().fresh_db,
        RollbackAction::AlreadySatisfied
    );

    let observed = allocating
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: Some(42),
            previous_archive: None,
            usr_exchange: None,
            candidate: InitialRollbackAction::Pending,
            fresh_db: Some(InitialRollbackAction::Pending),
        })
        .unwrap();
    assert_eq!(observed.candidate.id, Some(42));
    assert_eq!(observed.rollback.as_ref().unwrap().fresh_db, RollbackAction::Pending);
}

#[test]
fn production_rollback_successor_requires_one_exact_action_outcome_and_persists_unverified_boot() {
    let source = new_state_record(Phase::PreviousArchiveIntent);
    let decided = source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: Some(InitialRollbackAction::Pending),
            usr_exchange: Some(InitialRollbackAction::AlreadySatisfied),
            candidate: InitialRollbackAction::Pending,
            fresh_db: Some(InitialRollbackAction::AlreadySatisfied),
        })
        .unwrap();
    assert!(matches!(
        decided.rollback_successor(Some(RollbackActionOutcome::Applied)),
        Err(CodecError::RollbackActionOutcomeMismatch)
    ));
    let restore_intent = decided.rollback_successor(None).unwrap();
    assert_eq!(restore_intent.phase, Phase::PreviousRestoreIntent);
    assert!(matches!(
        restore_intent.rollback_successor(None),
        Err(CodecError::RollbackActionOutcomeMismatch)
    ));
    let restored = restore_intent
        .rollback_successor(Some(RollbackActionOutcome::Applied))
        .unwrap();
    assert_eq!(restored.phase, Phase::PreviousRestoredToStaging);
    assert_eq!(
        restored.rollback.as_ref().unwrap().previous_archive,
        RollbackAction::Applied
    );
    let preserve_intent = restored.rollback_successor(None).unwrap();
    assert_eq!(preserve_intent.phase, Phase::CandidatePreserveIntent);
    let preserved = preserve_intent
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    assert_eq!(
        preserved.rollback.as_ref().unwrap().candidate.action,
        RollbackAction::AlreadySatisfied
    );
    assert_eq!(preserved.rollback_successor(None).unwrap().phase, Phase::RollbackComplete);

    let boot_source = new_state_record(Phase::BootSyncStarted);
    let boot_decided = boot_source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: Some(InitialRollbackAction::AlreadySatisfied),
            usr_exchange: Some(InitialRollbackAction::AlreadySatisfied),
            candidate: InitialRollbackAction::AlreadySatisfied,
            fresh_db: Some(InitialRollbackAction::AlreadySatisfied),
        })
        .unwrap();
    let required = boot_decided.rollback_successor(None).unwrap();
    let started = required.rollback_successor(None).unwrap();
    let unverified = started.rollback_successor(None).unwrap();
    assert_eq!(unverified.phase, Phase::BootRepairUnverified);
    assert_eq!(unverified.rollback.as_ref().unwrap().boot, BootRollback::Unverified);
    assert!(matches!(
        unverified.rollback_successor(None),
        Err(CodecError::TerminalPhaseAdvance)
    ));

    // The reverse-exchange outcome describes this reconciliation invocation,
    // not the historical origin of the exact PRE layout it completes. Both
    // resolved outcomes are legal, and only the completed /usr action may
    // differ between their exact successors.
    let reverse_intent = valid_rollback_record(Phase::ReverseExchangeIntent);
    assert_eq!(
        reverse_intent.rollback.as_ref().unwrap().usr_exchange,
        RollbackAction::Pending
    );
    for (outcome, resolved) in [
        (RollbackActionOutcome::Applied, RollbackAction::Applied),
        (
            RollbackActionOutcome::AlreadySatisfied,
            RollbackAction::AlreadySatisfied,
        ),
    ] {
        let restored = reverse_intent.rollback_successor(Some(outcome)).unwrap();
        let mut expected = reverse_intent.clone();
        expected.generation += 1;
        expected.phase = Phase::UsrRestored;
        expected.rollback.as_mut().unwrap().usr_exchange = resolved;

        assert_eq!(restored, expected, "outcome {outcome:?}");
        validate_advance(&reverse_intent, &restored).unwrap();
    }
}

#[test]
fn production_rollback_successor_executes_every_pending_effect_in_fixed_order() {
    let source = new_state_record(Phase::BootSyncStarted);
    let mut current = source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: Some(InitialRollbackAction::Pending),
            usr_exchange: Some(InitialRollbackAction::Pending),
            candidate: InitialRollbackAction::Pending,
            fresh_db: Some(InitialRollbackAction::Pending),
        })
        .unwrap();

    for (expected, outcome) in [
        (Phase::PreviousRestoreIntent, None),
        (
            Phase::PreviousRestoredToStaging,
            Some(RollbackActionOutcome::Applied),
        ),
        (Phase::ReverseExchangeIntent, None),
        (Phase::UsrRestored, Some(RollbackActionOutcome::Applied)),
        (Phase::CandidatePreserveIntent, None),
        (
            Phase::CandidatePreserved,
            Some(RollbackActionOutcome::Applied),
        ),
        (Phase::FreshDbInvalidationIntent, None),
        (
            Phase::FreshDbInvalidated,
            Some(RollbackActionOutcome::Applied),
        ),
        (Phase::BootRepairRequired, None),
        (Phase::BootRepairStarted, None),
        (Phase::BootRepairUnverified, None),
    ] {
        current = current.rollback_successor(outcome).unwrap();
        assert_eq!(current.phase, expected);
    }

    let plan = current.rollback.as_ref().unwrap();
    assert_eq!(plan.previous_archive, RollbackAction::Applied);
    assert_eq!(plan.usr_exchange, RollbackAction::Applied);
    assert_eq!(plan.candidate.action, RollbackAction::Applied);
    assert_eq!(plan.fresh_db, RollbackAction::Applied);
    assert_eq!(plan.boot, BootRollback::Unverified);
}
