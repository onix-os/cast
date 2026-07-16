#[test]
fn journal_coordinator_new_state_reaches_candidate_prepared_through_exact_generations() {
    let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
    assert_new_state_payload_sentinel(&fixture);
    assert_candidate_state_id_absent(&fixture);
    let coordinator = identity
        .begin_transition(request(CandidateKind::NewState, &fixture, true, true))
        .unwrap();
    assert_record_prefix(coordinator.record(), Operation::NewState, Phase::Preparing, 1);
    assert_eq!(coordinator.record().candidate.id, None);
    assert_candidate_state_id_absent(&fixture);

    let coordinator = coordinator.begin_fresh_allocation().unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::FreshStateAllocating,
        2,
    );
    assert_eq!(coordinator.record().candidate.id, None);
    assert_candidate_state_id_absent(&fixture);

    let unrelated = fixture
        .database
        .add(&[], Some("force a dynamic fresh-state ID"), None)
        .unwrap();
    assert_eq!(unrelated.id, fixture.candidate_state);
    let allocated = allocate_matching_state(&fixture, &coordinator);
    assert_ne!(allocated, fixture.candidate_state);
    let coordinator = coordinator
        .finish_fresh_allocation(&fixture.database, allocated)
        .unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::FreshStateAllocated,
        3,
    );
    assert_eq!(coordinator.record().candidate.id, Some(i32::from(allocated)));
    assert_candidate_state_id_absent(&fixture);

    let coordinator = coordinator.begin_candidate_prepare().unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::CandidatePrepareStarted,
        4,
    );
    assert_candidate_state_id_absent(&fixture);
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::CandidatePrepared,
        5,
    );
    assert_eq!(coordinator.record().candidate.id, Some(i32::from(allocated)));
    assert_candidate_state_id(&fixture, allocated);
    assert_candidate_metadata(&fixture);
    assert_new_state_payload_sentinel(&fixture);
}

#[test]
fn journal_coordinator_new_state_previous_origins_and_options_are_exact() {
    for (previous_kind, run_system_triggers, run_boot_sync, expected_origin) in [
        (PreviousKind::Active, false, true, PreviousOrigin::ActiveState),
        (
            PreviousKind::SynthesizedEmpty,
            true,
            false,
            PreviousOrigin::SynthesizedEmpty,
        ),
    ] {
        let (fixture, identity) = fixture(CandidateKind::NewState, previous_kind);
        let previous = match previous_kind {
            PreviousKind::Active => NewStatePrevious::Active(fixture.previous_state),
            PreviousKind::SynthesizedEmpty => NewStatePrevious::SynthesizedEmpty,
        };
        let coordinator = identity
            .begin_transition(StatefulTransitionRequest::NewState {
                previous,
                run_system_triggers,
                run_boot_sync,
            })
            .unwrap();
        let record = coordinator.record();

        assert_record_prefix(record, Operation::NewState, Phase::Preparing, 1);
        assert_eq!(record.candidate.id, None);
        assert_eq!(record.candidate.origin, CandidateOrigin::Fresh);
        assert_eq!(record.previous.origin, expected_origin);
        assert_eq!(
            record.previous.id,
            (previous_kind == PreviousKind::Active).then_some(i32::from(fixture.previous_state))
        );
        assert_eq!(record.options.archive_previous, previous_kind == PreviousKind::Active);
        assert_eq!(record.options.run_system_triggers, run_system_triggers);
        assert_eq!(record.options.run_boot_sync, run_boot_sync);
    }

    let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::SynthesizedEmpty);
    assert!(matches!(
        identity.begin_transition(StatefulTransitionRequest::NewState {
            previous: NewStatePrevious::Unmanaged,
            run_system_triggers: false,
            run_boot_sync: false,
        }),
        Err(StatefulTransitionCoordinatorError::UnmanagedPreviousUnsupported)
    ));
    assert_canonical_journal_absent(&fixture.installation.root);
}

