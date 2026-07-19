#[test]
fn disabled_forward_phases_and_rollback_plan_placement_fail_closed() {
    let invalid = archived_record(Phase::TransactionTriggersStarted);
    assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

    let mut invalid = record(Phase::SystemTriggersStarted);
    invalid.options.run_system_triggers = false;
    assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

    let mut invalid = record(Phase::PreviousArchiveIntent);
    invalid.options.archive_previous = false;
    invalid.previous.origin = PreviousOrigin::SynthesizedEmpty;
    invalid.previous.id = None;
    assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

    let mut invalid = record(Phase::BootSyncStarted);
    invalid.options.run_boot_sync = false;
    assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

    let mut invalid = record(Phase::Preparing);
    invalid.rollback = Some(rollback_decided(&invalid).rollback.unwrap());
    assert!(matches!(encode(&invalid), Err(CodecError::RollbackPlanOnForwardPhase)));

    let mut invalid = record(Phase::RollbackComplete);
    invalid.rollback = None;
    assert!(matches!(encode(&invalid), Err(CodecError::MissingRollbackPlan)));

    let mut invalid = rollback_decided(&new_state_record(Phase::CandidatePrepared));
    invalid.rollback.as_mut().unwrap().source = ForwardPhase::CommitDecided;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::InvalidRollbackSource(ForwardPhase::CommitDecided))
    ));
}

#[test]
fn all_operations_and_forward_option_paths_have_exact_successors() {
    for run_system_triggers in [false, true] {
        for archive_previous in [false, true] {
            for run_boot_sync in [false, true] {
                let mut current = new_state_record(Phase::Preparing);
                if !archive_previous {
                    current = without_previous_archive(current, PreviousOrigin::SynthesizedEmpty);
                }
                current.generation = 1;
                current.options.run_system_triggers = run_system_triggers;
                current.options.run_boot_sync = run_boot_sync;
                while current.phase != Phase::Complete {
                    let next = legal_forward_advance(&current);
                    validate_advance(&current, &next).unwrap();
                    current = next;
                }
            }
        }
    }

    for mut current in [archived_record(Phase::Preparing), reblit_record(Phase::Preparing)] {
        current.generation = 1;
        for run_system_triggers in [false, true] {
            for run_boot_sync in [false, true] {
                let mut path = current.clone();
                path.options.run_system_triggers = run_system_triggers;
                path.options.run_boot_sync = run_boot_sync;
                let mut visited = Vec::new();
                while path.phase != Phase::Complete {
                    visited.push(path.phase);
                    let next = legal_forward_advance(&path);
                    validate_advance(&path, &next).unwrap();
                    path = next;
                }
                if matches!(current.operation, Operation::ActivateArchived) {
                    assert!(!visited.contains(&Phase::TransactionTriggersStarted));
                    assert!(!visited.contains(&Phase::TransactionTriggersComplete));
                }
            }
        }
    }
}

#[test]
fn rollback_is_available_until_commit_except_after_verified_boot_sync() {
    let mut current = new_state_record(Phase::Preparing);
    loop {
        let source = current.phase.forward().unwrap();
        let rollback = rollback_decided(&current);
        if rollback_allowed(&current, source) {
            validate_advance(&current, &rollback).unwrap();
            let sequence = rollback_sequence(&current);
            let terminal = sequence.last().unwrap().phase;
            let expected = if source == ForwardPhase::BootSyncStarted {
                Phase::BootRepairUnverified
            } else {
                Phase::RollbackComplete
            };
            assert_eq!(terminal, expected, "rollback from {source:?}");
        } else {
            assert!(matches!(
                validate_advance(&current, &rollback),
                Err(CodecError::InvalidRollbackSource(_)) | Err(CodecError::IllegalPhaseAdvance { .. })
            ));
        }
        if current.phase == Phase::Complete {
            break;
        }
        current = legal_forward_advance(&current);
    }

    let mut immediate_commit = without_previous_archive(
        new_state_record(Phase::RootLinksComplete),
        PreviousOrigin::SynthesizedEmpty,
    );
    immediate_commit.options.run_system_triggers = false;
    immediate_commit.options.run_boot_sync = false;
    assert_eq!(
        next_forward_phase(&immediate_commit, ForwardPhase::RootLinksComplete),
        Some(ForwardPhase::CommitDecided)
    );
    assert!(rollback_allowed(&immediate_commit, ForwardPhase::RootLinksComplete));
    validate_advance(&immediate_commit, &rollback_decided(&immediate_commit)).unwrap();

    let mut after_system = without_previous_archive(
        new_state_record(Phase::SystemTriggersComplete),
        PreviousOrigin::SynthesizedEmpty,
    );
    after_system.options.run_boot_sync = false;
    assert_eq!(
        next_forward_phase(&after_system, ForwardPhase::SystemTriggersComplete),
        Some(ForwardPhase::CommitDecided)
    );
    assert!(rollback_allowed(&after_system, ForwardPhase::SystemTriggersComplete));
    validate_advance(&after_system, &rollback_decided(&after_system)).unwrap();

    let mut after_archive = new_state_record(Phase::PreviousArchived);
    after_archive.options.run_boot_sync = false;
    assert_eq!(
        next_forward_phase(&after_archive, ForwardPhase::PreviousArchived),
        Some(ForwardPhase::CommitDecided)
    );
    assert!(rollback_allowed(&after_archive, ForwardPhase::PreviousArchived));
    validate_advance(&after_archive, &rollback_decided(&after_archive)).unwrap();
    assert!(!rollback_allowed(
        &new_state_record(Phase::BootSyncComplete),
        ForwardPhase::BootSyncComplete
    ));
}

