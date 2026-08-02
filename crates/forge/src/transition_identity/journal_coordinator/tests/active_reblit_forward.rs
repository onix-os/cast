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

// The wrapper-rotation proof, ported from
// `active_reblit_rotates_the_whole_old_wrapper_and_leaves_exact_empty_staging`.
//
// Worth stating precisely, because the intermediate state is misleading: at
// `SystemTriggersComplete` the old live tree is sitting in *staging* and the
// quarantine wrapper is an empty reserved name. The rotation into the wrapper
// happens in the no-boot completion step. Asserting the legacy end state
// against the forward prefix alone would fail, and asserting the prefix's own
// state as if it were final would enshrine a half-finished namespace.
#[test]
fn coordinated_active_reblit_rotates_the_old_wrapper_and_empties_staging() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
    let live = fixture.installation.root.join("usr");
    let old_usr_inode = fs::symlink_metadata(&live).unwrap().ino();

    let complete = run_active_reblit(&fixture, identity, authority).expect("clean active reblit reaches completion");
    complete
        .complete_active_reblit_without_boot()
        .expect("no-boot completion rotates the wrapper");

    // The live tree is a different inode than the one the transition started
    // on: the re-blitted candidate, not the tree it replaced.
    assert_ne!(fs::symlink_metadata(&live).unwrap().ino(), old_usr_inode);

    // Staging is left exactly empty — no residue of the tree that passed
    // through it on the way to the wrapper.
    assert_eq!(
        fs::read_dir(fixture.installation.staging_dir()).unwrap().count(),
        0,
        "staging was not left empty"
    );

    // Exactly one wrapper was reserved, and it holds the old tree by inode.
    let wrappers = wrapper_quarantines(&fixture);
    assert_eq!(wrappers.len(), 1, "expected exactly one replaced wrapper");
    assert_eq!(
        fs::symlink_metadata(wrappers[0].join("usr")).unwrap().ino(),
        old_usr_inode,
        "the quarantined wrapper does not hold the original live tree"
    );
}

fn wrapper_quarantines(fixture: &CoordinatorFixture) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(fixture.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("replaced-active-reblit-wrapper-")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
