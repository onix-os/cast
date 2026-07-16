#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TriggerEffectError;

impl std::fmt::Display for TriggerEffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("injected transaction-trigger effect failure")
    }
}

impl std::error::Error for TriggerEffectError {}

fn coordinator_at_candidate_prepared(
    candidate_kind: CandidateKind,
) -> (CoordinatorFixture, PreparedTransactionTriggerCoordinator) {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    let expected_generation = if candidate_kind == CandidateKind::NewState { 5 } else { 3 };
    assert_record_prefix(
        coordinator.record(),
        match candidate_kind {
            CandidateKind::NewState => Operation::NewState,
            CandidateKind::Archived => Operation::ActivateArchived,
            CandidateKind::ActiveReblit => Operation::ActiveReblit,
        },
        Phase::CandidatePrepared,
        expected_generation,
    );
    let coordinator = match coordinator {
        PreparedStatefulTransitionCoordinator::NewStateIsolation(coordinator) => coordinator,
        PreparedStatefulTransitionCoordinator::ActiveReblitReservation(coordinator) => coordinator
            .reserve_for_transaction_triggers(&fixture.installation)
            .unwrap(),
        PreparedStatefulTransitionCoordinator::Archived(_) => {
            panic!("archived activation cannot yield transaction-trigger authority")
        }
    }
    .prepare_for_transaction_triggers(&fixture.installation)
    .unwrap();
    (fixture, coordinator)
}