#[test]
fn conditional_advance_rejects_generation_transition_phase_and_layout_changes() {
    let current = creation_record();
    let legal = advance_record(&current, Phase::FreshStateAllocating);
    validate_advance(&current, &legal).unwrap();

    let mut invalid = legal.clone();
    invalid.generation = current.generation;
    assert!(matches!(
        validate_advance(&current, &invalid),
        Err(CodecError::GenerationMismatch { .. })
    ));

    let mut invalid = legal.clone();
    invalid.transition_id = other_id();
    assert!(matches!(
        validate_advance(&current, &invalid),
        Err(CodecError::TransitionChanged)
    ));

    let mut invalid = legal.clone();
    invalid.phase = Phase::FreshStateAllocated;
    invalid.candidate.id = Some(42);
    assert!(matches!(
        validate_advance(&current, &invalid),
        Err(CodecError::IllegalPhaseAdvance { .. })
    ));

    let mut invalid = legal.clone();
    invalid.options.run_boot_sync = false;
    assert!(matches!(
        validate_advance(&current, &invalid),
        Err(CodecError::ImmutableTransitionDataChanged)
    ));

    let mut exhausted = record(Phase::CandidatePrepared);
    exhausted.generation = u64::MAX;
    let mut next = exhausted.clone();
    next.generation = 1;
    next.phase = Phase::TransactionTriggersStarted;
    assert!(matches!(
        validate_advance(&exhausted, &next),
        Err(CodecError::GenerationExhausted)
    ));

    let mut epoch_boot_changed = legal.clone();
    epoch_boot_changed.creation_epoch.boot_id = BootId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let mut epoch_namespace_changed = legal.clone();
    epoch_namespace_changed.creation_epoch.mount_namespace.inode += 1;
    let mut candidate_token_changed = legal.clone();
    candidate_token_changed.candidate.tree_token = tree_token('c');
    let mut previous_token_changed = legal.clone();
    previous_token_changed.previous.tree_token = tree_token('c');
    let mut candidate_runtime_changed = legal.clone();
    candidate_runtime_changed.candidate.usr_runtime_identity = identity(99);
    let mut previous_runtime_changed = legal.clone();
    previous_runtime_changed.previous.usr_runtime_identity = identity(98);

    for changed in [
        epoch_boot_changed,
        epoch_namespace_changed,
        candidate_token_changed,
        previous_token_changed,
        candidate_runtime_changed,
        previous_runtime_changed,
    ] {
        assert!(matches!(
            validate_advance(&current, &changed),
            Err(CodecError::ImmutableTransitionDataChanged)
        ));
    }
}

