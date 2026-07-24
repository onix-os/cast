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
