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

// Ported from
// `active_reblit_refuses_missing_or_malformed_live_state_id_without_staging_mutation`.
//
// ActiveReblit is the only operation that takes its identity *from the live
// tree*, so a live `.stateID` that is absent or not the retained inode makes
// the whole premise unverifiable. The refusal must land before anything moves:
// no wrapper reserved, no staging mutation, live tree untouched.
#[test]
fn coordinated_active_reblit_refuses_a_broken_live_state_id_without_moving_anything() {
    for corruption in ["missing", "malformed"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
        let live = fixture.installation.root.join("usr");
        let live_inode = fs::symlink_metadata(&live).unwrap().ino();
        let path = live.join(".stateID");

        match corruption {
            // Unlinking leaves the retained descriptor at links=0.
            "missing" => fs::remove_file(&path).unwrap(),
            // Rewriting keeps the name but changes the inode behind it.
            _ => fs::write(&path, b"corrupt").unwrap(),
        }

        let error = run_active_reblit(&fixture, identity, authority)
            .err()
            .expect("a broken live state-ID must refuse the transition");

        assert_eq!(error.stage(), "/usr exchange", "{error:#?}");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("LiveActiveStateProof"),
            "{corruption} was not refused by the live active-state proof: {error:#?}"
        );
        // A rewritten `.stateID` is a well-formed file at a new inode, so the
        // inode *policy* check passes it and only snapshot revalidation can
        // catch it. That makes the malformed case deterministic.
        //
        // The missing case is not, and deliberately is not asserted that way.
        // An unlinked `.stateID` trips two independent guards — the policy
        // check sees `links=0`, and snapshot revalidation sees the retained
        // metadata change — and which one reports first is timing-dependent.
        // Measured at 2/25 runs taking the other branch. Pinning either one
        // makes this test flaky for no gain: both are `LiveActiveStateProof`
        // refusals and the transition stops either way.
        if corruption == "malformed" {
            assert!(
                rendered.contains("revalidate live active-state snapshot"),
                "malformed was not refused by snapshot revalidation: {error:#?}"
            );
        }

        // Nothing *moved*: the live tree is the same inode it was.
        assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), live_inode);

        // The coordinated route reserves the wrapper *name* during the forward
        // prefix, before the exchange that refuses here — so unlike the legacy
        // route, a wrapper does exist after a refusal. What must hold is that
        // it is empty: a reserved name is inert, a populated one would mean a
        // tree was rotated out from under a transition that then failed.
        for wrapper in wrapper_quarantines(&fixture) {
            assert_eq!(
                fs::read_dir(&wrapper).unwrap().count(),
                0,
                "a refused transition rotated a tree into {}",
                wrapper.display()
            );
        }
        match corruption {
            "missing" => assert!(!path.exists()),
            _ => assert_eq!(fs::read(&path).unwrap(), b"corrupt"),
        }
    }
}

/// Driver variant that lets the caller mutate during the transaction trigger,
/// the coordinated stand-in for the legacy `AfterTransactionTriggers`
/// checkpoint.
fn run_active_reblit_with_transaction(
    fixture: &CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    transaction: impl FnOnce(),
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
        |_view| {
            transaction();
            Ok::<(), TriggerEffectError>(())
        },
        |_view| Ok::<(), TriggerEffectError>(()),
    )
}

// Ported from `active_reblit_rejects_same_inode_state_id_rewrite_before_exchange`
// and `active_reblit_rejects_same_content_new_state_id_inode`.
//
// The candidate's published state ID is retained by descriptor, so neither a
// rewrite through the same inode nor a same-bytes replacement at a new inode
// may pass. The two shapes exist because they defeat different naive checks: a
// content comparison misses the rewrite, an inode comparison misses nothing but
// a path comparison misses the substitution.
#[test]
fn coordinated_active_reblit_rejects_candidate_state_id_substitution() {
    for shape in ["same-inode-rewrite", "new-inode-same-content"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
        let live = fixture.installation.root.join("usr");
        let live_inode = fs::symlink_metadata(&live).unwrap().ino();
        let candidate = fixture.candidate_path.join(".stateID");
        let displaced = fixture.candidate_path.join(".stateID.retained");

        let error = run_active_reblit_with_transaction(&fixture, identity, authority, move || match shape {
            "same-inode-rewrite" => fs::write(&candidate, b"9").unwrap(),
            _ => {
                let contents = fs::read(&candidate).unwrap();
                fs::rename(&candidate, &displaced).unwrap();
                write_canonical_file(&candidate, &contents);
            }
        })
        .err()
        .expect("a substituted candidate state ID must fail the transition");

        // Caught as post-effect evidence: the trigger ran, and the coordinator
        // refused to advance over what it did.
        assert_eq!(error.stage(), "transaction triggers", "{shape}: {error:#?}");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("PostEffectEvidence") && rendered.contains("revalidate retained state ID"),
            "{shape} was not caught by retained state-ID revalidation: {error:#?}"
        );

        // The refusal is before the exchange, so the old tree is still live.
        assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), live_inode);
        for wrapper in wrapper_quarantines(&fixture) {
            assert_eq!(fs::read_dir(&wrapper).unwrap().count(), 0, "{shape} rotated a tree");
        }
    }
}

// Ported from `active_reblit_exchange_preflight_rejects_last_moment_state_id_replacement`.
//
// The same substitution, but armed to land in the window immediately before the
// retained exchange rename — after every earlier check has already passed. The
// distinguishing evidence is `outcome: NotApplied`: the exchange syscall did
// not happen, so the old tree is live because it was never replaced, not
// because something put it back.
#[test]
fn coordinated_active_reblit_exchange_refuses_a_last_moment_state_id_replacement() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
    let live = fixture.installation.root.join("usr");
    let live_inode = fs::symlink_metadata(&live).unwrap().ino();
    let candidate = fixture.candidate_path.join(".stateID");
    let displaced = fixture.candidate_path.join(".stateID.original");
    let witness = displaced.clone();

    arm_before_retained_exchange_rename(move || {
        let contents = fs::read(&candidate).unwrap();
        fs::rename(&candidate, &displaced).unwrap();
        write_canonical_file(&candidate, &contents);
    });

    let error = run_active_reblit(&fixture, identity, authority)
        .err()
        .expect("a last-moment state-ID replacement must fail the exchange");

    assert_eq!(error.stage(), "/usr exchange", "{error:#?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("NotApplied"),
        "the exchange must report NotApplied, not an applied-then-reversed outcome: {error:#?}"
    );
    assert!(
        rendered.contains("revalidate retained state ID"),
        "the exchange was not refused by retained state-ID revalidation: {error:#?}"
    );

    // Never exchanged: the original inode is still live.
    assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), live_inode);
    // The hook did run — otherwise this test would prove nothing.
    assert!(witness.is_file(), "the substitution hook never fired");
}