#[test]
fn journal_coordinator_existing_candidate_database_removal_blocks_journal_creation() {
    for (candidate_kind, operation) in [
        (CandidateKind::Archived, Operation::ActivateArchived),
        (CandidateKind::ActiveReblit, Operation::ActiveReblit),
    ] {
        let (fixture, identity) = fixture(candidate_kind, PreviousKind::Active);
        fixture.database.remove(&fixture.candidate_state).unwrap();

        assert!(matches!(
            identity.begin_transition(request(candidate_kind, &fixture, false, false)),
            Err(StatefulTransitionCoordinatorError::ExistingCandidateOwnershipMismatch {
                operation: actual_operation,
                state,
                ownership: TransitionOwnership::Missing,
            }) if actual_operation == operation && state == i32::from(fixture.candidate_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
}

#[test]
fn journal_coordinator_distinct_previous_database_removal_blocks_journal_creation() {
    for (candidate_kind, operation) in [
        (CandidateKind::NewState, Operation::NewState),
        (CandidateKind::Archived, Operation::ActivateArchived),
    ] {
        let (fixture, identity) = fixture(candidate_kind, PreviousKind::Active);
        fixture.database.remove(&fixture.previous_state).unwrap();

        assert!(matches!(
            identity.begin_transition(request(candidate_kind, &fixture, false, false)),
            Err(StatefulTransitionCoordinatorError::PreviousStateOwnershipMismatch {
                operation: actual_operation,
                state,
                ownership: TransitionOwnership::Missing,
            }) if actual_operation == operation && state == i32::from(fixture.previous_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
}

#[test]
fn journal_coordinator_transaction_triggers_complete_exact_new_state_and_active_reblit_generations() {
    for (candidate_kind, operation, started_generation, complete_generation) in [
        (CandidateKind::NewState, Operation::NewState, 6, 7),
        (CandidateKind::ActiveReblit, Operation::ActiveReblit, 4, 5),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(candidate_kind);
        let transition = coordinator.record().transition_id.clone();
        let candidate = state::Id::from(coordinator.record().candidate.id.unwrap());
        let calls = std::cell::Cell::new(0usize);
        let trigger_output = fixture.candidate_path.join("transaction-trigger-output");

        let coordinator = coordinator
            .run_transaction_triggers(|authority| {
                calls.set(calls.get() + 1);
                let started = read_canonical(&fixture.installation.root);
                assert_record_prefix(&started, operation, Phase::TransactionTriggersStarted, started_generation);
                assert_eq!(authority.transition_id(), &transition);
                assert_eq!(authority.candidate_state(), candidate);
                let (retained, path) = authority.retained_candidate_usr();
                assert_eq!(path, fixture.candidate_path.as_path());
                let retained = retained.metadata().unwrap();
                let named = fs::metadata(path).unwrap();
                assert_eq!((retained.dev(), retained.ino()), (named.dev(), named.ino()));
                write_canonical_file(&trigger_output, b"durable transaction-trigger output\n");
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap();

        assert_eq!(calls.get(), 1);
        assert_record_prefix(
            coordinator.record(),
            operation,
            Phase::TransactionTriggersComplete,
            complete_generation,
        );
        assert_eq!(read_canonical(&fixture.installation.root), *coordinator.record());
        assert_eq!(
            fs::read(&trigger_output).unwrap(),
            b"durable transaction-trigger output\n"
        );
        let output_metadata = fs::symlink_metadata(trigger_output).unwrap();
        assert!(output_metadata.file_type().is_file());
        assert_eq!(output_metadata.uid(), nix::unistd::Uid::effective().as_raw());
        assert_eq!(output_metadata.permissions().mode() & 0o7777, 0o644);
        assert_eq!(output_metadata.nlink(), 1);
    }
}

#[test]
fn journal_coordinator_archived_transaction_triggers_are_rejected_without_effect() {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::Archived);
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    let prepared = coordinator.record().clone();
    let calls = std::cell::Cell::new(0usize);

    let PreparedStatefulTransitionCoordinator::Archived(_archived) = coordinator else {
        calls.set(calls.get() + 1);
        panic!("archived activation acquired transaction-trigger authority")
    };
    assert_eq!(calls.get(), 0);
    assert_eq!(read_canonical(&fixture.installation.root), prepared);
}

#[test]
fn journal_coordinator_transaction_trigger_effect_error_runs_once_and_preserves_started() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let transition = coordinator.record().transition_id.clone();
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|authority| {
            calls.set(calls.get() + 1);
            assert_eq!(authority.transition_id(), &transition);
            assert_eq!(read_canonical(&fixture.installation.root).phase, Phase::TransactionTriggersStarted);
            Err(TriggerEffectError)
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::Effect {
            transition_id,
            source: TriggerEffectError,
        } if transition_id == transition
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
}

#[test]
fn journal_coordinator_transaction_trigger_intent_faults_leave_old_or_successor_without_effect() {
    for (post_exchange, expected_phase, expected_generation) in [
        (false, Phase::CandidatePrepared, 3),
        (true, Phase::TransactionTriggersStarted, 4),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
        let transition = coordinator.record().transition_id.clone();
        let calls = std::cell::Cell::new(0usize);
        if post_exchange {
            crate::transition_journal::arm_next_update_first_directory_sync_fault();
        } else {
            crate::transition_journal::arm_next_temporary_sync_fault();
        }

        let failure = coordinator
            .run_transaction_triggers(|_| {
                calls.set(calls.get() + 1);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        if post_exchange {
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        } else {
            crate::transition_journal::assert_temporary_sync_fault_consumed();
        }
        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::IntentPersistence { transition_id, .. }
                if transition_id == transition
        ));
        assert_eq!(calls.get(), 0);
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::ActiveReblit,
            expected_phase,
            expected_generation,
        );
    }
}

#[test]
fn journal_coordinator_transaction_trigger_completion_faults_leave_started_or_complete_after_one_effect() {
    for (post_exchange, expected_phase, expected_generation) in [
        (false, Phase::TransactionTriggersStarted, 6),
        (true, Phase::TransactionTriggersComplete, 7),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
        let transition = coordinator.record().transition_id.clone();
        let calls = std::cell::Cell::new(0usize);

        let failure = coordinator
            .run_transaction_triggers(|_| {
                calls.set(calls.get() + 1);
                if post_exchange {
                    crate::transition_journal::arm_next_update_first_directory_sync_fault();
                } else {
                    crate::transition_journal::arm_next_temporary_sync_fault();
                }
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        if post_exchange {
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        } else {
            crate::transition_journal::assert_temporary_sync_fault_consumed();
        }
        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::CompletionPersistence { transition_id, .. }
                if transition_id == transition
        ));
        assert_eq!(calls.get(), 1);
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::NewState,
            expected_phase,
            expected_generation,
        );
    }
}

#[test]
fn journal_coordinator_transaction_trigger_preflight_failure_runs_no_effect() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let prepared = coordinator.record().clone();
    fs::set_permissions(&fixture.candidate_path, fs::Permissions::from_mode(0o700)).unwrap();
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::Preflight { transition_id, .. }
            if transition_id == prepared.transition_id
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(read_canonical(&fixture.installation.root), prepared);
}

#[test]
fn journal_coordinator_transaction_trigger_post_effect_failure_preserves_started() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let transition = coordinator.record().transition_id.clone();
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            fs::set_permissions(&fixture.candidate_path, fs::Permissions::from_mode(0o700)).unwrap();
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, .. }
            if transition_id == transition
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::TransactionTriggersStarted,
        4,
    );
}

#[test]
fn journal_coordinator_transaction_trigger_post_effect_database_changes_are_blocked() {
    for remove_fresh in [false, true] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
        let transition = coordinator.record().transition_id.clone();
        let candidate = state::Id::from(coordinator.record().candidate.id.unwrap());

        let failure = coordinator
            .run_transaction_triggers(|_| {
                if remove_fresh {
                    fixture
                        .database
                        .remove_transition_if_matches(candidate, &transition)
                        .unwrap();
                } else {
                    fixture
                        .database
                        .clear_transition_if_matches(candidate, &transition)
                        .unwrap();
                }
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, .. }
                if transition_id == transition
        ));
        assert_eq!(
            fixture
                .database
                .transition_ownership(candidate, &transition)
                .unwrap(),
            if remove_fresh {
                TransitionOwnership::Missing
            } else {
                TransitionOwnership::Cleared
            }
        );
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::NewState,
            Phase::TransactionTriggersStarted,
            6,
        );
    }

    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
    let transition = coordinator.record().transition_id.clone();
    let candidate = state::Id::from(coordinator.record().candidate.id.unwrap());
    let failure = coordinator
        .run_transaction_triggers(|_| {
            fixture.database.remove(&candidate).unwrap();
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, .. }
            if transition_id == transition
    ));
    assert_eq!(
        fixture
            .database
            .transition_ownership(candidate, &transition)
            .unwrap(),
        TransitionOwnership::Missing
    );
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::TransactionTriggersStarted,
        4,
    );
}