#[test]
fn journal_coordinator_archived_activation_reaches_candidate_prepared_without_allocation_phases() {
    let (fixture, identity) = fixture(CandidateKind::Archived, PreviousKind::Active);
    assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
    let coordinator = identity
        .begin_transition(request(CandidateKind::Archived, &fixture, false, true))
        .unwrap();
    let preparing = coordinator.record().clone();
    assert_record_prefix(&preparing, Operation::ActivateArchived, Phase::Preparing, 1);
    assert!(matches!(
        coordinator.transition_id_for_allocation(),
        Err(StatefulTransitionCoordinatorError::UnexpectedOperation {
            expected: Operation::NewState,
            actual: Operation::ActivateArchived,
            ..
        })
    ));
    assert_eq!(read_canonical(&fixture.installation.root), preparing);
    assert_eq!(preparing.candidate.id, Some(i32::from(fixture.candidate_state)));
    assert_eq!(preparing.candidate.origin, CandidateOrigin::Archived);
    assert_eq!(preparing.previous.id, Some(i32::from(fixture.previous_state)));
    assert_eq!(preparing.previous.origin, PreviousOrigin::ActiveState);
    assert_eq!(
        fixture
            .database
            .transition_ownership(fixture.candidate_state, &preparing.transition_id)
            .unwrap(),
        TransitionOwnership::Cleared
    );

    let coordinator = coordinator.begin_candidate_prepare().unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::ActivateArchived,
        Phase::CandidatePrepareStarted,
        2,
    );
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::ActivateArchived,
        Phase::CandidatePrepared,
        3,
    );
    assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
    assert_candidate_metadata(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reaches_candidate_prepared_without_allocation_phases() {
    let (fixture, identity) = fixture(CandidateKind::ActiveReblit, PreviousKind::Active);
    let coordinator = identity
        .begin_transition(request(CandidateKind::ActiveReblit, &fixture, true, false))
        .unwrap();
    let preparing = coordinator.record().clone();
    assert_record_prefix(&preparing, Operation::ActiveReblit, Phase::Preparing, 1);
    assert!(matches!(
        coordinator.transition_id_for_allocation(),
        Err(StatefulTransitionCoordinatorError::UnexpectedOperation {
            expected: Operation::NewState,
            actual: Operation::ActiveReblit,
            ..
        })
    ));
    assert_eq!(read_canonical(&fixture.installation.root), preparing);
    assert_eq!(preparing.candidate.id, Some(i32::from(fixture.previous_state)));
    assert_eq!(preparing.previous.id, Some(i32::from(fixture.previous_state)));
    assert_eq!(preparing.candidate.origin, CandidateOrigin::ActiveReblit);
    assert_eq!(preparing.previous.origin, PreviousOrigin::ActiveReblitCorrupt);
    assert!(!preparing.options.archive_previous);
    assert_ne!(preparing.candidate.tree_token, preparing.previous.tree_token);
    assert_ne!(
        preparing.candidate.usr_runtime_identity,
        preparing.previous.usr_runtime_identity
    );

    let coordinator = coordinator.begin_candidate_prepare().unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::ActiveReblit,
        Phase::CandidatePrepareStarted,
        2,
    );
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::ActiveReblit,
        Phase::CandidatePrepared,
        3,
    );
    assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
    assert_candidate_metadata(&fixture);
}

