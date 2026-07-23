fn coordinator_ready_for_system_triggers(
    candidate_kind: CandidateKind,
    run_system_triggers: bool,
) -> (CoordinatorFixture, RootLinksCompleteCoordinator) {
    coordinator_ready_for_system_triggers_with_options(
        candidate_kind,
        run_system_triggers,
        false,
    )
}

fn coordinator_ready_for_system_triggers_with_options(
    candidate_kind: CandidateKind,
    run_system_triggers: bool,
    run_boot_sync: bool,
) -> (CoordinatorFixture, RootLinksCompleteCoordinator) {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(candidate_kind, PreviousKind::Active);
    let (fixture, intent, authority) = coordinator_from_exchange_fixture_with_options(
        candidate_kind,
        fixture,
        identity,
        authority,
        run_system_triggers,
        run_boot_sync,
    );
    let exchanged = intent.execute_usr_exchange(authority).unwrap();
    let complete = exchanged.publish_root_abi().unwrap();
    (fixture, complete)
}

#[test]
fn journal_coordinator_active_reblit_no_boot_commit_decision_is_exact() {
    let (fixture, coordinator) =
        coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
    let complete = coordinator
        .run_system_triggers(|_| Ok::<(), TriggerEffectError>(()))
        .unwrap();
    let source = complete.record().clone();
    assert_record_prefix(
        &source,
        Operation::ActiveReblit,
        Phase::SystemTriggersComplete,
        10,
    );
    assert!(!source.options.run_boot_sync);
    assert!(source.boot_publication_receipts.is_none());
    let expected = source.forward_successor(None).unwrap();

    let handoff = complete.commit_active_reblit_without_boot().unwrap();

    assert_eq!(handoff.record(), &expected);
    assert_record_prefix(
        handoff.record(),
        Operation::ActiveReblit,
        Phase::CommitDecided,
        11,
    );
    assert_eq!(handoff.journal().load().unwrap(), Some(expected));
    assert_eq!(read_canonical(&fixture.installation.root), *handoff.record());
}

#[test]
fn journal_coordinator_active_reblit_no_boot_commit_rejects_other_routes_without_record_change() {
    {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::NewState, true);
        let complete = coordinator
            .run_system_triggers(|_| Ok::<(), TriggerEffectError>(()))
            .unwrap();
        let source = complete.record().clone();
        assert!(matches!(
            complete.commit_active_reblit_without_boot(),
            Err(ActiveReblitNoBootCommitDecisionFailure::SourceContract { .. })
        ));
        assert_eq!(read_canonical(&fixture.installation.root), source);
    }

    {
        let (fixture, coordinator) = coordinator_ready_for_system_triggers_with_options(
            CandidateKind::ActiveReblit,
            true,
            true,
        );
        let complete = coordinator
            .run_system_triggers(|_| Ok::<(), TriggerEffectError>(()))
            .unwrap();
        let source = complete.record().clone();
        assert!(matches!(
            complete.commit_active_reblit_without_boot(),
            Err(ActiveReblitNoBootCommitDecisionFailure::SourceContract { .. })
        ));
        assert_eq!(read_canonical(&fixture.installation.root), source);
    }
}

#[test]
fn journal_coordinator_active_reblit_no_boot_commit_faults_classify_only_source_or_successor() {
    for successor_visible in [false, true] {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
        let complete = coordinator
            .run_system_triggers(|_| Ok::<(), TriggerEffectError>(()))
            .unwrap();
        if successor_visible {
            crate::transition_journal::arm_next_update_first_directory_sync_fault();
        } else {
            crate::transition_journal::arm_next_temporary_sync_fault();
        }

        let failure = match complete.commit_active_reblit_without_boot() {
            Ok(_) => panic!("faulted no-boot commit unexpectedly returned a handoff"),
            Err(failure) => failure,
        };

        if successor_visible {
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        } else {
            crate::transition_journal::assert_temporary_sync_fault_consumed();
        }
        assert!(matches!(
            failure,
            ActiveReblitNoBootCommitDecisionFailure::Persistence {
                source: BoundSystemTriggerAdvanceFailure::Advance { durable, .. },
                ..
            } if durable == if successor_visible {
                DurableSystemTriggerRecord::Successor
            } else {
                DurableSystemTriggerRecord::Predecessor
            }
        ));
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::ActiveReblit,
            if successor_visible {
                Phase::CommitDecided
            } else {
                Phase::SystemTriggersComplete
            },
            if successor_visible { 11 } else { 10 },
        );
    }
}

