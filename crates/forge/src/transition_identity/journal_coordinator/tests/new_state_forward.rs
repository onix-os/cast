#[test]
fn execute_new_state_forward_reaches_system_triggers_complete_with_fresh_allocation() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let previous = NewStatePrevious::Active(fixture.previous_state);

    let (complete, allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        previous,
        &[],
        "new-state forward slice",
        false,
        |_| {
            crate::transition_identity::CandidateMetadataOutputs::from_policy(
                COORDINATOR_OS_RELEASE,
                crate::system_model::snapshot_authorities(),
                COORDINATOR_SYSTEM_SNAPSHOT,
            )
        },
        |_view| Ok::<(), TriggerEffectError>(()),
        |_view| Ok::<(), TriggerEffectError>(()),
    )
    .expect("new-state forward prefix reaches system-triggers complete");

    // The composed driver ends exactly at the durable SystemTriggersComplete
    // authority, at the NewState generation (two higher than ActiveReblit's
    // because of the fresh-allocation phases).
    assert_record_prefix(
        complete.record(),
        Operation::NewState,
        Phase::SystemTriggersComplete,
        12,
    );
    // The mid-prefix fresh row was correlated into the record's candidate id.
    assert_eq!(complete.record().candidate.id, Some(i32::from(allocated)));
    // The fresh row is owned by this transition (proves the correlated
    // allocation, not a stray insert, backs the candidate).
    assert_eq!(
        fixture
            .database
            .transition_ownership(allocated, &complete.record().transition_id)
            .unwrap(),
        TransitionOwnership::Matching,
    );
}

#[test]
fn archive_previous_tree_advances_new_state_through_previous_archived() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let previous = NewStatePrevious::Active(fixture.previous_state);

    let (complete, _allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        previous,
        &[],
        "new-state archive slice",
        false,
        |_| {
            crate::transition_identity::CandidateMetadataOutputs::from_policy(
                COORDINATOR_OS_RELEASE,
                crate::system_model::snapshot_authorities(),
                COORDINATOR_SYSTEM_SNAPSHOT,
            )
        },
        |_view| Ok::<(), TriggerEffectError>(()),
        |_view| Ok::<(), TriggerEffectError>(()),
    )
    .expect("new-state forward prefix reaches system-triggers complete");

    // Durably archive the displaced predecessor: SystemTriggersComplete (12) →
    // PreviousArchiveIntent (13) → PreviousArchived (14).
    let archived: PreviousArchivedCoordinator = complete
        .archive_previous_tree()
        .expect("archive_previous advances through PreviousArchived");
    assert_record_prefix(
        archived.record(),
        Operation::NewState,
        Phase::PreviousArchived,
        14,
    );
    assert!(archived.record().options.archive_previous);
}

#[test]
fn new_state_previous_archived_hands_off_into_boot_with_candidate_state() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let previous = NewStatePrevious::Active(fixture.previous_state);

    let (complete, allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        previous,
        &[],
        "new-state boot slice",
        true,
        |_| {
            crate::transition_identity::CandidateMetadataOutputs::from_policy(
                COORDINATOR_OS_RELEASE,
                crate::system_model::snapshot_authorities(),
                COORDINATOR_SYSTEM_SNAPSHOT,
            )
        },
        |_view| Ok::<(), TriggerEffectError>(()),
        |_view| Ok::<(), TriggerEffectError>(()),
    )
    .expect("new-state forward prefix reaches system-triggers complete");

    let archived = complete
        .archive_previous_tree()
        .expect("archive_previous advances through PreviousArchived");
    let handoff = archived
        .into_new_state_boot_sync_handoff()
        .expect("NewState PreviousArchived hands off into boot");

    // The handoff carries the freshly booted candidate, distinct from the now
    // archived predecessor — the shape the operation-aware boot gate admits.
    assert_eq!(handoff.record().phase, Phase::PreviousArchived);
    assert_eq!(handoff.record().candidate.id, Some(i32::from(allocated)));
    assert_ne!(
        handoff.record().previous.id,
        handoff.record().candidate.id
    );
}
