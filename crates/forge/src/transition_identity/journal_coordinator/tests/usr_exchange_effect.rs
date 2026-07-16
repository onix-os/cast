fn coordinator_ready_for_usr_exchange_effect(
    candidate_kind: CandidateKind,
) -> (
    CoordinatorFixture,
    UsrExchangeIntentCoordinator,
    JournalUsrExchangeAuthority,
) {
    coordinator_ready_for_usr_exchange_effect_with_previous(candidate_kind, PreviousKind::Active)
}

fn coordinator_ready_for_usr_exchange_effect_with_previous(
    candidate_kind: CandidateKind,
    previous_kind: PreviousKind,
) -> (
    CoordinatorFixture,
    UsrExchangeIntentCoordinator,
    JournalUsrExchangeAuthority,
) {
    let (fixture, identity, authority) = fixture_with_exchange_authority(candidate_kind, previous_kind);
    coordinator_from_exchange_fixture(candidate_kind, fixture, identity, authority)
}

fn coordinator_from_exchange_fixture(
    candidate_kind: CandidateKind,
    fixture: CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
) -> (
    CoordinatorFixture,
    UsrExchangeIntentCoordinator,
    JournalUsrExchangeAuthority,
) {
    let mut coordinator = identity
        .begin_transition(request(candidate_kind, &fixture, false, false))
        .unwrap();
    if candidate_kind == CandidateKind::NewState {
        coordinator = coordinator.begin_fresh_allocation().unwrap();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        coordinator = coordinator
            .finish_fresh_allocation(&fixture.database, allocated)
            .unwrap();
    }
    coordinator = coordinator.begin_candidate_prepare().unwrap();
    let prepared = finish_candidate_prepare(coordinator).unwrap();
    let ready = match prepared {
        PreparedStatefulTransitionCoordinator::Archived(ready) => {
            TestUsrExchangeReady::Archived(ready)
        }
        PreparedStatefulTransitionCoordinator::TransactionTriggers(ready) => {
            let complete = ready
                .run_transaction_triggers(|_| Ok::<(), TriggerEffectError>(()))
                .unwrap();
            TestUsrExchangeReady::TransactionTriggers(complete)
        }
    };
    (fixture, ready.begin().unwrap(), authority)
}

fn expected_usr_exchanged_generation(candidate_kind: CandidateKind) -> u64 {
    match candidate_kind {
        CandidateKind::NewState => 9,
        CandidateKind::Archived => 5,
        CandidateKind::ActiveReblit => 7,
    }
}

fn assert_root_links_absent(fixture: &CoordinatorFixture) {
    for name in ["bin", "sbin", "lib", "lib32", "lib64"] {
        assert_state_metadata_name_absent(&fixture.installation.root.join(name));
    }
}

fn assert_exchange_layout(fixture: &CoordinatorFixture, candidate_live: bool, candidate: (u64, u64), previous: (u64, u64)) {
    let live = directory_identity(&fixture.installation.root.join("usr"));
    let staged = directory_identity(&fixture.candidate_path);
    if candidate_live {
        assert_eq!((live, staged), (candidate, previous));
    } else {
        assert_eq!((live, staged), (previous, candidate));
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_applies_once_for_every_operation_without_root_links() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(candidate_kind);
        let intent_record = intent.record().clone();
        let candidate = directory_identity(&fixture.candidate_path);
        let previous = directory_identity(&fixture.installation.root.join("usr"));
        reset_retained_exchange_syscall_count();

        let exchanged = intent.execute_usr_exchange(authority).unwrap();

        assert_record_prefix(
            exchanged.record(),
            intent_record.operation,
            Phase::UsrExchanged,
            expected_usr_exchanged_generation(candidate_kind),
        );
        assert_eq!(read_canonical(&fixture.installation.root), *exchanged.record());
        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_exchange_layout(&fixture, true, candidate, previous);
        exchanged.revalidate_retained_authorities().unwrap();
        assert_root_links_absent(&fixture);
    }
}

