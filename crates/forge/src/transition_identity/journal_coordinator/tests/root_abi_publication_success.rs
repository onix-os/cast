#[test]
fn journal_coordinator_root_links_complete_publishes_all_initial_link_subsets_for_every_operation() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        for mask in 0..32 {
            let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(candidate_kind, mask);
            let source = exchanged.record().clone();
            let links_before = root_abi_identities(&fixture.installation.root);
            let namespace_before =
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root);
            let database_before = usr_exchange_database_snapshot(&fixture, &source);
            let live_before = directory_identity(&fixture.installation.root.join("usr"));
            let staged_before = directory_identity(&fixture.candidate_path);

            let complete = exchanged.publish_root_abi().unwrap();

            assert_record_prefix(
                complete.record(),
                source.operation,
                Phase::RootLinksComplete,
                expected_root_links_complete_generation(candidate_kind),
            );
            assert_eq!(read_canonical(&fixture.installation.root), *complete.record());
            assert_eq!(
                snapshot_startup_recovery_namespace_without_root_abi(&fixture.installation.root),
                namespace_before,
                "operation={candidate_kind:?} mask={mask:05b}"
            );
            assert_eq!(
                usr_exchange_database_snapshot(&fixture, &source),
                database_before,
                "operation={candidate_kind:?} mask={mask:05b}"
            );
            assert_eq!(directory_identity(&fixture.installation.root.join("usr")), live_before);
            assert_eq!(directory_identity(&fixture.candidate_path), staged_before);
            assert_initial_root_abi_inodes_preserved(&fixture.installation.root, &links_before);
            complete.revalidate_retained_authorities().unwrap();
        }
    }
}

#[test]
fn journal_coordinator_root_links_complete_preserves_synthesized_empty_and_active_reblit_reservations() {
    {
        let (fixture, exchanged) = coordinator_ready_for_root_abi_publication_with_previous(
            CandidateKind::NewState,
            PreviousKind::SynthesizedEmpty,
            0,
        );
        let source = exchanged.record().clone();
        assert_eq!(source.previous.origin, PreviousOrigin::SynthesizedEmpty);
        assert_eq!(source.previous.id, None);
        assert_state_metadata_name_absent(&fixture.candidate_path.join(".stateID"));

        let complete = exchanged.publish_root_abi().unwrap();

        assert_record_prefix(complete.record(), Operation::NewState, Phase::RootLinksComplete, 10);
        assert_state_metadata_name_absent(&fixture.candidate_path.join(".stateID"));
        assert_root_links_complete(&fixture);
        complete.revalidate_retained_authorities().unwrap();
    }

    {
        let (fixture, identity, authority) = fixture_parts_with_root_abi_mask(
            CandidateKind::ActiveReblit,
            PreviousKind::Active,
            true,
            true,
            0,
        );
        let authority = authority.expect("ActiveReblit root ABI fixture authority");
        let (fixture, intent, authority) =
            coordinator_from_exchange_fixture(CandidateKind::ActiveReblit, fixture, identity, authority);
        let exchanged = intent.execute_usr_exchange(authority).unwrap();
        let source = exchanged.record().clone();
        let parked = active_reblit_parked_slot_path(&fixture, &source, 0);
        let parked_slot = active_reblit_slot_marker_path(
            &parked,
            fixture.previous_state,
            source.previous.tree_token.as_str(),
        );
        let replacement = active_reblit_replacement_path(&fixture, &source, 0);
        let parked_before = directory_identity(&parked);
        let slot_before = fs::symlink_metadata(&parked_slot).unwrap();
        let replacement_before = directory_identity(&replacement);

        let complete = exchanged.publish_root_abi().unwrap();

        assert_record_prefix(complete.record(), Operation::ActiveReblit, Phase::RootLinksComplete, 8);
        assert_eq!(directory_identity(&parked), parked_before);
        let slot_after = fs::symlink_metadata(&parked_slot).unwrap();
        assert_eq!((slot_after.dev(), slot_after.ino()), (slot_before.dev(), slot_before.ino()));
        assert_eq!(slot_after.nlink(), 2);
        assert_eq!(directory_identity(&replacement), replacement_before);
        assert_empty_private_reservation(&replacement);
        assert_root_links_complete(&fixture);
        complete.revalidate_retained_authorities().unwrap();
    }
}

#[test]
fn journal_coordinator_root_links_complete_success_revalidates_exact_successor_binding_without_replaying_publication() {
    let (fixture, exchanged) = coordinator_ready_for_root_abi_publication(CandidateKind::Archived, 0);
    let callback_count = std::rc::Rc::new(std::cell::Cell::new(0));
    let callback_count_hook = std::rc::Rc::clone(&callback_count);
    crate::client::arm_before_retained_root_abi_link_publication(0, move || {
        callback_count_hook.set(callback_count_hook.get() + 1);
    });

    let complete = exchanged.publish_root_abi().unwrap();

    crate::client::assert_before_retained_root_abi_link_publication_consumed();
    assert_eq!(callback_count.get(), 1);
    complete.revalidate_retained_authorities().unwrap();
    complete.revalidate_retained_authorities().unwrap();
    assert_eq!(callback_count.get(), 1, "typestate revalidation replayed publication");

    let canonical = canonical_journal(&fixture.installation.root);
    let displaced = fixture.installation.root.join("successor-binding.displaced");
    replace_regular_file_with_same_bytes_at(&canonical, &displaced);
    let error = complete.revalidate_retained_authorities().unwrap_err();
    assert!(matches!(
        error,
        StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
            expected_phase: Phase::RootLinksComplete,
            ..
        }
    ), "unexpected successor-binding error: {error:?}");
    assert!(displaced.is_file());
    assert_eq!(callback_count.get(), 1);
    assert_root_links_complete(&fixture);
}