#[test]
fn rollback_plan_requirements_are_derived_from_source_and_observation() {
    let preparing = new_state_record(Phase::Preparing);
    let mut rollback = rollback_decided(&preparing);
    let plan = rollback.rollback.as_ref().unwrap();
    assert_eq!(plan.previous_archive, RollbackAction::NotRequired);
    assert_eq!(plan.usr_exchange, RollbackAction::NotRequired);
    assert_eq!(plan.candidate.action, RollbackAction::Pending);
    assert_eq!(plan.fresh_db, RollbackAction::NotRequired);
    assert_eq!(plan.boot, BootRollback::NotRequired);
    encode(&rollback).unwrap();

    rollback.rollback.as_mut().unwrap().usr_exchange = RollbackAction::Pending;
    assert!(matches!(
        encode(&rollback),
        Err(CodecError::InvalidRollbackRequirement {
            action: "usr-exchange",
            ..
        })
    ));

    let mut candidate_omitted = rollback_decided(&preparing);
    candidate_omitted.rollback.as_mut().unwrap().candidate.action = RollbackAction::NotRequired;
    assert!(matches!(
        encode(&candidate_omitted),
        Err(CodecError::InvalidRollbackRequirement {
            action: "candidate",
            ..
        })
    ));

    let mut falsely_applied = rollback_decided(&preparing);
    falsely_applied.rollback.as_mut().unwrap().candidate.action = RollbackAction::Applied;
    assert!(matches!(
        encode(&falsely_applied),
        Err(CodecError::RollbackPlanPhaseMismatch {
            phase: Phase::RollbackDecided
        })
    ));

    let allocating = new_state_record(Phase::FreshStateAllocating);
    let mut absent = rollback_decided(&allocating);
    absent.candidate.id = None;
    absent.rollback.as_mut().unwrap().fresh_db = RollbackAction::AlreadySatisfied;
    encode(&absent).unwrap();

    let mut row_observed = rollback_decided(&allocating);
    assert_eq!(row_observed.candidate.id, Some(42));
    assert_eq!(
        row_observed.rollback.as_ref().unwrap().fresh_db,
        RollbackAction::Pending
    );
    encode(&row_observed).unwrap();
    row_observed.candidate.id = None;
    assert!(matches!(encode(&row_observed), Err(CodecError::CandidateStateLayout)));

    let mut row_removed_concurrently = rollback_decided(&allocating);
    row_removed_concurrently.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
    let invalidation_intent = advance_record(&row_removed_concurrently, Phase::FreshDbInvalidationIntent);
    validate_advance(&row_removed_concurrently, &invalidation_intent).unwrap();
    let mut invalidated = advance_record(&invalidation_intent, Phase::FreshDbInvalidated);
    invalidated.rollback.as_mut().unwrap().fresh_db = RollbackAction::AlreadySatisfied;
    assert_eq!(invalidated.candidate.id, Some(42));
    validate_advance(&invalidation_intent, &invalidated).unwrap();
    encode(&invalidated).unwrap();

    let source = new_state_record(Phase::PreviousArchiveIntent);
    let mut observed_safe = rollback_decided(&source);
    let plan = observed_safe.rollback.as_mut().unwrap();
    plan.previous_archive = RollbackAction::AlreadySatisfied;
    plan.usr_exchange = RollbackAction::AlreadySatisfied;
    plan.candidate.action = RollbackAction::AlreadySatisfied;
    plan.fresh_db = RollbackAction::AlreadySatisfied;
    encode(&observed_safe).unwrap();
    assert_eq!(
        next_rollback_phase(observed_safe.rollback.as_ref().unwrap(), observed_safe.phase),
        Some(Phase::RollbackComplete)
    );

    observed_safe.rollback.as_mut().unwrap().previous_archive = RollbackAction::NotRequired;
    assert!(matches!(
        encode(&observed_safe),
        Err(CodecError::InvalidRollbackRequirement {
            action: "previous-archive",
            ..
        })
    ));

    let before_exchange = rollback_decided(&new_state_record(Phase::TransactionTriggersComplete));
    assert_eq!(
        before_exchange.rollback.as_ref().unwrap().usr_exchange,
        RollbackAction::NotRequired
    );
    let at_exchange = rollback_decided(&new_state_record(Phase::UsrExchangeIntent));
    assert_eq!(
        at_exchange.rollback.as_ref().unwrap().usr_exchange,
        RollbackAction::Pending
    );
    let before_archive = rollback_decided(&new_state_record(Phase::SystemTriggersComplete));
    assert_eq!(
        before_archive.rollback.as_ref().unwrap().previous_archive,
        RollbackAction::NotRequired
    );
    let at_archive = rollback_decided(&new_state_record(Phase::PreviousArchiveIntent));
    assert_eq!(
        at_archive.rollback.as_ref().unwrap().previous_archive,
        RollbackAction::Pending
    );

    let no_archive = without_previous_archive(
        new_state_record(Phase::BootSyncStarted),
        PreviousOrigin::SynthesizedEmpty,
    );
    assert_eq!(
        rollback_decided(&no_archive)
            .rollback
            .as_ref()
            .unwrap()
            .previous_archive,
        RollbackAction::NotRequired
    );
    assert_eq!(
        rollback_decided(&reblit_record(Phase::BootSyncStarted))
            .rollback
            .as_ref()
            .unwrap()
            .fresh_db,
        RollbackAction::NotRequired
    );
}