#[test]
fn journal_coordinator_new_state_synthesized_empty_exchange_applies_once_and_retains_empty_previous() {
    let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect_with_previous(
        CandidateKind::NewState,
        PreviousKind::SynthesizedEmpty,
    );
    let intent_record = intent.record().clone();
    assert_eq!(intent_record.previous.origin, PreviousOrigin::SynthesizedEmpty);
    assert_eq!(intent_record.previous.id, None);
    let candidate = directory_identity(&fixture.candidate_path);
    let previous = directory_identity(&fixture.installation.root.join("usr"));
    assert_state_metadata_name_absent(&fixture.installation.root.join("usr/.stateID"));
    reset_retained_exchange_syscall_count();

    let exchanged = intent.execute_usr_exchange(authority).unwrap();

    assert_record_prefix(exchanged.record(), Operation::NewState, Phase::UsrExchanged, 9);
    assert_eq!(read_canonical(&fixture.installation.root), *exchanged.record());
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_exchange_layout(&fixture, true, candidate, previous);
    assert_state_metadata_name_absent(&fixture.candidate_path.join(".stateID"));
    exchanged.revalidate_retained_authorities().unwrap();
    assert_root_links_absent(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_exchange_preserves_exact_two_link_slot_without_rotation_or_parking() {
    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let wrapper = fixture.installation.root_path(fixture.previous_state.to_string());
    let slot = fs::read_dir(&wrapper).unwrap().next().unwrap().unwrap().path();
    let wrapper_before = directory_identity(&wrapper);
    let slot_before = fs::symlink_metadata(&slot).unwrap();
    let (fixture, intent, authority) =
        coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
    reset_retained_exchange_syscall_count();

    let exchanged = intent.execute_usr_exchange(authority).unwrap();

    assert_eq!(exchanged.record().phase, Phase::UsrExchanged);
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_eq!(directory_identity(&wrapper), wrapper_before);
    let slot_after = fs::symlink_metadata(&slot).unwrap();
    assert_eq!((slot_after.dev(), slot_after.ino()), (slot_before.dev(), slot_before.ino()));
    assert_eq!(slot_after.nlink(), 2);
    let root_names = fs::read_dir(fixture.installation.root_path(""))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(root_names.iter().all(|name| !name.starts_with(".archived-candidate-slot-")));
    assert_root_links_absent(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_slot_and_state_substitution_stop_before_exchange() {
    {
        let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
        let wrapper = fixture.installation.root_path(fixture.previous_state.to_string());
        let displaced = fixture.installation.root_path("slot-wrapper.displaced");
        let hook_wrapper = wrapper.clone();
        let hook_displaced = displaced.clone();
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
        let intent_record = intent.record().clone();
        arm_before_retained_exchange_rename(move || {
            fs::rename(&hook_wrapper, &hook_displaced).unwrap();
            fs::create_dir(&hook_wrapper).unwrap();
            fs::set_permissions(&hook_wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        });
        reset_retained_exchange_syscall_count();

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(
            failure,
            UsrExchangeEffectFailure::Exchange {
                outcome: RetainedExchangeOutcome::NotApplied,
                ..
            }
        ));
        assert_eq!(retained_exchange_syscall_count(), 0);
        assert_eq!(read_canonical(&fixture.installation.root), intent_record);
        assert!(displaced.is_dir());
    }

    {
        let (fixture, intent, authority) =
            coordinator_ready_for_usr_exchange_effect(CandidateKind::ActiveReblit);
        let intent_record = intent.record().clone();
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        arm_before_retained_exchange_rename(move || database.remove(&candidate).unwrap());
        reset_retained_exchange_syscall_count();

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(
            failure,
            UsrExchangeEffectFailure::Exchange {
                outcome: RetainedExchangeOutcome::NotApplied,
                ..
            }
        ));
        assert_eq!(retained_exchange_syscall_count(), 0);
        assert_eq!(read_canonical(&fixture.installation.root), intent_record);
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_raw_result_matrix_never_retries() {
    for (fault, applied) in [
        (RetainedExchangeSyscallFault::ErrorWithoutApply, false),
        (RetainedExchangeSyscallFault::SuccessWithoutApply, false),
        (RetainedExchangeSyscallFault::ErrorAfterApply, true),
    ] {
        let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::Archived);
        let intent_record = intent.record().clone();
        let candidate = directory_identity(&fixture.candidate_path);
        let previous = directory_identity(&fixture.installation.root.join("usr"));
        arm_retained_exchange_syscall_fault(fault);

        let result = intent.execute_usr_exchange(authority);

        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_exchange_layout(&fixture, applied, candidate, previous);
        assert_root_links_absent(&fixture);
        if applied {
            let exchanged = result.unwrap();
            assert_eq!(exchanged.record().phase, Phase::UsrExchanged);
        } else {
            assert!(matches!(
                result,
                Err(UsrExchangeEffectFailure::Exchange {
                    outcome: RetainedExchangeOutcome::NotApplied,
                    ..
                })
            ));
            assert_eq!(read_canonical(&fixture.installation.root), intent_record);
        }
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry() {
    for point in [
        RetainedExchangeFaultPoint::StagingParentSync,
        RetainedExchangeFaultPoint::InstallationRootSync,
        RetainedExchangeFaultPoint::FinalRevalidation,
    ] {
        let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::NewState);
        let intent_record = intent.record().clone();
        let candidate = directory_identity(&fixture.candidate_path);
        let previous = directory_identity(&fixture.installation.root.join("usr"));
        reset_retained_exchange_syscall_count();
        arm_retained_exchange_fault(point);

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(
            failure,
            UsrExchangeEffectFailure::Exchange {
                outcome: RetainedExchangeOutcome::Applied,
                ..
            }
        ));
        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_exchange_layout(&fixture, true, candidate, previous);
        assert_eq!(read_canonical(&fixture.installation.root), intent_record);
        assert_root_links_absent(&fixture);
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_reconciles_foreign_post_syscall_layout_as_ambiguous() {
    let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::Archived);
    let intent_record = intent.record().clone();
    let live = fixture.installation.root.join("usr");
    let parked = fixture.installation.root.join("usr.applied-candidate");
    let candidate = directory_identity(&fixture.candidate_path);
    let previous = directory_identity(&live);
    let hook_live = live.clone();
    let hook_parked = parked.clone();
    arm_after_retained_exchange_rename(move || {
        fs::rename(&hook_live, &hook_parked).unwrap();
        create_canonical_directory(&hook_live);
    });
    reset_retained_exchange_syscall_count();

    let failure = intent.execute_usr_exchange(authority).unwrap_err();

    assert!(matches!(
        failure,
        UsrExchangeEffectFailure::Exchange {
            outcome: RetainedExchangeOutcome::Ambiguous,
            ..
        }
    ));
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_eq!(read_canonical(&fixture.installation.root), intent_record);
    assert_eq!(directory_identity(&parked), candidate);
    assert_eq!(directory_identity(&fixture.candidate_path), previous);
    assert_ne!(directory_identity(&live), candidate);
    assert_ne!(directory_identity(&live), previous);
    assert_root_links_absent(&fixture);
}

#[test]
fn journal_coordinator_usr_exchange_effect_repeats_full_proof_immediately_before_syscall() {
    for mutation in [
        "journal",
        "root",
        "cast",
        "staging-wrapper",
        "candidate-marker",
        "candidate-state-id",
        "candidate-identities",
        "previous-marker",
        "metadata",
        "provenance",
        "database",
        "lease",
        "root-abi",
    ] {
        let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::NewState);
        let intent_record = intent.record().clone();
        let candidate = state::Id::from(intent_record.candidate.id.unwrap());
        let root = fixture.installation.root.clone();
        let database = fixture.database.clone();
        let transition = intent_record.transition_id.clone();
        let metadata_path = fixture.candidate_path.join("lib/os-release");
        let candidate_path = fixture.candidate_path.clone();
        arm_before_retained_exchange_rename(move || match mutation {
            "journal" => fs::write(canonical_journal(&root), b"corrupt journal").unwrap(),
            "root" => {
                let mode = fs::symlink_metadata(&root).unwrap().permissions().mode() & 0o7777;
                fs::set_permissions(
                    &root,
                    fs::Permissions::from_mode(if mode == 0o755 { 0o700 } else { 0o755 }),
                )
                .unwrap();
            }
            "cast" => {
                fs::rename(root.join(".cast"), root.join(".cast.effect-displaced")).unwrap();
                create_canonical_directory(&root.join(".cast"));
            }
            "staging-wrapper" => {
                let staging = candidate_path.parent().unwrap();
                fs::rename(staging, staging.with_file_name("staging.effect-displaced")).unwrap();
                fs::create_dir(staging).unwrap();
                fs::set_permissions(staging, fs::Permissions::from_mode(0o700)).unwrap();
            }
            "candidate-marker" => replace_file_with_same_bytes(
                &candidate_path.join(".cast-tree-id"),
                ".cast-tree-id.effect-displaced",
            ),
            "candidate-state-id" => replace_file_with_same_bytes(
                &candidate_path.join(".stateID"),
                ".stateID.effect-displaced",
            ),
            "candidate-identities" => {
                replace_file_with_same_bytes(&candidate_path.join(".cast-tree-id"), ".cast-tree-id.effect-displaced");
                replace_file_with_same_bytes(&candidate_path.join(".stateID"), ".stateID.effect-displaced");
            }
            "previous-marker" => replace_file_with_same_bytes(
                &root.join("usr/.cast-tree-id"),
                ".cast-tree-id.effect-live-displaced",
            ),
            "metadata" => replace_file_with_same_bytes(&metadata_path, "os-release.effect-displaced"),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "database" => database.clear_transition_if_matches(candidate, &transition).unwrap(),
            "lease" => replace_file_with_same_bytes(
                &root.join("usr/.stateID"),
                ".stateID.effect-live-displaced",
            ),
            "root-abi" => std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap(),
            _ => unreachable!(),
        });
        reset_retained_exchange_syscall_count();

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(
            failure,
            UsrExchangeEffectFailure::Exchange {
                outcome: RetainedExchangeOutcome::NotApplied,
                ..
            }
        ));
        assert_eq!(retained_exchange_syscall_count(), 0, "mutation={mutation}");
        if mutation == "root-abi" {
            assert!(fixture.installation.root.join("bin").is_symlink());
        } else {
            assert_root_links_absent(&fixture);
        }
        if mutation == "cast" {
            assert_canonical_journal_absent(&fixture.installation.root);
            let displaced = fixture
                .installation
                .root
                .join(".cast.effect-displaced/journal/state-transition");
            assert_eq!(decode(&fs::read(displaced).unwrap()).unwrap(), intent_record);
        } else if mutation != "journal" {
            assert_eq!(read_canonical(&fixture.installation.root), intent_record);
        }
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_post_apply_metadata_substitution_is_fail_stop() {
    let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::Archived);
    let intent_record = intent.record().clone();
    let live_release = fixture.installation.root.join("usr/lib/os-release");
    let displaced = fixture.installation.root.join("os-release.applied-displaced");
    arm_after_retained_exchange_rename(move || {
        let bytes = fs::read(&live_release).unwrap();
        fs::rename(&live_release, &displaced).unwrap();
        write_canonical_file(&live_release, &bytes);
    });
    reset_retained_exchange_syscall_count();

    let failure = intent.execute_usr_exchange(authority).unwrap_err();

    assert!(matches!(failure, UsrExchangeEffectFailure::PostEffectEvidence { .. }));
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert_eq!(read_canonical(&fixture.installation.root), intent_record);
    assert_root_links_absent(&fixture);
}

#[test]
fn journal_coordinator_usr_exchange_effect_post_apply_authority_failures_remain_at_intent() {
    for mutation in ["database", "provenance", "root-abi", "active-reblit-state", "second-link"] {
        let (fixture, identity, authority) = if mutation == "second-link" {
            fixture_with_exchange_authority_and_previous_slot()
        } else {
            let kind = if mutation == "active-reblit-state" {
                CandidateKind::ActiveReblit
            } else {
                CandidateKind::NewState
            };
            fixture_with_exchange_authority(kind, PreviousKind::Active)
        };
        let kind = if matches!(mutation, "active-reblit-state" | "second-link") {
            CandidateKind::ActiveReblit
        } else {
            CandidateKind::NewState
        };
        let wrapper = fixture.installation.root_path(fixture.previous_state.to_string());
        let displaced_wrapper = fixture.installation.root_path("post-effect-slot.displaced");
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        let root = fixture.installation.root.clone();
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(kind, fixture, identity, authority);
        let intent_record = intent.record().clone();
        arm_after_retained_exchange_rename(move || match mutation {
            "database" => database
                .clear_transition_if_matches(candidate, &intent_record.transition_id)
                .unwrap(),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "root-abi" => std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap(),
            "active-reblit-state" => database.remove(&candidate).unwrap(),
            "second-link" => {
                fs::rename(&wrapper, &displaced_wrapper).unwrap();
                fs::create_dir(&wrapper).unwrap();
                fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
            }
            _ => unreachable!(),
        });
        reset_retained_exchange_syscall_count();

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(failure, UsrExchangeEffectFailure::PostEffectEvidence { .. }));
        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_eq!(read_canonical(&fixture.installation.root).phase, Phase::UsrExchangeIntent);
        if mutation == "root-abi" {
            assert!(fixture.installation.root.join("bin").is_symlink());
        } else {
            assert_root_links_absent(&fixture);
        }
    }
}

#[test]
fn journal_coordinator_usr_exchange_effect_completion_faults_leave_intent_or_exchanged_after_one_call() {
    for after_commit in [false, true] {
        let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::ActiveReblit);
        if after_commit {
            crate::transition_journal::arm_next_update_first_directory_sync_fault();
        } else {
            crate::transition_journal::arm_next_temporary_sync_fault();
        }
        reset_retained_exchange_syscall_count();

        let failure = intent.execute_usr_exchange(authority).unwrap_err();

        assert!(matches!(failure, UsrExchangeEffectFailure::CompletionPersistence { .. }));
        assert_eq!(retained_exchange_syscall_count(), 1);
        assert_eq!(
            read_canonical(&fixture.installation.root).phase,
            if after_commit { Phase::UsrExchanged } else { Phase::UsrExchangeIntent }
        );
        assert_root_links_absent(&fixture);
    }
}

#[test]
fn journal_coordinator_usr_exchange_authority_is_writer_first_and_never_waits_behind_journal() {
    let (fixture, identity) = fixture(CandidateKind::Archived, PreviousKind::Active);
    let installation = fixture.installation.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&installation, None);
        sender.send(result.is_err()).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(true),
        "pre-journal authority waited behind StatefulTreeIdentity's retained journal lock"
    );
    worker.join().unwrap();

    let coordinator = identity
        .begin_transition(request(CandidateKind::Archived, &fixture, false, false))
        .unwrap();
    drop(coordinator);
    let error = JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&fixture.installation, None)
        .unwrap_err();
    assert!(matches!(error, crate::client::JournalUsrExchangeAuthorityError::UnresolvedJournal { .. }));
}

