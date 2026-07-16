#[test]
fn journal_coordinator_fresh_allocation_effect_observes_durable_intent_before_database_commit() {
    let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
    let coordinator = identity
        .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
        .unwrap()
        .begin_fresh_allocation()
        .unwrap();
    let allocating = coordinator.record().clone();
    assert_record_prefix(
        &allocating,
        Operation::NewState,
        Phase::FreshStateAllocating,
        2,
    );
    assert_eq!(read_canonical(&fixture.installation.root), allocating);
    assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);

    // This closure represents the explicitly interleaved DB effect. The
    // coordinator does not claim generic callback ownership in this slice.
    let database_effect = || {
        assert_eq!(read_canonical(&fixture.installation.root), allocating);
        assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
        allocate_matching_state(&fixture, &coordinator)
    };
    let allocated = database_effect();
    assert_eq!(allocated, fixture.candidate_state);
    assert_eq!(read_canonical(&fixture.installation.root), allocating);
    assert_eq!(
        fixture
            .database
            .transition_ownership(allocated, &allocating.transition_id)
            .unwrap(),
        TransitionOwnership::Matching
    );

    let coordinator = coordinator
        .finish_fresh_allocation(&fixture.database, allocated)
        .unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::FreshStateAllocated,
        3,
    );
    assert_eq!(read_canonical(&fixture.installation.root), *coordinator.record());
}

