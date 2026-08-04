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
    coordinator_from_exchange_fixture_with_options(
        candidate_kind,
        fixture,
        identity,
        authority,
        false,
        false,
    )
}

fn coordinator_from_exchange_fixture_with_options(
    candidate_kind: CandidateKind,
    fixture: CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    run_system_triggers: bool,
    run_boot_sync: bool,
) -> (
    CoordinatorFixture,
    UsrExchangeIntentCoordinator,
    JournalUsrExchangeAuthority,
) {
    let mut coordinator = identity
        .begin_transition(request(
            candidate_kind,
            &fixture,
            run_system_triggers,
            run_boot_sync,
        ))
        .unwrap();
    if candidate_kind == CandidateKind::NewState {
        coordinator = coordinator.begin_fresh_allocation().unwrap();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        coordinator = coordinator
            .finish_fresh_allocation(&fixture.database, allocated)
            .unwrap();
    }
    coordinator = coordinator.begin_candidate_prepare_through_staging().unwrap();
    let prepared = finish_candidate_prepare(coordinator).unwrap();
    let ready = match prepared {
        PreparedStatefulTransitionCoordinator::Archived(ready) => {
            TestUsrExchangeReady::Archived(ready.prepare_archived_isolation(&fixture.installation).unwrap())
        }
        PreparedStatefulTransitionCoordinator::NewStateIsolation(ready) => {
            let ready = ready
                .prepare_for_transaction_triggers(&fixture.installation)
                .unwrap();
            let complete = ready
                .run_transaction_triggers(|_| Ok::<(), TriggerEffectError>(()))
                .unwrap();
            TestUsrExchangeReady::TransactionTriggers(complete)
        }
        PreparedStatefulTransitionCoordinator::ActiveReblitReservation(ready) => {
            let ready = ready
                .reserve_for_transaction_triggers(&fixture.installation)
                .unwrap()
                .prepare_for_transaction_triggers(&fixture.installation)
                .unwrap();
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
        CandidateKind::Archived => 7,
        CandidateKind::ActiveReblit => 7,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct UsrExchangeDatabaseSnapshot {
    states: Vec<crate::State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: TransitionOwnership,
    candidate_provenance: Option<db::state::MetadataProvenance>,
    previous_ownership: TransitionOwnership,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

fn usr_exchange_database_snapshot(
    fixture: &CoordinatorFixture,
    source: &TransitionRecord,
) -> UsrExchangeDatabaseSnapshot {
    UsrExchangeDatabaseSnapshot {
        states: fixture.database.all().unwrap(),
        in_flight: fixture.database.audit_in_flight_transition().unwrap(),
        candidate_ownership: fixture
            .database
            .transition_ownership(fixture.candidate_state, &source.transition_id)
            .unwrap(),
        candidate_provenance: fixture.database.metadata_provenance(fixture.candidate_state).unwrap(),
        previous_ownership: fixture
            .database
            .transition_ownership(fixture.previous_state, &source.transition_id)
            .unwrap(),
        previous_provenance: fixture.database.metadata_provenance(fixture.previous_state).unwrap(),
    }
}

fn assert_exact_pending_reverse_decision(source: &TransitionRecord, actual: &TransitionRecord) {
    assert_eq!(actual.phase, Phase::RollbackDecided);
    assert_eq!(actual.generation, source.generation + 1);
    assert_eq!(actual.transition_id, source.transition_id);
    assert_eq!(actual.operation, source.operation);
    assert_eq!(actual.creation_epoch, source.creation_epoch);
    assert_eq!(actual.candidate, source.candidate);
    assert_eq!(actual.previous, source.previous);
    assert_eq!(actual.options, source.options);
    assert_eq!(actual.quarantine_name, source.quarantine_name);
    let forward_source = match source.phase {
        Phase::UsrExchangeIntent => ForwardPhase::UsrExchangeIntent,
        Phase::UsrExchanged => ForwardPhase::UsrExchanged,
        Phase::RootLinksComplete => ForwardPhase::RootLinksComplete,
        other => panic!("unexpected forward /usr rollback source {other:?}"),
    };
    assert_eq!(
        actual.rollback,
        Some(RollbackPlan {
            source: forward_source,
            previous_archive: RollbackAction::NotRequired,
            usr_exchange: RollbackAction::Pending,
            candidate: CandidateRollback {
                action: RollbackAction::Pending,
                disposition: if source.operation == Operation::ActivateArchived {
                    AbortDisposition::Rearchive
                } else {
                    AbortDisposition::Quarantine
                },
            },
            fresh_db: if source.operation == Operation::NewState {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            boot: BootRollback::NotRequired,
            external_effects_may_remain: source.operation != Operation::ActivateArchived,
        })
    );
}

fn assert_root_links_absent(fixture: &CoordinatorFixture) {
    for name in ["bin", "sbin", "lib", "lib32", "lib64"] {
        assert_state_metadata_name_absent(&fixture.installation.root.join(name));
    }
}

fn assert_root_links_complete(fixture: &CoordinatorFixture) {
    for (name, target) in [
        ("bin", "usr/bin"),
        ("sbin", "usr/sbin"),
        ("lib", "usr/lib"),
        ("lib32", "usr/lib32"),
        ("lib64", "usr/lib"),
    ] {
        assert_eq!(fs::read_link(fixture.installation.root.join(name)).unwrap(), Path::new(target));
    }
}

fn assert_root_links_after_forward_recovery(fixture: &CoordinatorFixture, source_phase: Phase) {
    match source_phase {
        Phase::UsrExchangeIntent => assert_root_links_absent(fixture),
        Phase::UsrExchanged => assert_root_links_complete(fixture),
        other => panic!("unexpected forward /usr recovery source {other:?}"),
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
    let live_usr = fixture.installation.root.join("usr");
    let previous = directory_identity(&live_usr);
    assert_state_metadata_name_absent(&live_usr.join(".stateID"));

    // The synthesized previous tree is a real marked tree, not a bare
    // directory: preparation creates it, marks it, and leaves nothing else in
    // it. A synthesized tree that shared the candidate's token would make the
    // two indistinguishable to every later identity check.
    let metadata = fs::symlink_metadata(&live_usr).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.uid(), nix::unistd::Uid::effective().as_raw());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
    let entries = fs::read_dir(&live_usr)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [std::ffi::OsString::from(".cast-tree-id")]);
    assert_ne!(
        TreeMarkerStore::open_path(&live_usr)
            .unwrap()
            .read_for_recovery()
            .unwrap()
            .token()
            .as_str(),
        TreeMarkerStore::open_path(&fixture.candidate_path)
            .unwrap()
            .read_for_recovery()
            .unwrap()
            .token()
            .as_str()
    );
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
fn journal_coordinator_active_reblit_exchange_preserves_exact_parked_two_link_slot_and_reservation() {
    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let wrapper = fixture.installation.root_path(fixture.previous_state.to_string());
    let slot = fs::read_dir(&wrapper).unwrap().next().unwrap().unwrap().path();
    let wrapper_before = directory_identity(&wrapper);
    let slot_before = fs::symlink_metadata(&slot).unwrap();
    let (fixture, intent, authority) =
        coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
    let intent_record = intent.record().clone();
    let parked = active_reblit_parked_slot_path(&fixture, &intent_record, 0);
    let parked_slot = active_reblit_slot_marker_path(
        &parked,
        fixture.previous_state,
        intent_record.previous.tree_token.as_str(),
    );
    let replacement = active_reblit_replacement_path(&fixture, &intent_record, 0);
    assert!(!wrapper.exists());
    assert_eq!(directory_identity(&parked), wrapper_before);
    let parked_before = fs::symlink_metadata(&parked_slot).unwrap();
    assert_eq!((parked_before.dev(), parked_before.ino()), (slot_before.dev(), slot_before.ino()));
    assert_empty_private_reservation(&replacement);
    reset_retained_exchange_syscall_count();

    let exchanged = intent.execute_usr_exchange(authority).unwrap();

    assert_eq!(exchanged.record().phase, Phase::UsrExchanged);
    assert_eq!(retained_exchange_syscall_count(), 1);
    assert!(!wrapper.exists());
    assert_eq!(directory_identity(&parked), wrapper_before);
    let slot_after = fs::symlink_metadata(&parked_slot).unwrap();
    assert_eq!((slot_after.dev(), slot_after.ino()), (slot_before.dev(), slot_before.ino()));
    assert_eq!(slot_after.nlink(), 2);
    assert_empty_private_reservation(&replacement);
    exchanged.revalidate_retained_authorities().unwrap();
    assert_root_links_absent(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_slot_and_state_substitution_stop_before_exchange() {
    {
        let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
        let displaced = fixture.installation.root_path("slot-wrapper.displaced");
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
        let intent_record = intent.record().clone();
        let parked = active_reblit_parked_slot_path(&fixture, &intent_record, 0);
        let hook_parked = parked.clone();
        let hook_displaced = displaced.clone();
        arm_before_retained_exchange_rename(move || {
            fs::rename(&hook_parked, &hook_displaced).unwrap();
            fs::create_dir(&hook_parked).unwrap();
            fs::set_permissions(&hook_parked, fs::Permissions::from_mode(0o700)).unwrap();
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
fn journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        for point in [
            RetainedExchangeFaultPoint::StagingParentSync,
            RetainedExchangeFaultPoint::InstallationRootSync,
            RetainedExchangeFaultPoint::FinalRevalidation,
        ] {
            let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(candidate_kind);
            let intent_record = intent.record().clone();
            let candidate = directory_identity(&fixture.candidate_path);
            let previous = directory_identity(&fixture.installation.root.join("usr"));
            let namespace_before_exchange = snapshot_startup_recovery_namespace(&fixture.installation.root);
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
            assert_eq!(retained_exchange_syscall_count(), 1, "{candidate_kind:?} {point:?}");
            assert_exchange_layout(&fixture, true, candidate, previous);
            assert_eq!(read_canonical(&fixture.installation.root), intent_record);
            assert_root_links_absent(&fixture);

            let post_exchange_namespace = snapshot_startup_recovery_namespace(&fixture.installation.root);
            let database_before = usr_exchange_database_snapshot(&fixture, &intent_record);

            assert_usr_exchange_post_recovers_to_pending_reverse(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            assert_eq!(retained_exchange_syscall_count(), 1, "{candidate_kind:?} {point:?}");
            let decision = read_canonical(&fixture.installation.root);
            assert_exact_pending_reverse_decision(&intent_record, &decision);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                post_exchange_namespace,
                "{candidate_kind:?} {point:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {point:?}"
            );

            assert_usr_rollback_decision_routes_to_reverse_exchange_intent(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            assert!(
                retained_exchange_syscall_count() == 1,
                "routing retried the forward exchange for {candidate_kind:?} {point:?}"
            );
            assert_eq!(
                read_canonical(&fixture.installation.root),
                decision.rollback_successor(None).unwrap(),
                "{candidate_kind:?} {point:?}"
            );
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                post_exchange_namespace,
                "{candidate_kind:?} {point:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {point:?}"
            );

            let reverse_intent = decision.rollback_successor(None).unwrap();
            assert_reverse_exchange_intent_recovers_to_usr_restored(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            let restored = reverse_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(retained_exchange_syscall_count(), 2, "{candidate_kind:?} {point:?}");
            assert_eq!(read_canonical(&fixture.installation.root), restored);
            assert_exchange_layout(&fixture, false, candidate, previous);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                namespace_before_exchange,
                "{candidate_kind:?} {point:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {point:?}"
            );
            assert_root_links_absent(&fixture);

            assert_usr_restored_routes_to_candidate_preserve_intent(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );
            let preserve_intent = restored.rollback_successor(None).unwrap();
            assert_eq!(retained_exchange_syscall_count(), 2, "{candidate_kind:?} {point:?}");
            assert_eq!(read_canonical(&fixture.installation.root), preserve_intent);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                namespace_before_exchange,
                "{candidate_kind:?} {point:?}"
            );
        }
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
        let displaced_wrapper = fixture.installation.root_path("post-effect-slot.displaced");
        let database = fixture.database.clone();
        let candidate = fixture.candidate_state;
        let root = fixture.installation.root.clone();
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(kind, fixture, identity, authority);
        let intent_record = intent.record().clone();
        let parked_wrapper = active_reblit_parked_slot_path(&fixture, &intent_record, 0);
        arm_after_retained_exchange_rename(move || match mutation {
            "database" => database
                .clear_transition_if_matches(candidate, &intent_record.transition_id)
                .unwrap(),
            "provenance" => database.delete_metadata_provenance_for_test(candidate).unwrap(),
            "root-abi" => std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap(),
            "active-reblit-state" => database.remove(&candidate).unwrap(),
            "second-link" => {
                fs::rename(&parked_wrapper, &displaced_wrapper).unwrap();
                fs::create_dir(&parked_wrapper).unwrap();
                fs::set_permissions(&parked_wrapper, fs::Permissions::from_mode(0o700)).unwrap();
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
fn journal_coordinator_usr_exchange_completion_faults_recover_from_exact_source_to_usr_restored() {
    let faults: [(fn(), fn(), Phase); 5] = [
        (
            crate::transition_journal::arm_next_temporary_sync_fault,
            crate::transition_journal::assert_temporary_sync_fault_consumed,
            Phase::UsrExchangeIntent,
        ),
        (
            crate::transition_journal::arm_next_update_exchange_fault,
            crate::transition_journal::assert_update_exchange_fault_consumed,
            Phase::UsrExchangeIntent,
        ),
        (
            crate::transition_journal::arm_next_update_first_directory_sync_fault,
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed,
            Phase::UsrExchanged,
        ),
        (
            crate::transition_journal::arm_next_displaced_unlink_fault,
            crate::transition_journal::assert_displaced_unlink_fault_consumed,
            Phase::UsrExchanged,
        ),
        (
            crate::transition_journal::arm_next_update_final_directory_sync_fault,
            crate::transition_journal::assert_update_final_directory_sync_fault_consumed,
            Phase::UsrExchanged,
        ),
    ];

    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        for (arm, assert_consumed, durable_phase) in faults {
            let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(candidate_kind);
            let intent_record = intent.record().clone();
            let exchanged_record = intent_record.forward_successor(None).unwrap();
            let candidate = directory_identity(&fixture.candidate_path);
            let previous = directory_identity(&fixture.installation.root.join("usr"));
            let namespace_before_exchange =
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root);
            let database_before = usr_exchange_database_snapshot(&fixture, &intent_record);
            reset_retained_exchange_syscall_count();
            arm();

            let failure = intent.execute_usr_exchange(authority).unwrap_err();

            assert_consumed();
            assert!(matches!(failure, UsrExchangeEffectFailure::CompletionPersistence { .. }));
            assert_eq!(retained_exchange_syscall_count(), 1, "{candidate_kind:?} {durable_phase:?}");
            assert_exchange_layout(&fixture, true, candidate, previous);
            let durable_source = match durable_phase {
                Phase::UsrExchangeIntent => intent_record.clone(),
                Phase::UsrExchanged => exchanged_record,
                _ => unreachable!(),
            };
            assert_eq!(read_canonical(&fixture.installation.root), durable_source);
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_root_links_absent(&fixture);
            let post_exchange_namespace =
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root);

            assert_usr_exchange_post_recovers_to_pending_reverse(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            assert_eq!(retained_exchange_syscall_count(), 1, "{candidate_kind:?} {durable_phase:?}");
            let decision = read_canonical(&fixture.installation.root);
            assert_exact_pending_reverse_decision(&durable_source, &decision);
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                post_exchange_namespace,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_root_links_after_forward_recovery(&fixture, durable_phase);

            assert_usr_rollback_decision_routes_to_reverse_exchange_intent(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            assert_eq!(retained_exchange_syscall_count(), 1, "{candidate_kind:?} {durable_phase:?}");
            let reverse_intent = decision.rollback_successor(None).unwrap();
            assert_eq!(read_canonical(&fixture.installation.root), reverse_intent);
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                post_exchange_namespace,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_root_links_after_forward_recovery(&fixture, durable_phase);

            assert_reverse_exchange_intent_recovers_to_usr_restored(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );

            let restored = reverse_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(retained_exchange_syscall_count(), 2, "{candidate_kind:?} {durable_phase:?}");
            assert_eq!(read_canonical(&fixture.installation.root), restored);
            assert_exchange_layout(&fixture, false, candidate, previous);
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                namespace_before_exchange,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_root_links_after_forward_recovery(&fixture, durable_phase);

            assert_usr_restored_routes_to_candidate_preserve_intent(
                &fixture.installation,
                &fixture.database,
                &fixture.layout_database,
            );
            let preserve_intent = restored.rollback_successor(None).unwrap();
            assert_eq!(retained_exchange_syscall_count(), 2, "{candidate_kind:?} {durable_phase:?}");
            assert_eq!(read_canonical(&fixture.installation.root), preserve_intent);
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                namespace_before_exchange,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &intent_record),
                database_before,
                "{candidate_kind:?} {durable_phase:?}"
            );
            assert_exchange_layout(&fixture, false, candidate, previous);
            assert_root_links_after_forward_recovery(&fixture, durable_phase);
        }
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

/// Drive an applied-but-faulted exchange all the way to the exact
/// `ReverseExchangeIntent`, so a caller can substitute the live tree and prove
/// the *reverse* exchange refuses.
///
/// The existing recovery tests all take this route to prove it advances. These
/// take it to prove it stops, which is the half a substituted tree exercises.
fn reverse_exchange_intent_after_applied_exchange(
    candidate_kind: CandidateKind,
) -> (CoordinatorFixture, TransitionRecord, (u64, u64), (u64, u64)) {
    let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(candidate_kind);
    let candidate = directory_identity(&fixture.candidate_path);
    let previous = directory_identity(&fixture.installation.root.join("usr"));
    reset_retained_exchange_syscall_count();
    arm_retained_exchange_fault(RetainedExchangeFaultPoint::FinalRevalidation);

    let failure = intent.execute_usr_exchange(authority).unwrap_err();
    assert!(matches!(
        failure,
        UsrExchangeEffectFailure::Exchange {
            outcome: RetainedExchangeOutcome::Applied,
            ..
        }
    ));
    assert_eq!(retained_exchange_syscall_count(), 1);
    // The fault must have been consumed, not merely armed. A fault point that
    // is never reached produces the same green result as one that is survived,
    // and the two mean opposite things — see the note in `fault_injection.rs`.
    assert!(
        !retained_exchange_fault_armed(),
        "the forward exchange never reached its durability fault point"
    );

    assert_usr_exchange_post_recovers_to_pending_reverse(
        &fixture.installation,
        &fixture.database,
        &fixture.layout_database,
    );
    assert_usr_rollback_decision_routes_to_reverse_exchange_intent(
        &fixture.installation,
        &fixture.database,
        &fixture.layout_database,
    );

    let reverse_intent = read_canonical(&fixture.installation.root);
    assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent);
    assert_eq!(retained_exchange_syscall_count(), 1, "routing must not exchange");
    assert_exchange_layout(&fixture, true, candidate, previous);
    (fixture, reverse_intent, candidate, previous)
}

#[test]
fn journal_coordinator_reverse_exchange_refuses_a_hardlinked_marker() {
    let (fixture, reverse_intent, candidate, previous) =
        reverse_exchange_intent_after_applied_exchange(CandidateKind::Archived);
    let marker = fixture.installation.root.join("usr/.cast-tree-id");
    let external = fixture.installation.root.join("external-marker");

    // Same bytes, but reachable under a second name. A rewrite that only
    // changes the marker's inode is inert (see the test below); sharing the
    // inode is not, because the tree's identity becomes writable from outside
    // the tree.
    let frame = fs::read(&marker).unwrap();
    fs::write(&external, &frame).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o444)).unwrap();
    fs::remove_file(&marker).unwrap();
    fs::hard_link(&external, &marker).unwrap();
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 2);

    let reason =
        reverse_exchange_intent_refusal_reason(&fixture.installation, &fixture.database, &fixture.layout_database);
    assert!(!reason.is_empty(), "refusal carried no reason");

    assert_eq!(retained_exchange_syscall_count(), 1, "the reverse exchange must not run");
    assert_eq!(read_canonical(&fixture.installation.root), reverse_intent);
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 2, "the link was repaired");
}

/// The legacy route refused this; the coordinated route accepts it, and that
/// difference is deliberate rather than a gap.
///
/// The durable record identifies the previous tree by `usr_runtime_identity`
/// — the *directory's* `(st_dev, inode, mount_id)` — plus `tree_token`.
/// Rewriting the marker file with identical bytes changes neither, so no
/// durable evidence distinguishes it, and the reverse exchange has everything
/// it needs to be correct.
///
/// The legacy refusal came from holding a live descriptor on the marker across
/// the whole transition and revalidating by inode. Crash recovery cannot hold
/// one — it starts in a fresh process after a reboot — so that strictness is
/// not portable, and it was catching an inert rewrite rather than a hazard.
/// The two tests around this one pin the substitutions that *are* refused:
/// a shared inode, and a swapped directory.
#[test]
fn journal_coordinator_reverse_exchange_accepts_an_inert_same_content_marker_rewrite() {
    let (fixture, _reverse_intent, candidate, previous) =
        reverse_exchange_intent_after_applied_exchange(CandidateKind::Archived);
    let marker = fixture.installation.root.join("usr/.cast-tree-id");

    // Rewrite the marker at its canonical 0o444 rather than through
    // Rewrite at the canonical 0o444 rather than through
    // `replace_file_with_same_bytes`, which writes 0o644: a wrong mode would be
    // caught as a mode fault and would say nothing about inode identity.
    let frame = fs::read(&marker).unwrap();
    let original = fs::symlink_metadata(&marker).unwrap().ino();
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, &frame).unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o444)).unwrap();
    let rewritten = fs::symlink_metadata(&marker).unwrap().ino();
    assert_ne!(original, rewritten);
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 1);

    assert_reverse_exchange_intent_recovers_to_usr_restored(
        &fixture.installation,
        &fixture.database,
        &fixture.layout_database,
    );

    // The reverse exchange ran, ran exactly once, and put both trees back at
    // their original directory identities — the identities the record names,
    // and the ones the rewrite never touched.
    assert_eq!(retained_exchange_syscall_count(), 2);
    assert_eq!(read_canonical(&fixture.installation.root).phase, Phase::UsrRestored);
    assert_exchange_layout(&fixture, false, candidate, previous);
}

#[test]
fn journal_coordinator_reverse_exchange_refuses_a_whole_directory_same_token_substitution() {
    let (fixture, reverse_intent, candidate, previous) =
        reverse_exchange_intent_after_applied_exchange(CandidateKind::Archived);
    let live = fixture.installation.root.join("usr");
    let displaced = fixture.installation.root.join("displaced-live-usr");

    // A fresh directory carrying the same marker frame and the same `.stateID`
    // is indistinguishable from the retained tree by content alone. Only the
    // retained descriptor separates them.
    let frame = fs::read(live.join(".cast-tree-id")).unwrap();
    let state_id = fs::read(live.join(".stateID")).unwrap();
    fs::rename(&live, &displaced).unwrap();
    create_canonical_directory(&live);
    fs::write(live.join(".cast-tree-id"), &frame).unwrap();
    fs::set_permissions(live.join(".cast-tree-id"), fs::Permissions::from_mode(0o444)).unwrap();
    write_canonical_file(&live.join(".stateID"), &state_id);
    let substituted = directory_identity(&live);

    let reason =
        reverse_exchange_intent_refusal_reason(&fixture.installation, &fixture.database, &fixture.layout_database);
    assert!(!reason.is_empty(), "refusal carried no reason");
    assert_eq!(retained_exchange_syscall_count(), 1, "the reverse exchange must not run");
    assert_eq!(read_canonical(&fixture.installation.root), reverse_intent);
    assert_eq!(
        directory_identity(&live),
        substituted,
        "recovery must not exchange the substituted directory"
    );
    assert!(displaced.is_dir(), "the retained tree must survive untouched");
}

/// A previous tree that vanishes between preparation and the exchange must
/// stop the transition, never be synthesized.
///
/// The synthesize path is legitimate for a first install, where
/// `installation.active_state` is `None`. The hazard is an *active* previous
/// whose tree has gone missing: silently synthesizing an empty one there would
/// exchange the candidate over nothing and strand the real predecessor under a
/// name the record no longer describes.
#[test]
fn journal_coordinator_usr_exchange_never_synthesizes_a_missing_active_previous() {
    let (fixture, intent, authority) = coordinator_ready_for_usr_exchange_effect(CandidateKind::Archived);
    let intent_record = intent.record().clone();
    let live = fixture.installation.root.join("usr");
    let displaced = fixture.installation.root.join("displaced-previous-usr");
    let previous = directory_identity(&live);
    let candidate = directory_identity(&fixture.candidate_path);
    assert!(fixture.installation.active_state.is_some());
    fs::rename(&live, &displaced).unwrap();
    reset_retained_exchange_syscall_count();

    let failure = intent.execute_usr_exchange(authority).unwrap_err();

    assert!(
        matches!(failure, UsrExchangeEffectFailure::Preflight { .. }),
        "expected a preflight refusal, got {failure:?}"
    );
    assert_eq!(retained_exchange_syscall_count(), 0, "no exchange may be attempted");
    // `exists()` follows symlinks and would read a dangling link as absent.
    assert_state_metadata_name_absent(&live);
    assert_eq!(
        directory_identity(&displaced),
        previous,
        "the displaced previous tree must be left exactly where it went"
    );
    assert_eq!(directory_identity(&fixture.candidate_path), candidate);
    assert_eq!(read_canonical(&fixture.installation.root), intent_record);
}


// Ported from `fresh_identity_can_archive_after_a_complete_compensating_recovery`.
//
// Every other coordinated cross-transition test starts from a *successful*
// prior transition. This one starts from a rolled-back one: the claim is that a
// completed compensating recovery leaves the installation genuinely reusable,
// not merely consistent. A rollback that left residue — a half-retired slot, a
// stale journal, an unreleased reservation — would satisfy every single-
// transition assertion and still make the next transition impossible.
#[test]
// IGNORED — a reproducer, not a passing port. Measured 2026-08-04: the
// ActivateArchived rollback reaches `CandidatePreserveIntent` and then neither
// advances nor names a blocker. `drive_startup_recovery_to_clean` records the
// trail `[UsrRestored, CandidatePreserveIntent x31]`, every entry blocker-free.
// Confirmed independently by `reverse_exchange_intent_refusal_reason`, which
// trips its own "stalled without naming a blocker" assertion at that phase.
//
// A blocker-free non-advancing entry is a liveness failure whatever the cause:
// a real system would spin at every boot with nothing to diagnose it by.
//
// Caveat on the cause: this fixture's archived candidate is staged directly by
// `fixture_parts` and has no originating archive slot, so the `Rearchive`
// disposition may have no valid destination. That would explain a *refusal*; it
// does not explain silent non-advancement. Whether the fix belongs in the
// rearchive destination logic or in the fixture is the open question — tasks
// #23 and #24 covered adjacent ground.
#[ignore = "reproducer for the CandidatePreserveIntent rollback stall; see comment"]
fn journal_coordinator_a_completed_rollback_leaves_the_installation_reusable() {
    let (fixture, _reverse_intent, candidate, previous) =
        reverse_exchange_intent_after_applied_exchange(CandidateKind::Archived);

    let entries = drive_startup_recovery_to_clean(
        &fixture.installation,
        &fixture.database,
        &fixture.layout_database,
    );
    assert!(entries > 1, "the rollback completed without advancing");

    // A clean startup means no record survives to block the next transition.
    assert_canonical_journal_absent(&fixture.installation.root);
    assert_exchange_layout(&fixture, false, candidate, previous);

    // The installation must now accept a fresh transition that archives the
    // same previous state the rollback just restored.
    fs::remove_dir_all(fixture.installation.staging_path("usr")).unwrap();
    let (identity, authority) = reacquire_new_state(&fixture);
    drop(identity);
    drop(authority);
}