#[test]
fn journal_coordinator_usr_exchange_identity_handoff_fails_bounded_when_contender_wins_journal_gap() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(":memory:").unwrap();
    let preflight =
        JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&installation, None).unwrap();
    let candidate_path = installation.staging_path("usr");
    create_canonical_directory(&candidate_path);
    write_canonical_file(&candidate_path.join("payload-sentinel"), NEW_STATE_PAYLOAD_SENTINEL);

    // This owner acquires the journal after writer-first preflight released its
    // absence probe but before identity preparation attempts the handoff.
    let contender_root = installation.root.clone();
    let (acquired_sender, acquired_receiver) = std::sync::mpsc::sync_channel(1);
    let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
    let contender = std::thread::spawn(move || {
        let journal = TransitionJournalStore::open(&contender_root).unwrap();
        acquired_sender.send(()).unwrap();
        let _release = release_receiver.recv_timeout(std::time::Duration::from_secs(2));
        drop(journal);
    });
    assert_eq!(
        acquired_receiver.recv_timeout(std::time::Duration::from_secs(2)),
        Ok(()),
        "journal contender did not acquire the handoff gap"
    );
    let error = preflight
        .prepare_unallocated_candidate(&database, &candidate_path)
        .unwrap_err();

    assert!(matches!(
        error,
        crate::client::JournalUsrExchangeAuthorityError::Identity(
            crate::transition_identity::Error::Journal(
                crate::transition_journal::StorageError::AcquireLock { .. }
            )
        )
    ));
    assert_state_metadata_name_absent(&candidate_path.join(".cast-tree-id"));
    release_sender.send(()).unwrap();
    contender.join().unwrap();

    let retry_preflight =
        JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&installation, None).unwrap();
    let (identity, authority) = retry_preflight
        .prepare_unallocated_candidate(&database, &candidate_path)
        .unwrap();
    drop(identity);
    drop(authority);
}