#[test]
fn rollback_candidate_disposition_and_external_effects_are_derived() {
    let cases = [
        (
            new_state_record(Phase::CandidatePrepared),
            AbortDisposition::Quarantine,
            false,
        ),
        (
            new_state_record(Phase::TransactionTriggersStarted),
            AbortDisposition::Quarantine,
            true,
        ),
        (
            reblit_record(Phase::TransactionTriggersStarted),
            AbortDisposition::Quarantine,
            true,
        ),
        (archived_record(Phase::Preparing), AbortDisposition::Rearchive, false),
        (
            archived_record(Phase::SystemTriggersStarted),
            AbortDisposition::Quarantine,
            true,
        ),
        (
            archived_record(Phase::SystemTriggersComplete),
            AbortDisposition::Rearchive,
            true,
        ),
        (
            archived_record(Phase::PreviousArchiveIntent),
            AbortDisposition::Rearchive,
            true,
        ),
    ];
    for (source, disposition, external_effects) in cases {
        let rollback = rollback_decided(&source);
        let plan = rollback.rollback.as_ref().unwrap();
        assert_eq!(plan.candidate.disposition, disposition);
        assert_eq!(plan.external_effects_may_remain, external_effects);
        encode(&rollback).unwrap();
    }

    let mut invalid = rollback_decided(&archived_record(Phase::Preparing));
    invalid.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Quarantine;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::InvalidCandidateDisposition { .. })
    ));

    let mut invalid = rollback_decided(&new_state_record(Phase::TransactionTriggersStarted));
    invalid.rollback.as_mut().unwrap().external_effects_may_remain = false;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::InvalidExternalEffectsEvidence { .. })
    ));
}

#[test]
fn rollback_recovery_order_and_status_updates_are_exact() {
    let mut source = new_state_record(Phase::PreviousArchiveIntent);
    source.options.run_boot_sync = false;
    let sequence = rollback_sequence(&source);
    assert_eq!(
        sequence.iter().map(|record| record.phase).collect::<Vec<_>>(),
        [
            Phase::RollbackDecided,
            Phase::PreviousRestoreIntent,
            Phase::PreviousRestoredToStaging,
            Phase::ReverseExchangeIntent,
            Phase::UsrRestored,
            Phase::CandidatePreserveIntent,
            Phase::CandidatePreserved,
            Phase::FreshDbInvalidationIntent,
            Phase::FreshDbInvalidated,
            Phase::RollbackComplete,
        ]
    );

    let decided = &sequence[0];
    let previous_intent = &sequence[1];
    assert_eq!(decided.rollback, previous_intent.rollback);

    let mut changed_during_intent = previous_intent.clone();
    changed_during_intent.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
    assert!(matches!(
        validate_advance(decided, &changed_during_intent),
        Err(CodecError::RollbackPlanChangedIllegally)
    ));
    assert_eq!(
        sequence[2].rollback.as_ref().unwrap().previous_archive,
        RollbackAction::Applied
    );
    assert_eq!(
        sequence[4].rollback.as_ref().unwrap().usr_exchange,
        RollbackAction::Applied
    );
    assert_eq!(
        sequence[6].rollback.as_ref().unwrap().candidate.action,
        RollbackAction::Applied
    );
    assert_eq!(sequence[8].rollback.as_ref().unwrap().fresh_db, RollbackAction::Applied);

    let mut skipped = rollback_decided(&source);
    let plan = skipped.rollback.as_mut().unwrap();
    plan.previous_archive = RollbackAction::AlreadySatisfied;
    plan.usr_exchange = RollbackAction::AlreadySatisfied;
    assert_eq!(
        next_rollback_phase(skipped.rollback.as_ref().unwrap(), skipped.phase),
        Some(Phase::CandidatePreserveIntent)
    );
    encode(&skipped).unwrap();

    let candidate_intent = advance_record(&skipped, Phase::CandidatePreserveIntent);
    validate_advance(&skipped, &candidate_intent).unwrap();
    let mut candidate_complete = advance_record(&candidate_intent, Phase::CandidatePreserved);
    candidate_complete.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
    validate_advance(&candidate_intent, &candidate_complete).unwrap();

    let mut out_of_order = candidate_intent.clone();
    out_of_order.phase = Phase::FreshDbInvalidationIntent;
    assert!(matches!(
        encode(&out_of_order),
        Err(CodecError::RollbackPlanPhaseMismatch { .. })
    ));
}

