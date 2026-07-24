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

/// Physical foundation for the (not-yet-built) `PreviousRestore` rollback
/// dispatcher: once the predecessor is archived, the transition journal is
/// retained at `PreviousArchived`, so the *legacy* restore correctly refuses to
/// move the tree (a present journal signals an unreconciled crash), while the
/// recovery-sealed restore is permitted to perform exactly the compensating
/// move the dispatcher will drive. Everything above the physical move — record
/// admission, namespace proof, journal advance — is the deferred dispatcher.
#[test]
fn recovery_sealed_restore_reverses_the_archive_while_the_journal_is_retained() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let previous = NewStatePrevious::Active(fixture.previous_state);

    let (complete, _allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        previous,
        &[],
        "new-state restore-primitive slice",
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

    let archived = complete
        .archive_previous_tree()
        .expect("archive_previous advances through PreviousArchived");

    let installation = archived.installation();
    let identity = archived.tree_identity();
    let previous_id = archived
        .record()
        .previous
        .id
        .map(crate::state::Id::from)
        .expect("archived record has a predecessor");

    // The journal is present at PreviousArchived, so the legacy (no-journal)
    // restore refuses before moving anything.
    let legacy = identity.restore_previous(installation, previous_id);
    let legacy = legacy.expect_err("legacy restore must refuse a present journal");
    assert_eq!(
        legacy.outcome(),
        crate::transition_identity::RetainedPreviousMoveOutcome::NotApplied,
        "legacy restore refuses at the journal guard, before any move",
    );

    // The recovery seal permits the compensating move despite the retained
    // journal — the exact physical step the PreviousRestore dispatcher performs.
    let seal = crate::transition_identity::PreviousRestoreRecoverySeal::for_recovery();
    identity
        .restore_previous_with_journal(installation, previous_id, &seal)
        .expect("recovery-sealed restore reverses the archive");
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