#[test]
fn journal_coordinator_transaction_trigger_post_effect_previous_database_removal_is_blocked() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let transition = coordinator.record().transition_id.clone();
    let previous = fixture.previous_state;

    let failure = coordinator
        .run_transaction_triggers(|_| {
            fixture.database.remove(&previous).unwrap();
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence {
            transition_id,
            source: StatefulTransitionCoordinatorError::PreviousStateOwnershipMismatch {
                operation: Operation::NewState,
                state,
                ownership: TransitionOwnership::Missing,
            },
        } if transition_id == transition && state == i32::from(previous)
    ));
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
}

#[test]
fn journal_coordinator_transaction_trigger_global_database_audit_blocks_foreign_rows() {
    for (candidate_kind, operation, started_generation) in [
        (CandidateKind::NewState, Operation::NewState, 6),
        (CandidateKind::ActiveReblit, Operation::ActiveReblit, 4),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(candidate_kind);
        let transition = coordinator.record().transition_id.clone();
        let foreign_transition = other_transition_id();
        let mut foreign_state = None;

        let failure = coordinator
            .run_transaction_triggers(|_| {
                foreign_state = Some(
                    fixture
                        .database
                        .add_with_transition(
                            &foreign_transition,
                            &[],
                            Some("foreign transaction injected during triggers"),
                            None,
                        )
                        .unwrap()
                        .id,
                );
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        match (candidate_kind, failure) {
            (
                CandidateKind::NewState,
                StatefulTransactionTriggerFailure::PostEffectEvidence {
                    transition_id,
                    source:
                        StatefulTransitionCoordinatorError::StateEvidence(
                            db::state::TransitionEvidenceError::MultipleInFlightTransitions,
                        ),
                },
            ) => assert_eq!(transition_id, transition),
            (
                CandidateKind::ActiveReblit,
                StatefulTransactionTriggerFailure::PostEffectEvidence {
                    transition_id,
                    source: StatefulTransitionCoordinatorError::TransitionAuditMismatch { actual: Some(actual), .. },
                },
            ) => {
                assert_eq!(transition_id, transition);
                assert_eq!(actual.state_id, foreign_state.unwrap());
                assert_eq!(actual.transition_id, foreign_transition);
            }
            (_, failure) => panic!("unexpected global-audit failure: {failure:#?}"),
        }
        let foreign_state = foreign_state.unwrap();
        assert_eq!(
            fixture
                .database
                .transition_ownership(foreign_state, &foreign_transition)
                .unwrap(),
            TransitionOwnership::Matching
        );
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            operation,
            Phase::TransactionTriggersStarted,
            started_generation,
        );
    }
}

#[test]
fn journal_coordinator_transaction_trigger_state_id_and_public_name_substitution_are_blocked() {
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::ActiveReblit);
        let transition = coordinator.record().transition_id.clone();
        let state_id = state_id_path(&fixture);
        let displaced = fixture.candidate_path.join(".stateID.displaced");
        let failure = coordinator
            .run_transaction_triggers(|_| {
                let bytes = fs::read(&state_id).unwrap();
                fs::rename(&state_id, &displaced).unwrap();
                write_canonical_file(&state_id, &bytes);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, .. }
                if transition_id == transition
        ));
        let replacement = fs::symlink_metadata(&state_id).unwrap();
        let original = fs::symlink_metadata(displaced).unwrap();
        assert_ne!((replacement.dev(), replacement.ino()), (original.dev(), original.ino()));
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::ActiveReblit,
            Phase::TransactionTriggersStarted,
            4,
        );
    }

    {
        let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
        let transition = coordinator.record().transition_id.clone();
        let parked = fixture.installation.staging_path("usr-trigger-substituted");
        let public_name = fixture.candidate_path.clone();
        let failure = coordinator
            .run_transaction_triggers(|_| {
                fs::rename(&public_name, &parked).unwrap();
                create_canonical_directory(&public_name);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        assert!(matches!(
            failure,
            StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, .. }
                if transition_id == transition
        ));
        assert!(!public_name.join(".cast-tree-id").exists());
        assert!(parked.join(".cast-tree-id").is_file());
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::NewState,
            Phase::TransactionTriggersStarted,
            6,
        );
    }
}

#[test]
fn journal_coordinator_transaction_trigger_failure_releases_journal_while_error_lives() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let root = fixture.installation.root.clone();
    let failure = coordinator
        .run_transaction_triggers(|_| Err(TriggerEffectError))
        .unwrap_err();
    assert!(matches!(failure, StatefulTransactionTriggerFailure::Effect { .. }));

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let record = TransitionJournalStore::open(&root).unwrap().load().unwrap().unwrap();
        sender.send((record.phase, record.generation)).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(10)),
        Ok((Phase::TransactionTriggersStarted, 6)),
        "a returned trigger failure retained the exclusive journal lock"
    );
    worker.join().unwrap();
    drop(failure);
}