#[test]
fn journal_coordinator_active_reblit_no_boot_commit_binding_replacements_fail_stop() {
    for fresh_binding_seam in [false, true] {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
        let complete = coordinator
            .run_system_triggers(|_| Ok::<(), TriggerEffectError>(()))
            .unwrap();
        let canonical = canonical_journal(&fixture.installation.root);
        let displaced = fixture.installation.root.join(format!(
            "no-boot-commit-{}.displaced",
            if fresh_binding_seam { "fresh" } else { "old" }
        ));
        let replace = move || replace_regular_file_with_same_bytes_at(&canonical, &displaced);
        if fresh_binding_seam {
            arm_before_reopened_fresh_binding_validation(Phase::CommitDecided, replace);
        } else {
            arm_after_bound_successor_same_store_validation(Phase::CommitDecided, replace);
        }

        assert!(matches!(
            complete.commit_active_reblit_without_boot(),
            Err(ActiveReblitNoBootCommitDecisionFailure::Persistence { .. })
        ));
        assert_record_prefix(
            &read_canonical(&fixture.installation.root),
            Operation::ActiveReblit,
            Phase::CommitDecided,
            11,
        );
    }
}

#[test]
fn journal_coordinator_system_triggers_complete_exact_new_state_and_active_reblit_generations() {
    for (candidate_kind, operation, started_generation, complete_generation) in [
        (CandidateKind::NewState, Operation::NewState, 11, 12),
        (CandidateKind::ActiveReblit, Operation::ActiveReblit, 9, 10),
    ] {
        let (fixture, coordinator) = coordinator_ready_for_system_triggers(candidate_kind, true);
        let transition = coordinator.record().transition_id.clone();
        let candidate = state::Id::from(coordinator.record().candidate.id.unwrap());
        let calls = std::cell::Cell::new(0usize);
        let output = fixture.installation.root.join("usr/system-trigger-output");

        let complete = coordinator
            .run_system_triggers(|authority| {
                calls.set(calls.get() + 1);
                assert_eq!(authority.transition_id(), &transition);
                assert_eq!(authority.candidate_state(), candidate);
                let (installation, retained_usr, _isolation_root) = authority.retained_view();
                assert_eq!(installation.root, fixture.installation.root);
                let retained = retained_usr.metadata().unwrap();
                let named = fs::metadata(installation.root.join("usr")).unwrap();
                assert_eq!((retained.dev(), retained.ino()), (named.dev(), named.ino()));
                assert_record_prefix(
                    &read_canonical(&fixture.installation.root),
                    operation,
                    Phase::SystemTriggersStarted,
                    started_generation,
                );
                write_canonical_file(&output, b"durable system-trigger output\n");
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap();

        assert_eq!(calls.get(), 1);
        assert_record_prefix(
            complete.record(),
            operation,
            Phase::SystemTriggersComplete,
            complete_generation,
        );
        assert_eq!(read_canonical(&fixture.installation.root), *complete.record());
        assert_eq!(fs::read(output).unwrap(), b"durable system-trigger output\n");
        complete.revalidate_retained_authorities().unwrap();
    }
}

#[test]
fn journal_coordinator_system_triggers_reject_archived_or_disabled_paths_without_effect() {
    {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::Archived, true);
        let source = coordinator.record().clone();
        let calls = std::cell::Cell::new(0usize);
        let failure = coordinator
            .run_system_triggers(|_| {
                calls.set(calls.get() + 1);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();
        assert!(matches!(
            failure,
            StatefulSystemTriggerFailure::ArchivedIsolationUnsupported { transition_id }
                if transition_id == source.transition_id
        ));
        assert_eq!(calls.get(), 0);
        assert_eq!(read_canonical(&fixture.installation.root), source);
    }

    for candidate_kind in [CandidateKind::NewState, CandidateKind::ActiveReblit] {
        let (fixture, coordinator) = coordinator_ready_for_system_triggers(candidate_kind, false);
        let source = coordinator.record().clone();
        let calls = std::cell::Cell::new(0usize);
        let failure = coordinator
            .run_system_triggers(|_| {
                calls.set(calls.get() + 1);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();
        assert!(matches!(
            failure,
            StatefulSystemTriggerFailure::SuccessorContract {
                expected_phase: Phase::SystemTriggersStarted,
                ..
            }
        ));
        assert_eq!(calls.get(), 0);
        assert_eq!(read_canonical(&fixture.installation.root), source);
    }
}

#[test]
fn journal_coordinator_system_trigger_effect_failure_runs_once_and_preserves_started() {
    let (fixture, coordinator) =
        coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
    let transition = coordinator.record().transition_id.clone();
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_system_triggers(|authority| {
            calls.set(calls.get() + 1);
            assert_eq!(authority.transition_id(), &transition);
            Err(TriggerEffectError)
        })
        .unwrap_err();

    assert!(matches!(
        failure,
        StatefulSystemTriggerFailure::Effect {
            transition_id,
            source: TriggerEffectError,
        } if transition_id == transition
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::SystemTriggersStarted,
        9,
    );
}

#[test]
fn journal_coordinator_system_trigger_persistence_faults_leave_only_bound_predecessor_or_successor() {
    for (completion, successor_visible) in [
        (false, false),
        (false, true),
        (true, false),
        (true, true),
    ] {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::NewState, true);
        let calls = std::cell::Cell::new(0usize);
        if !completion {
            if successor_visible {
                crate::transition_journal::arm_next_update_first_directory_sync_fault();
            } else {
                crate::transition_journal::arm_next_temporary_sync_fault();
            }
        }

        let failure = coordinator
            .run_system_triggers(|_| {
                calls.set(calls.get() + 1);
                if completion {
                    if successor_visible {
                        crate::transition_journal::arm_next_update_first_directory_sync_fault();
                    } else {
                        crate::transition_journal::arm_next_temporary_sync_fault();
                    }
                }
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        if successor_visible {
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        } else {
            crate::transition_journal::assert_temporary_sync_fault_consumed();
        }
        if completion {
            assert!(matches!(
                failure,
                StatefulSystemTriggerFailure::CompletionPersistence { .. }
            ));
            assert_eq!(calls.get(), 1);
            assert_record_prefix(
                &read_canonical(&fixture.installation.root),
                Operation::NewState,
                if successor_visible {
                    Phase::SystemTriggersComplete
                } else {
                    Phase::SystemTriggersStarted
                },
                if successor_visible { 12 } else { 11 },
            );
        } else {
            assert!(matches!(
                failure,
                StatefulSystemTriggerFailure::IntentPersistence { .. }
            ));
            assert_eq!(calls.get(), 0);
            assert_record_prefix(
                &read_canonical(&fixture.installation.root),
                Operation::NewState,
                if successor_visible {
                    Phase::SystemTriggersStarted
                } else {
                    Phase::RootLinksComplete
                },
                if successor_visible { 11 } else { 10 },
            );
        }
    }
}

#[test]
fn journal_coordinator_system_trigger_successor_inode_replacements_fail_stop_at_both_reopen_seams() {
    for fresh_binding_seam in [false, true] {
        for replacement_phase in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
            let (fixture, coordinator) =
                coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
            let canonical = canonical_journal(&fixture.installation.root);
            let displaced = fixture.installation.root.join(format!(
                "system-{}-{}-binding.displaced",
                if replacement_phase == Phase::SystemTriggersStarted {
                    "started"
                } else {
                    "complete"
                },
                if fresh_binding_seam { "fresh" } else { "old" }
            ));
            let replace = move || replace_regular_file_with_same_bytes_at(&canonical, &displaced);
            if fresh_binding_seam {
                arm_before_reopened_fresh_binding_validation(replacement_phase, replace);
            } else {
                arm_after_bound_successor_same_store_validation(replacement_phase, replace);
            }
            let calls = std::cell::Cell::new(0usize);

            let failure = coordinator
                .run_system_triggers(|_| {
                    calls.set(calls.get() + 1);
                    Ok::<(), TriggerEffectError>(())
                })
                .unwrap_err();

            match replacement_phase {
                Phase::SystemTriggersStarted => {
                    assert!(matches!(
                        failure,
                        StatefulSystemTriggerFailure::IntentPersistence { .. }
                    ));
                    assert_eq!(calls.get(), 0);
                }
                Phase::SystemTriggersComplete => {
                    assert!(matches!(
                        failure,
                        StatefulSystemTriggerFailure::CompletionPersistence { .. }
                    ));
                    assert_eq!(calls.get(), 1);
                }
                _ => unreachable!(),
            }
            assert_eq!(read_canonical(&fixture.installation.root).phase, replacement_phase);
        }
    }
}

#[test]
fn journal_coordinator_system_trigger_reopen_never_waits_behind_writer_first_contender() {
    for contested_phase in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
        let (fixture, coordinator) =
            coordinator_ready_for_system_triggers(CandidateKind::ActiveReblit, true);
        let root = fixture.installation.root.clone();
        let (begin_sender, begin_receiver) = std::sync::mpsc::sync_channel(1);
        let (acquired_sender, acquired_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let contender = std::thread::spawn(move || {
            begin_receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("reopen handoff did not release the contender");
            let journal = TransitionJournalStore::open(&root).unwrap();
            acquired_sender.send(()).unwrap();
            release_receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("test did not release the journal contender");
            drop(journal);
        });
        arm_after_old_journal_drop_before_reopen(contested_phase, move || {
            begin_sender.send(()).unwrap();
            acquired_receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("journal contender did not acquire the reopen gap");
        });
        let calls = std::cell::Cell::new(0usize);

        let failure = coordinator
            .run_system_triggers(|_| {
                calls.set(calls.get() + 1);
                Ok::<(), TriggerEffectError>(())
            })
            .unwrap_err();

        match contested_phase {
            Phase::SystemTriggersStarted => {
                assert!(matches!(
                    failure,
                    StatefulSystemTriggerFailure::IntentPersistence { .. }
                ));
                assert_eq!(calls.get(), 0);
            }
            Phase::SystemTriggersComplete => {
                assert!(matches!(
                    failure,
                    StatefulSystemTriggerFailure::CompletionPersistence { .. }
                ));
                assert_eq!(calls.get(), 1);
            }
            _ => unreachable!(),
        }
        assert_eq!(read_canonical(&fixture.installation.root).phase, contested_phase);
        release_sender.send(()).unwrap();
        contender.join().unwrap();
    }
}