#[test]
fn journal_coordinator_allocation_finish_rejects_missing_cleared_foreign_and_wrong_state_evidence() {
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let before = coordinator.record().clone();
        let unrelated = db::state::Database::new(":memory:").unwrap();
        let unrelated_state = unrelated
            .add_with_transition(
                &before.transition_id,
                &[],
                Some("matching token in unrelated database"),
                None,
            )
            .unwrap()
            .id;
        assert!(matches!(
            coordinator.finish_fresh_allocation(&unrelated, unrelated_state),
            Err(StatefulTransitionCoordinatorError::StateDatabaseCapabilityMismatch)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
        assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
        assert_eq!(
            unrelated
                .transition_ownership(unrelated_state, &before.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let before = coordinator.record().clone();
        let missing = state::Id::from(10_000);
        assert!(matches!(
            coordinator.finish_fresh_allocation(&fixture.database, missing),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state: 10_000,
                ownership: TransitionOwnership::Missing,
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
            coordinator.finish_fresh_allocation(&fixture.database, fixture.previous_state),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                ownership: TransitionOwnership::Cleared,
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
        let foreign = fixture
            .database
            .add_with_transition(&other_transition_id(), &[], Some("foreign transition"), None)
            .unwrap()
            .id;
        assert!(matches!(
            coordinator.finish_fresh_allocation(&fixture.database, foreign),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                ownership: TransitionOwnership::Foreign,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
        assert_eq!(
            fixture
                .database
                .transition_ownership(foreign, &other_transition_id())
                .unwrap(),
            TransitionOwnership::Matching
        );
    }
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let before = coordinator.record().clone();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        assert_eq!(allocated, fixture.candidate_state);
        assert!(matches!(
            coordinator.finish_fresh_allocation(&fixture.database, fixture.previous_state),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                ownership: TransitionOwnership::Cleared,
                ..
            })
        ));
        assert_eq!(reopen_record(&fixture.installation.root), before);
        assert_eq!(
            fixture
                .database
                .transition_ownership(allocated, &before.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
    }
}

#[test]
fn journal_coordinator_database_commit_and_completion_share_exact_transition_correlation() {
    let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
    let coordinator = identity
        .begin_transition(request(CandidateKind::NewState, &fixture, true, true))
        .unwrap()
        .begin_fresh_allocation()
        .unwrap();
    let transition = coordinator.transition_id_for_allocation().unwrap().clone();
    let allocated = fixture
        .database
        .add_with_transition(&transition, &[], Some("exact transition candidate"), None)
        .unwrap()
        .id;
    assert_eq!(allocated, fixture.candidate_state);
    assert_eq!(
        fixture.database.get_by_transition(&transition).unwrap().unwrap().id,
        allocated
    );
    assert_eq!(
        fixture
            .database
            .transition_ownership(allocated, &transition)
            .unwrap(),
        TransitionOwnership::Matching
    );

    let coordinator = coordinator
        .finish_fresh_allocation(&fixture.database, allocated)
        .unwrap();
    assert_eq!(coordinator.record().transition_id, transition);
    assert_eq!(coordinator.record().candidate.id, Some(i32::from(allocated)));
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::FreshStateAllocated,
        3,
    );
    let coordinator = coordinator
        .begin_candidate_prepare()
        .unwrap()
        .finish_candidate_prepare()
        .unwrap();
    assert_record_prefix(
        coordinator.record(),
        Operation::NewState,
        Phase::CandidatePrepared,
        5,
    );
    assert_candidate_state_id(&fixture, allocated);
    assert_eq!(
        fixture.database.audit_in_flight_transition().unwrap().unwrap(),
        db::state::InFlightTransition {
            state_id: allocated,
            transition_id: transition,
        }
    );
}

#[test]
fn journal_coordinator_post_commit_journal_failure_preserves_matching_database_evidence() {
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let allocating = coordinator.record().clone();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        crate::transition_journal::arm_next_temporary_sync_fault();
        assert!(matches!(
            coordinator.finish_fresh_allocation(&fixture.database, allocated),
            Err(StatefulTransitionCoordinatorError::Journal(_))
        ));
        crate::transition_journal::assert_temporary_sync_fault_consumed();

        assert_eq!(reopen_record(&fixture.installation.root), allocating);
        assert_eq!(
            fixture
                .database
                .transition_ownership(allocated, &allocating.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
        assert_eq!(
            fixture
                .database
                .get_by_transition(&allocating.transition_id)
                .unwrap()
                .unwrap()
                .id,
            allocated
        );
        assert_eq!(
            journal_names(&fixture.installation.root),
            ["state-transition", "state-transition.lock"]
        );
        assert_candidate_state_id_absent(&fixture);
    }

    // The state ID is already durably published when the final journal
    // advance fails. Recovery therefore sees exact DB + namespace evidence
    // correlated with the still-durable CandidatePrepareStarted record.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let allocated = state::Id::from(started.candidate.id.unwrap());
        assert_candidate_state_id_absent(&fixture);
        crate::transition_journal::arm_next_temporary_sync_fault();
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::Journal(_))
        ));
        crate::transition_journal::assert_temporary_sync_fault_consumed();

        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id(&fixture, allocated);
        assert_eq!(
            fixture
                .database
                .transition_ownership(allocated, &started.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
        assert_eq!(
            journal_names(&fixture.installation.root),
            ["state-transition", "state-transition.lock"]
        );
    }
}

fn coordinator_at_candidate_prepare_started(
    candidate_kind: CandidateKind,
) -> (CoordinatorFixture, StatefulTransitionCoordinator) {
    let (fixture, identity) = fixture(candidate_kind, PreviousKind::Active);
    let coordinator = identity
        .begin_transition(request(candidate_kind, &fixture, false, false))
        .unwrap();
    let coordinator = if candidate_kind == CandidateKind::NewState {
        let coordinator = coordinator.begin_fresh_allocation().unwrap();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        coordinator
            .finish_fresh_allocation(&fixture.database, allocated)
            .unwrap()
    } else {
        coordinator
    };
    let coordinator = coordinator.begin_candidate_prepare().unwrap();
    (fixture, coordinator)
}

#[test]
fn journal_coordinator_candidate_prepare_effect_order_and_failure_preserve_exact_evidence() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        let started = coordinator.record().clone();
        let expected_generation = if candidate_kind == CandidateKind::NewState { 4 } else { 2 };
        assert_eq!(started.phase, Phase::CandidatePrepareStarted);
        assert_eq!(started.generation, expected_generation);

        // The explicit test effect sees the durable intent. This demonstrates
        // interleaving; it does not claim a generic callback API.
        let candidate_effect = || {
            assert_eq!(read_canonical(&fixture.installation.root), started);
            if candidate_kind == CandidateKind::NewState {
                assert_eq!(
                    fixture
                        .database
                        .transition_ownership(fixture.candidate_state, &started.transition_id)
                        .unwrap(),
                    TransitionOwnership::Matching
                );
            } else {
                assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
            }
            Ok::<(), &'static str>(())
        };
        candidate_effect().unwrap();
        let coordinator = coordinator.finish_candidate_prepare().unwrap();
        assert_eq!(coordinator.record().phase, Phase::CandidatePrepared);
        assert_eq!(coordinator.record().generation, expected_generation + 1);
        assert_eq!(read_canonical(&fixture.installation.root), *coordinator.record());
        if candidate_kind == CandidateKind::NewState {
            assert_candidate_state_id(
                &fixture,
                state::Id::from(coordinator.record().candidate.id.unwrap()),
            );
        }
    }

    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        let started = coordinator.record().clone();
        let candidate_effect = || Err::<(), _>("injected candidate-prepare failure");
        assert!(candidate_effect().is_err());
        drop(coordinator);
        assert_eq!(reopen_record(&fixture.installation.root), started);
        if candidate_kind == CandidateKind::NewState {
            assert_candidate_state_id_absent(&fixture);
            assert_eq!(
                fixture
                    .database
                    .transition_ownership(fixture.candidate_state, &started.transition_id)
                    .unwrap(),
                TransitionOwnership::Matching
            );
        } else {
            assert_eq!(fixture.database.audit_in_flight_transition().unwrap(), None);
        }
    }

    // Clearing the exact allocation token after g3 blocks g4 without any
    // namespace mutation.
    {
        let (fixture, identity) = fixture(CandidateKind::NewState, PreviousKind::Active);
        let coordinator = identity
            .begin_transition(request(CandidateKind::NewState, &fixture, false, false))
            .unwrap()
            .begin_fresh_allocation()
            .unwrap();
        let allocated = allocate_matching_state(&fixture, &coordinator);
        let coordinator = coordinator
            .finish_fresh_allocation(&fixture.database, allocated)
            .unwrap();
        let allocated_record = coordinator.record().clone();
        fixture
            .database
            .clear_transition_if_matches(allocated, &allocated_record.transition_id)
            .unwrap();
        assert!(matches!(
            coordinator.begin_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state,
                ownership: TransitionOwnership::Cleared,
            }) if state == i32::from(allocated)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), allocated_record);
        assert_candidate_state_id_absent(&fixture);
    }

    // Clearing the token after g4 is likewise rejected before publication.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let allocated = state::Id::from(started.candidate.id.unwrap());
        fixture
            .database
            .clear_transition_if_matches(allocated, &started.transition_id)
            .unwrap();
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state,
                ownership: TransitionOwnership::Cleared,
            }) if state == i32::from(allocated)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id_absent(&fixture);
    }

    // The scoped hook mutates the already-retained marker inode itself. This
    // is not a pathname substitution: the device/inode pair stays exact while
    // its authenticated metadata changes before the runtime proof.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let marker = fixture.candidate_path.join(".cast-tree-id");
        let before = fs::symlink_metadata(&marker).unwrap();
        let hook_marker = marker.clone();
        arm_before_finish_candidate_runtime_proof(move || {
            fs::set_permissions(&hook_marker, fs::Permissions::from_mode(0o644)).unwrap();
        });
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::Identity(_))
        ));
        let after = fs::symlink_metadata(&marker).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.permissions().mode() & 0o7777, 0o644);
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id_absent(&fixture);
    }

    // A collision introduced after the publication preflight is never
    // overwritten by the no-replace rename. The coordinator remains at g4.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let collision = state_id_path(&fixture);
        super::super::state_tree_metadata::arm_before_state_id_publish(move || {
            write_canonical_file(&collision, b"foreign");
        });
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::StateIdPublication(failure))
                if failure.outcome()
                    == super::super::state_tree_metadata::StateIdPublicationOutcome::NotPublished
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_eq!(fs::read(state_id_path(&fixture)).unwrap(), b"foreign");
        assert_candidate_state_id_temporary_absent(&fixture);
    }

    // A known pre-rename sync failure is fully cleaned up and leaves no
    // canonical or temporary state-ID evidence.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        super::super::state_tree_metadata::arm_state_id_publication_fault(
            super::super::state_tree_metadata::StateIdPublicationFaultPoint::TemporarySync,
        );
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::StateIdPublication(failure))
                if failure.outcome()
                    == super::super::state_tree_metadata::StateIdPublicationOutcome::NotPublished
        ));
        super::super::state_tree_metadata::assert_state_id_publication_fault_consumed();
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id_absent(&fixture);
    }

    // If the exact inode loses its canonical name after rename and a foreign
    // inode occupies that name before reconciliation, the publisher neither
    // retries nor adopts it. The outcome is explicitly ambiguous at g4.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let canonical = state_id_path(&fixture);
        let hook_canonical = canonical.clone();
        let hook_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let observed_calls = hook_calls.clone();
        super::super::state_tree_metadata::arm_after_state_id_rename(move || {
            hook_calls.set(hook_calls.get() + 1);
            fs::remove_file(&hook_canonical).unwrap();
            write_canonical_file(&hook_canonical, b"foreign-after-rename");
        });
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::StateIdPublication(failure))
                if failure.outcome()
                    == super::super::state_tree_metadata::StateIdPublicationOutcome::Ambiguous
        ));
        assert_eq!(observed_calls.get(), 1);
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_eq!(fs::read(canonical).unwrap(), b"foreign-after-rename");
        assert_candidate_state_id_temporary_absent(&fixture);
    }

    // A directory-sync checkpoint fails conservatively after rename: the
    // exact final inode remains, but CandidatePrepared is never claimed.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let allocated = state::Id::from(started.candidate.id.unwrap());
        super::super::state_tree_metadata::arm_state_id_publication_fault(
            super::super::state_tree_metadata::StateIdPublicationFaultPoint::DirectorySync,
        );
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::StateIdPublication(failure))
                if failure.outcome()
                    == super::super::state_tree_metadata::StateIdPublicationOutcome::Published
        ));
        super::super::state_tree_metadata::assert_state_id_publication_fault_consumed();
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id(&fixture, allocated);
    }

    // Mutating the retained DB after the coordinator's pre-publication check
    // is caught by the post-publication check. The g4 journal and exact final
    // inode make the interrupted ordering explicit for recovery.
    {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let allocated = state::Id::from(started.candidate.id.unwrap());
        let database = fixture.database.clone();
        let transition = started.transition_id.clone();
        super::super::state_tree_metadata::arm_before_state_id_publish(move || {
            database
                .clear_transition_if_matches(allocated, &transition)
                .unwrap();
        });
        assert!(matches!(
            coordinator.finish_candidate_prepare(),
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state,
                ownership: TransitionOwnership::Cleared,
            }) if state == i32::from(allocated)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_candidate_state_id(&fixture, allocated);
        assert_eq!(
            fixture
                .database
                .transition_ownership(allocated, &started.transition_id)
                .unwrap(),
            TransitionOwnership::Cleared
        );
    }
}