#[test]
fn ambiguous_boot_repair_is_terminal_unverified_and_nondeletable() {
    let source = new_state_record(Phase::BootSyncStarted);
    let mut prematurely_unverified = rollback_decided(&source);
    prematurely_unverified.rollback.as_mut().unwrap().boot = BootRollback::Unverified;
    assert!(matches!(
        encode(&prematurely_unverified),
        Err(CodecError::RollbackPlanPhaseMismatch {
            phase: Phase::RollbackDecided
        })
    ));

    let sequence = rollback_sequence(&source);
    let terminal = sequence.last().unwrap();
    assert_eq!(terminal.phase, Phase::BootRepairUnverified);
    assert_eq!(terminal.rollback.as_ref().unwrap().boot, BootRollback::Unverified);
    let mut attempted = terminal.clone();
    attempted.generation += 1;
    assert!(matches!(
        validate_advance(terminal, &attempted),
        Err(CodecError::TerminalPhaseAdvance)
    ));

    let (_temporary, store) = fixture();
    assert!(matches!(store.delete(terminal), Err(StorageError::DeleteNonterminal)));
    assert!(!terminal.phase.deletable());
    assert!(record(Phase::RollbackComplete).phase.deletable());

    let started = sequence
        .iter()
        .find(|record| record.phase == Phase::BootRepairStarted)
        .unwrap();
    for (outcome, status) in [
        (BootRepairOutcome::Applied, BootRollback::Applied),
        (
            BootRepairOutcome::AlreadySatisfied,
            BootRollback::AlreadySatisfied,
        ),
    ] {
        let complete = started.boot_repair_complete_successor(outcome).unwrap();
        assert_eq!(complete.phase, Phase::BootRepairComplete);
        assert_eq!(complete.rollback.as_ref().unwrap().boot, status);
        assert!(!complete.phase.blocks_advance());
        assert!(!complete.phase.deletable());

        assert!(matches!(
            complete.rollback_successor(None),
            Err(CodecError::ExplicitBootRepairSuccessorRequired(
                Phase::BootRepairComplete
            ))
        ));
        let finalized = complete.boot_repair_rollback_complete_successor().unwrap();
        assert_eq!(finalized.phase, Phase::RollbackComplete);
        assert_eq!(finalized.rollback.as_ref().unwrap().boot, status);
        assert!(finalized.phase.deletable());
    }

    let mut skipped_completion = started.clone();
    skipped_completion.generation += 1;
    skipped_completion.phase = Phase::RollbackComplete;
    skipped_completion.rollback.as_mut().unwrap().boot = BootRollback::Applied;
    assert!(matches!(
        validate_advance(started, &skipped_completion),
        Err(CodecError::IllegalPhaseAdvance {
            current: Phase::BootRepairStarted,
            next: Phase::RollbackComplete,
        })
    ));
}

#[test]
fn shared_transition_id_is_the_only_journal_correlation_encoding() {
    let value = new_state_record(Phase::Preparing);
    let encoded = encode(&value).unwrap();
    let payload = std::str::from_utf8(&encoded[HEADER_SIZE..]).unwrap();
    assert!(payload.contains("\"transition_id\":\"0123456789abcdef0123456789abcdef\""));
    assert!(!payload.contains("transaction_id"));
    assert_eq!(decode(&encoded).unwrap().transition_id, id());
}
