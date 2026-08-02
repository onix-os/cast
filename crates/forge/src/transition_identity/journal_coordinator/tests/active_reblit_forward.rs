// Coordinated ActiveReblit forward proofs, ported from the legacy
// `client/active_reblit_tests.rs` suite.
//
// ActiveReblit re-blits the *live* state onto itself, so it is the one
// operation whose candidate and previous are the same row. That makes the old
// live wrapper something the transition has to get out of the way wholesale
// rather than archive, which is what the wrapper-rotation proofs below are
// about.

/// Drive the composed ActiveReblit prefix over a fixture, with hooks already
/// armed by the caller. Mirrors the legacy `run` helper.
fn run_active_reblit(
    fixture: &CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
) -> Result<SystemTriggersCompleteCoordinator, ActiveReblitForwardError> {
    execute_active_reblit_forward(
        identity,
        authority,
        fixture.candidate_state,
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
}

#[test]
fn coordinated_active_reblit_reaches_system_triggers_complete() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);

    let complete = run_active_reblit(&fixture, identity, authority).expect("clean active reblit reaches completion");

    assert_record_prefix(
        complete.record(),
        Operation::ActiveReblit,
        Phase::SystemTriggersComplete,
        10,
    );
}