#[test]
fn journal_coordinator_creation_captures_exact_epoch_tokens_and_runtime_tree_witnesses() {
    let (fixture, identity) = fixture(CandidateKind::Archived, PreviousKind::Active);
    let expected_epoch = RuntimeEpoch::capture().unwrap();
    let expected_candidate =
        RuntimeTreeIdentity::capture_directory(identity.candidate.store.retained_directory()).unwrap();
    let expected_previous =
        RuntimeTreeIdentity::capture_directory(identity.previous.store.retained_directory()).unwrap();
    let expected_candidate_token = identity.candidate.marker.token().clone();
    let expected_previous_token = identity.previous.marker.token().clone();

    let coordinator = identity
        .begin_transition(request(CandidateKind::Archived, &fixture, true, true))
        .unwrap();
    let preparing = coordinator.record().clone();
    assert_eq!(preparing.creation_epoch, expected_epoch);
    assert_eq!(preparing.candidate.tree_token, expected_candidate_token);
    assert_eq!(preparing.previous.tree_token, expected_previous_token);
    assert_eq!(preparing.candidate.usr_runtime_identity, expected_candidate);
    assert_eq!(preparing.previous.usr_runtime_identity, expected_previous);

    let coordinator = coordinator.begin_candidate_prepare().unwrap();
    let coordinator = finish_candidate_prepare(coordinator).unwrap();
    let prepared = coordinator.record();
    assert_eq!(prepared.creation_epoch, preparing.creation_epoch);
    assert_eq!(prepared.candidate.tree_token, preparing.candidate.tree_token);
    assert_eq!(prepared.previous.tree_token, preparing.previous.tree_token);
    assert_eq!(
        prepared.candidate.usr_runtime_identity,
        preparing.candidate.usr_runtime_identity
    );
    assert_eq!(
        prepared.previous.usr_runtime_identity,
        preparing.previous.usr_runtime_identity
    );
}

#[test]
fn journal_coordinator_quarantine_name_is_fixed_transition_token_evidence() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, identity) = fixture(candidate_kind, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(candidate_kind, &fixture, false, false))
            .unwrap();
        let expected = format!("failed-transition-{}", coordinator.record().transition_id);
        assert_eq!(coordinator.record().quarantine_name.as_str(), expected);

        if candidate_kind == CandidateKind::NewState {
            let coordinator = coordinator.begin_fresh_allocation().unwrap();
            let allocated = allocate_matching_state(&fixture, &coordinator);
            let coordinator = coordinator
                .finish_fresh_allocation(&fixture.database, allocated)
                .unwrap();
            assert_eq!(coordinator.record().quarantine_name.as_str(), expected);
            assert_eq!(coordinator.record().candidate.id, Some(i32::from(allocated)));
        }
    }
}

