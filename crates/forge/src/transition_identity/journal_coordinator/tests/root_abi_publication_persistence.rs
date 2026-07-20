#[test]
fn journal_coordinator_root_links_complete_root_directory_sync_failure_keeps_usr_exchanged() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(candidate_kind, 0);
        let source = exchanged.record().clone();
        let database_before = usr_exchange_database_snapshot(&fixture, &source);
        crate::client::arm_retained_root_abi_sync_fault();

        let failure = exchanged.publish_root_abi().unwrap_err();

        crate::client::assert_retained_root_abi_sync_fault_consumed();
        assert!(matches!(failure, RootAbiPublicationFailure::Publication { .. }));
        assert_usr_exchanged_source(&fixture, &source);
        assert_eq!(usr_exchange_database_snapshot(&fixture, &source), database_before);
        assert_root_links_complete(&fixture);
    }
}

#[test]
fn journal_coordinator_root_links_complete_journal_persistence_faults_expose_only_source_or_successor() {
    let faults: [(fn(), fn(), bool); 5] = [
        (
            crate::transition_journal::arm_next_temporary_sync_fault,
            crate::transition_journal::assert_temporary_sync_fault_consumed,
            false,
        ),
        (
            crate::transition_journal::arm_next_update_exchange_fault,
            crate::transition_journal::assert_update_exchange_fault_consumed,
            false,
        ),
        (
            crate::transition_journal::arm_next_update_first_directory_sync_fault,
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed,
            true,
        ),
        (
            crate::transition_journal::arm_next_displaced_unlink_fault,
            crate::transition_journal::assert_displaced_unlink_fault_consumed,
            true,
        ),
        (
            crate::transition_journal::arm_next_update_final_directory_sync_fault,
            crate::transition_journal::assert_update_final_directory_sync_fault_consumed,
            true,
        ),
    ];

    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        for (arm, assert_consumed, successor_visible) in faults {
            let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(candidate_kind, 0);
            let source = exchanged.record().clone();
            let successor = source.forward_successor(None).unwrap();
            let database_before = usr_exchange_database_snapshot(&fixture, &source);
            let namespace_before =
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root);
            arm();

            let failure = exchanged.publish_root_abi().unwrap_err();

            assert_consumed();
            assert!(matches!(
                failure,
                RootAbiPublicationFailure::CompletionPersistence { .. }
            ));
            assert_eq!(
                read_canonical(&fixture.installation.root),
                if successor_visible { successor } else { source.clone() },
                "operation={candidate_kind:?} successor_visible={successor_visible}"
            );
            assert_eq!(usr_exchange_database_snapshot(&fixture, &source), database_before);
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                namespace_before
            );
            assert_root_links_complete(&fixture);
        }
    }
}

#[test]
fn journal_coordinator_root_links_complete_restart_composes_from_exact_source_and_stops_at_unsupported_successor() {
    {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::NewState, 0);
        let source = exchanged.record().clone();
        crate::transition_journal::arm_next_temporary_sync_fault();
        let failure = exchanged.publish_root_abi().unwrap_err();
        crate::transition_journal::assert_temporary_sync_fault_consumed();
        assert!(matches!(
            failure,
            RootAbiPublicationFailure::CompletionPersistence { .. }
        ));
        assert_usr_exchanged_source(&fixture, &source);
        assert_root_links_complete(&fixture);

        assert_usr_exchange_post_recovers_to_pending_reverse(
            &fixture.installation,
            &fixture.database,
            &fixture.layout_database,
        );

        assert_eq!(read_canonical(&fixture.installation.root).phase, Phase::RollbackDecided);
        assert_root_links_complete(&fixture);
    }

    {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
        let successor = exchanged.record().forward_successor(None).unwrap();
        crate::transition_journal::arm_next_update_first_directory_sync_fault();
        let failure = exchanged.publish_root_abi().unwrap_err();
        crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        assert!(matches!(
            failure,
            RootAbiPublicationFailure::CompletionPersistence { .. }
        ));
        assert_eq!(read_canonical(&fixture.installation.root), successor);
        assert_root_links_complete(&fixture);

        assert_root_links_complete_restart_is_pending(
            &fixture.installation,
            &fixture.database,
            &fixture.layout_database,
        );

        assert_eq!(read_canonical(&fixture.installation.root), successor);
        assert_root_links_complete(&fixture);
    }
}

#[test]
fn journal_coordinator_root_links_complete_failures_release_journal_and_writer_authorities_while_error_lives() {
    for failure_kind in ["publication", "completion"] {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
        match failure_kind {
            "publication" => crate::client::arm_retained_root_abi_sync_fault(),
            "completion" => crate::transition_journal::arm_next_temporary_sync_fault(),
            _ => unreachable!(),
        }
        let failure = exchanged.publish_root_abi().unwrap_err();
        match failure_kind {
            "publication" => crate::client::assert_retained_root_abi_sync_fault_consumed(),
            "completion" => crate::transition_journal::assert_temporary_sync_fault_consumed(),
            _ => unreachable!(),
        }

        let root = fixture.installation.root.clone();
        let (journal_sender, journal_receiver) = std::sync::mpsc::sync_channel(1);
        let journal_worker = std::thread::spawn(move || {
            let store = TransitionJournalStore::open(&root).unwrap();
            let phase = store.load().unwrap().map(|record| record.phase);
            journal_sender.send(phase).unwrap();
        });
        assert_eq!(
            journal_receiver.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(Some(Phase::UsrExchanged)),
            "{failure_kind} error retained the journal authority"
        );
        journal_worker.join().unwrap();

        // The original Installation intentionally still describes the old
        // active state. Refresh only that discovery witness so this probe can
        // get past active-state capture and prove that it reached the
        // unresolved journal while the failure value remains alive.
        let mut installation = fixture.installation.clone();
        installation.active_state = Some(fixture.candidate_state);
        let (writer_sender, writer_receiver) = std::sync::mpsc::sync_channel(1);
        let writer_worker = std::thread::spawn(move || {
            let result = JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&installation, None);
            writer_sender
                .send(matches!(
                    result,
                    Err(crate::client::JournalUsrExchangeAuthorityError::UnresolvedJournal { .. })
                ))
                .unwrap();
        });
        assert_eq!(
            writer_receiver.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(true),
            "{failure_kind} error retained the cooperating-writer authority"
        );
        writer_worker.join().unwrap();

        assert!(
            matches!(
                (&failure_kind, &failure),
                (&"publication", RootAbiPublicationFailure::Publication { .. })
                    | (&"completion", RootAbiPublicationFailure::CompletionPersistence { .. })
            ),
            "unexpected {failure_kind} authority-release failure: {failure:?}"
        );
    }
}