#[test]
fn journal_coordinator_wrong_operation_or_phase_is_rejected_without_record_change() {
    {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let database = db::state::Database::new(":memory:").unwrap();
        let candidate = installation.staging_path("usr");
        create_canonical_directory(&candidate);
        let state_id = candidate.join(".stateID");
        write_canonical_file(&state_id, b"77");
        let before = fs::symlink_metadata(&state_id).unwrap();

        assert!(StatefulTreeIdentity::prepare_unallocated_candidate(
            &installation,
            &database,
            &candidate,
        )
        .is_err());
        let after = fs::symlink_metadata(&state_id).unwrap();
        assert_eq!(fs::read(&state_id).unwrap(), b"77");
        assert_eq!(
            (after.dev(), after.ino(), after.mode(), after.nlink(), after.len()),
            (before.dev(), before.ino(), before.mode(), before.nlink(), before.len())
        );
        assert!(matches!(
            fs::symlink_metadata(candidate.join(".cast-tree-id")),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert_canonical_journal_absent(&installation.root);
    }
    {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let database = db::state::Database::new(":memory:").unwrap();
        let candidate = installation.staging_path("usr");
        create_canonical_directory(&candidate);
        let temporary_state_id = candidate.join(".cast-state-id.tmp");
        fs::write(&temporary_state_id, b"residue").unwrap();
        fs::set_permissions(&temporary_state_id, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::symlink_metadata(&temporary_state_id).unwrap();

        assert!(StatefulTreeIdentity::prepare_unallocated_candidate(
            &installation,
            &database,
            &candidate,
        )
        .is_err());
        let after = fs::symlink_metadata(&temporary_state_id).unwrap();
        assert_eq!(fs::read(&temporary_state_id).unwrap(), b"residue");
        assert_eq!(
            (after.dev(), after.ino(), after.mode(), after.nlink(), after.len()),
            (before.dev(), before.ino(), before.mode(), before.nlink(), before.len())
        );
        assert!(matches!(
            fs::symlink_metadata(candidate.join(".cast-tree-id")),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert_canonical_journal_absent(&installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let wrong = state::Id::from(i32::from(fixture.previous_state) + 1_000);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::NewState {
                previous: NewStatePrevious::Active(wrong),
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: Operation::NewState,
                request_origin: PreviousOrigin::ActiveState,
                request_state: Some(request),
                retained_origin: PreviousOrigin::ActiveState,
                retained_state: Some(retained),
            }) if request == i32::from(wrong) && retained == i32::from(fixture.previous_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::NewState {
                previous: NewStatePrevious::SynthesizedEmpty,
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: Operation::NewState,
                request_origin: PreviousOrigin::SynthesizedEmpty,
                request_state: None,
                retained_origin: PreviousOrigin::ActiveState,
                retained_state: Some(retained),
            }) if retained == i32::from(fixture.previous_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::NewState {
                previous: NewStatePrevious::Unmanaged,
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::UnmanagedPreviousUnsupported)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::SynthesizedEmpty);
        let claimed = state::Id::from(1);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::NewState {
                previous: NewStatePrevious::Active(claimed),
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: Operation::NewState,
                request_origin: PreviousOrigin::ActiveState,
                request_state: Some(request),
                retained_origin: PreviousOrigin::SynthesizedEmpty,
                retained_state: None,
            }) if request == i32::from(claimed)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::SynthesizedEmpty);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::NewState {
                previous: NewStatePrevious::Unmanaged,
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::UnmanagedPreviousUnsupported)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::Archived, PreviousKind::Active);
        let wrong = state::Id::from(i32::from(fixture.previous_state) + 1_000);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::ActivateArchived {
                candidate: fixture.candidate_state,
                previous: wrong,
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: Operation::ActivateArchived,
                request_origin: PreviousOrigin::ActiveState,
                request_state: Some(request),
                retained_origin: PreviousOrigin::ActiveState,
                retained_state: Some(retained),
            }) if request == i32::from(wrong) && retained == i32::from(fixture.previous_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::ActiveReblit, PreviousKind::Active);
        let wrong = state::Id::from(i32::from(fixture.candidate_state) + 1_000);
        assert!(matches!(
            identity.begin_transition(StatefulTransitionRequest::ActiveReblit {
                state: wrong,
                run_system_triggers: false,
                run_boot_sync: false,
            }),
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: Operation::ActiveReblit,
                request_origin: PreviousOrigin::ActiveReblitCorrupt,
                request_state: Some(request),
                retained_origin: PreviousOrigin::ActiveState,
                retained_state: Some(retained),
            }) if request == i32::from(wrong) && retained == i32::from(fixture.previous_state)
        ));
        assert_canonical_journal_absent(&fixture.installation.root);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap();
        let before = coordinator.record().clone();
        assert!(matches!(
            coordinator.begin_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::UnexpectedPhase {
                expected: Phase::FreshStateAllocated,
                actual: Phase::Preparing,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::Archived, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::Archived, &fixture, false, false))
            .unwrap();
        let before = coordinator.record().clone();
        assert!(matches!(
            coordinator.begin_fresh_allocation(),
            Err(StatefulTransitionCoordinatorError::UnexpectedOperation {
                expected: Operation::NewState,
                actual: Operation::ActivateArchived,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap();
        let before = coordinator.record().clone();
        assert!(matches!(
            coordinator.finish_fresh_allocation(&fixture.database, fixture.candidate_state),
            Err(StatefulTransitionCoordinatorError::UnexpectedPhase {
                expected: Phase::FreshStateAllocating,
                actual: Phase::Preparing,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let before = coordinator.record().clone();
        assert!(matches!(
            coordinator.begin_fresh_allocation(),
            Err(StatefulTransitionCoordinatorError::UnexpectedPhase {
                expected: Phase::Preparing,
                actual: Phase::FreshStateAllocating,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
    }
    {
        let (fixture, identity) = fixture(CandidateKind::ActiveReblit, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::ActiveReblit, &fixture, false, false))
            .unwrap()
            .begin_candidate_prepare()
            .unwrap();
        let before = coordinator.record().clone();
        assert!(matches!(
            coordinator.begin_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::UnexpectedPhase {
                expected: Phase::Preparing,
                actual: Phase::CandidatePrepareStarted,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
    }
}
