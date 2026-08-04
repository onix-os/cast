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

/// Driver variant whose hook runs during the *system* trigger — after the usr
/// exchange, so the live tree and the candidate are the same inode by then.
fn run_active_reblit_with_system(
    fixture: &CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    system: impl FnOnce(),
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
        |_view| {
            system();
            Ok::<(), TriggerEffectError>(())
        },
    )
}

// Ported from
// `active_reblit_system_boundary_corruption_reverses_and_preserves_bad_candidate`.
//
// System triggers run after the exchange, so a trigger that corrupts the live
// `.stateID` is corrupting the tree this transition just published. The legacy
// route reversed the exchange itself and reported `StatefulTransitionUsrRestored`.
// The coordinated route does not self-reverse in the forward prefix: it refuses
// to record `SystemTriggersComplete` and parks the journal, leaving the
// reversal to recovery. What both routes agree on is that a corrupted live tree
// is never allowed to become a completed transition.
#[test]
fn coordinated_active_reblit_refuses_a_system_trigger_that_corrupts_the_live_state_id() {
    for mutation in ["rewrite", "remove", "replace"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
        let live = fixture.installation.root.join("usr/.stateID");
        let retained = fixture.installation.root.join("usr/.stateID.retained");
        let ran = std::cell::Cell::new(false);

        let error = run_active_reblit_with_system(&fixture, identity, authority, || {
            ran.set(true);
            match mutation {
                "rewrite" => fs::write(&live, b"9").unwrap(),
                "remove" => fs::remove_file(&live).unwrap(),
                "replace" => {
                    let contents = fs::read(&live).unwrap();
                    fs::rename(&live, &retained).unwrap();
                    write_canonical_file(&live, &contents);
                }
                _ => unreachable!(),
            }
        })
        .err()
        .expect("a corrupted live state ID must fail the transition");

        assert!(ran.get(), "{mutation}: the system trigger never ran");
        assert_eq!(error.stage(), "system triggers", "{mutation}: {error:#?}");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("PostEffectEvidence"),
            "{mutation} was not caught as post-effect evidence: {error:#?}"
        );
    }
}

// Boundary proof: the coordinated route uses the staging-wrapper *reservation*
// but never the legacy rotation *exchange*.
//
// This is the fact that decides whether the legacy rotation tests get ported or
// deleted, so it is asserted rather than left as a note. `rotate_active_reblit_
// staging` has exactly one caller — `client/core/stateful_transition.rs`, the
// legacy route — and the fault points inside it are therefore unreachable here.
//
// If someone later wires the rotation exchange into the coordinated path, this
// test fails and says so. It is expected to be deleted along with
// `legacy_lifecycle::rotate` itself.
#[test]
fn coordinated_active_reblit_never_reaches_the_legacy_rotation_exchange() {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority(CandidateKind::ActiveReblit, PreviousKind::Active);
    let exchanged = std::rc::Rc::new(std::cell::Cell::new(false));
    let hook = std::rc::Rc::clone(&exchanged);
    crate::transition_identity::staging_wrapper_rotation::arm_before_staging_wrapper_exchange(move || {
        hook.set(true);
    });
    arm_staging_wrapper_rotation_faults([
        WrapperFaultPoint::OriginalPostSync,
        WrapperFaultPoint::FinalRevalidation,
        WrapperFaultPoint::BeforeExchange,
    ]);

    run_active_reblit(&fixture, identity, authority)
        .expect("forward prefix")
        .complete_active_reblit_without_boot()
        .expect("no-boot completion");

    let remaining = crate::transition_identity::staging_wrapper_rotation::staging_wrapper_rotation_faults_remaining();
    arm_staging_wrapper_rotation_faults([]);

    // Every armed fault is still armed, and the exchange hook never ran: a
    // clean run here means the code path was not taken, not that it coped.
    assert_eq!(
        remaining, 3,
        "the coordinated route entered the legacy rotation exchange"
    );
    assert!(!exchanged.get(), "the legacy before-exchange hook fired");
}

/// The parked previous slot, located by scanning rather than by reconstructing
/// the tree token (which is only reachable through the transition record).
fn parked_previous_slots(fixture: &CoordinatorFixture) -> Vec<PathBuf> {
    let prefix = format!(".archived-candidate-slot-{}-", fixture.previous_state);
    // `root_path` resolves under `.cast/root`, not under the live root.
    let mut paths = fs::read_dir(fixture.installation.root_path(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with(&prefix))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

// Ported from
// `every_single_active_previous_slot_parking_fault_resumes_without_a_second_move`.
//
// Unlike the staging-wrapper rotation, the previous-slot parking *is* on the
// coordinated path: every one of the nine fault points is reached, and the
// transition resumes through each of them to completion. The `fired` assertion
// is the load-bearing one — without it a green run would be indistinguishable
// from the parking code never running, which is exactly how the wrapper-rotation
// faults looked before they were checked.
//
// "Without a second move" is asserted structurally: exactly one parked slot at
// index 0, holding exactly one entry. A retry that re-applied the move would
// leave a second slot at index 1 or a second marker beside the first.
#[test]
fn coordinated_active_reblit_resumes_every_previous_slot_parking_fault_without_a_second_move() {
    use crate::transition_identity::RetainedActivePreviousSlotParkingFaultPoint as SlotPoint;
    let points = [
        SlotPoint::MarkerPreSync,
        SlotPoint::WrapperPreSync,
        SlotPoint::RootsPreSync,
        SlotPoint::BeforeRename,
        SlotPoint::AfterRename,
        SlotPoint::MarkerPostSync,
        SlotPoint::WrapperPostSync,
        SlotPoint::RootsPostSync,
        SlotPoint::FinalRevalidation,
    ];

    for point in points {
        let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
        let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
        let marker_inode = fs::symlink_metadata(fixture.installation.root.join("usr/.cast-tree-id"))
            .unwrap()
            .ino();

        arm_active_previous_slot_parking_faults([point]);
        let completed = run_active_reblit(&fixture, identity, authority)
            .unwrap_or_else(|error| panic!("{point:?} was not resumed in the forward prefix: {error:#?}"))
            .complete_active_reblit_without_boot();
        let remaining = crate::transition_identity::active_previous_slot_parking_faults_remaining();
        arm_active_previous_slot_parking_faults([]);

        assert_eq!(remaining, 0, "{point:?} was never reached — this run proves nothing");
        completed.unwrap_or_else(|error| panic!("{point:?} was not resumed at completion: {error:#?}"));

        // The slot moved exactly once: canonical gone, one parked slot, one
        // entry inside it, and that entry is the original marker inode.
        assert!(!canonical.exists(), "{point:?} left the canonical slot behind");
        let parked = parked_previous_slots(&fixture);
        assert_eq!(parked.len(), 1, "{point:?} parked the slot more than once: {parked:?}");
        let entries = fs::read_dir(&parked[0]).unwrap().collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "{point:?} left extra entries in the parked slot");
        assert_eq!(
            fs::symlink_metadata(entries[0].as_ref().unwrap().path()).unwrap().ino(),
            marker_inode,
            "{point:?} parked a different marker inode"
        );
    }
}

// Ported from `active_previous_slot_scan_skips_every_foreign_occupant_kind`.
//
// The parking scan walks indexed names looking for a free one. Every occupant
// kind must be stepped over without being opened, followed, or replaced — a
// scan that stat'd through the symlink or tried to reuse the directory would
// either escape the roots or adopt a stranger's wrapper as its own slot.
// `keeps_wrong_wrapper_mode_untouched` covers a wrong-mode directory; the file,
// symlink and FIFO kinds are only covered here.
#[test]
fn coordinated_active_reblit_slot_scan_skips_every_foreign_occupant_kind() {
    use std::os::unix::fs::FileTypeExt as _;

    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    let marker_inode = fs::symlink_metadata(fixture.installation.root.join("usr/.cast-tree-id"))
        .unwrap()
        .ino();

    // The token is only reachable through the transition record, and the live
    // marker cannot be re-adopted to recover it (this fixture has already
    // hardlinked it, so `adopt_or_create_before_journal` refuses the two-link
    // marker). Read it back off the slot name the fixture itself created.
    let token = fs::read_dir(&canonical)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find_map(|name| {
            name.strip_prefix(&format!(".cast-state-slot-{}-", fixture.previous_state))
                .map(str::to_owned)
        })
        .expect("the two-link fixture planted a canonical previous slot");
    let parking = |index: usize| {
        fixture.installation.root_path(format!(
            ".archived-candidate-slot-{}-{token}-{index}",
            fixture.previous_state
        ))
    };

    write_canonical_file(&parking(0), b"regular occupant");
    std::os::unix::fs::symlink("missing-target", parking(1)).unwrap();
    nix::unistd::mkfifo(&parking(2), nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    create_canonical_directory(&parking(3));

    let completion = run_active_reblit(&fixture, identity, authority)
        .expect("forward prefix steps over foreign occupants")
        .complete_active_reblit_without_boot();

    // The parking itself succeeds; only the *commit cleanup* declines to finish
    // with strangers sitting in the indexed namespace, deferring to recovery
    // rather than acting on entries it cannot account for. The legacy route
    // completed inline here — this is the same fail-closed shift seen at the
    // other dispositions, not a parking failure.
    let deferred = format!("{completion:?}");
    assert!(
        deferred.contains("CommitCleanupDeferred"),
        "expected cleanup to defer over foreign occupants, got: {deferred}"
    );

    // Every occupant is exactly as it was left.
    assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
    assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
    assert!(fs::symlink_metadata(parking(2)).unwrap().file_type().is_fifo());
    assert_eq!(
        fs::symlink_metadata(parking(3)).unwrap().permissions().mode() & 0o7777,
        0o755
    );

    // The slot landed at the first free index, carrying the original marker.
    assert!(!canonical.exists(), "the canonical slot was left behind");
    assert_eq!(
        fs::symlink_metadata(parking(4).join(format!(".cast-state-slot-{}-{token}", fixture.previous_state)))
            .unwrap()
            .ino(),
        marker_inode
    );
}

/// The tree token, read back off the canonical slot the two-link fixture
/// planted. It cannot be recovered by re-adopting the live marker: that marker
/// is already at `links=2` here and `adopt_or_create_before_journal` refuses it.
fn previous_slot_token(fixture: &CoordinatorFixture) -> String {
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    fs::read_dir(&canonical)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find_map(|name| {
            name.strip_prefix(&format!(".cast-state-slot-{}-", fixture.previous_state))
                .map(str::to_owned)
        })
        .expect("the two-link fixture planted a canonical previous slot")
}

fn parked_slot_path(fixture: &CoordinatorFixture, token: &str, index: usize) -> PathBuf {
    fixture.installation.root_path(format!(
        ".archived-candidate-slot-{}-{token}-{index}",
        fixture.previous_state
    ))
}

fn slot_marker_path(wrapper: &Path, fixture: &CoordinatorFixture, token: &str) -> PathBuf {
    wrapper.join(format!(".cast-state-slot-{}-{token}", fixture.previous_state))
}

// Ported from `active_previous_slot_substitution_never_moves_or_adopts_the_foreign_wrapper`.
//
// The canonical slot is renamed away and a stranger's directory takes its name
// in the window before the parking rename. The parking must neither move the
// stranger nor adopt it as its own slot — the two failure modes that a
// path-keyed (rather than descriptor-keyed) parking would fall into.
#[test]
fn coordinated_active_reblit_refuses_a_substituted_previous_slot() {
    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let token = previous_slot_token(&fixture);
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    let displaced = fixture.installation.root_path("retained-active-slot-race");
    let marker_inode = fs::symlink_metadata(slot_marker_path(&canonical, &fixture, &token))
        .unwrap()
        .ino();
    let (hook_canonical, hook_displaced) = (canonical.clone(), displaced.clone());

    crate::transition_identity::arm_before_active_previous_slot_parking_rename(move || {
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::create_dir(&hook_canonical).unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(hook_canonical.join("foreign"), b"racing wrapper").unwrap();
    });

    let error = run_active_reblit(&fixture, identity, authority)
        .err()
        .expect("a substituted previous slot must refuse the reservation");

    assert_eq!(error.stage(), "ActiveReblit reservation", "{error:#?}");
    let rendered = format!("{error:?}");
    // `Ambiguous` is the honest outcome: the parking cannot prove whether its
    // move happened, so it refuses rather than guessing in either direction.
    assert!(
        rendered.contains("Ambiguous") && rendered.contains("NamespaceMismatch"),
        "substitution was not refused as an ambiguous namespace mismatch: {error:#?}"
    );

    // The stranger is untouched and was never adopted.
    assert_eq!(fs::read(canonical.join("foreign")).unwrap(), b"racing wrapper");
    assert!(
        !parked_slot_path(&fixture, &token, 0).exists(),
        "the foreign wrapper was parked as this transition's slot"
    );
    // The real slot is intact wherever the race left it.
    assert_eq!(
        fs::symlink_metadata(slot_marker_path(&displaced, &fixture, &token))
            .unwrap()
            .ino(),
        marker_inode
    );
}

// Ported from `active_previous_slot_parking_adopts_an_exact_externally_applied_move`.
//
// The counterpart to the refusal above: when the racing move is *exactly* the
// one the parking intended, it must be adopted rather than refused or repeated.
// This is what makes the parking crash-safe — a move that landed before the
// crash is indistinguishable from one an external actor performed, and both
// must resume forward.
#[test]
fn coordinated_active_reblit_adopts_an_exact_externally_applied_slot_move() {
    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let token = previous_slot_token(&fixture);
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    let parked = parked_slot_path(&fixture, &token, 0);
    let marker_inode = fs::symlink_metadata(slot_marker_path(&canonical, &fixture, &token))
        .unwrap()
        .ino();
    let (hook_canonical, hook_parked) = (canonical.clone(), parked.clone());

    crate::transition_identity::arm_before_active_previous_slot_parking_rename(move || {
        fs::rename(hook_canonical, hook_parked).unwrap();
    });

    run_active_reblit(&fixture, identity, authority)
        .expect("an exact external move is adopted, not refused")
        .complete_active_reblit_without_boot()
        .expect("completion proceeds over the adopted move");

    assert!(!canonical.exists(), "the canonical slot survived adoption");
    assert_eq!(
        fs::symlink_metadata(slot_marker_path(&parked, &fixture, &token))
            .unwrap()
            .ino(),
        marker_inode,
        "the adopted slot does not hold the original marker"
    );
    // Adopted, not repeated: no second slot at the next index.
    assert!(
        !parked_slot_path(&fixture, &token, 1).exists(),
        "the parking moved a second time after adopting"
    );
}

// Ported from `already_parked_previous_slot_with_foreign_canonical_name_fails_closed`.
//
// The slot is already parked *and* a stranger occupies the canonical name. That
// is not the adoptable case above — the namespace carries evidence of two
// different actors — so the reservation must fail closed rather than pick a
// reading.
#[test]
fn coordinated_active_reblit_fails_closed_on_a_parked_slot_with_a_foreign_canonical() {
    let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
    let token = previous_slot_token(&fixture);
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    let parked = parked_slot_path(&fixture, &token, 0);
    let marker_inode = fs::symlink_metadata(slot_marker_path(&canonical, &fixture, &token))
        .unwrap()
        .ino();

    fs::rename(&canonical, &parked).unwrap();
    fs::create_dir(&canonical).unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(canonical.join("foreign"), b"canonical occupant").unwrap();

    let error = run_active_reblit(&fixture, identity, authority)
        .err()
        .expect("a parked slot with a foreign canonical must fail closed");

    assert_eq!(error.stage(), "ActiveReblit reservation", "{error:#?}");
    assert!(
        format!("{error:?}").contains("NamespaceMismatch"),
        "the ambiguous namespace was not reported as a mismatch: {error:#?}"
    );

    // Neither the stranger nor the real parked slot was disturbed.
    assert_eq!(fs::read(canonical.join("foreign")).unwrap(), b"canonical occupant");
    assert_eq!(
        fs::symlink_metadata(slot_marker_path(&parked, &fixture, &token))
            .unwrap()
            .ino(),
        marker_inode
    );
}

// Ported from `active_reblit_rejects_a_slot_moved_back_to_canonical_after_triggers`
// and `active_reblit_reversal_cannot_report_success_after_parked_slot_is_moved_back`.
//
// By the time either trigger runs the slot is already parked, so renaming it
// back to the canonical name undoes a move the transition has durable evidence
// for. Neither trigger boundary may advance over that: the parked slot is part
// of the retained namespace, not incidental scenery, and a transition that
// accepted the un-move would report success over a namespace it no longer
// describes.
#[test]
fn coordinated_active_reblit_rejects_a_slot_moved_back_to_canonical() {
    for boundary in ["transaction", "system"] {
        let (fixture, identity, authority) = fixture_with_exchange_authority_and_previous_slot();
        let token = previous_slot_token(&fixture);
        let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
        let parked = parked_slot_path(&fixture, &token, 0);
        let marker_inode = fs::symlink_metadata(slot_marker_path(&canonical, &fixture, &token))
            .unwrap()
            .ino();
        let (hook_parked, hook_canonical) = (parked.clone(), canonical.clone());
        let unmove = move || {
            fs::rename(&hook_parked, &hook_canonical).unwrap();
        };

        let error = match boundary {
            "transaction" => run_active_reblit_with_transaction(&fixture, identity, authority, unmove),
            _ => run_active_reblit_with_system(&fixture, identity, authority, unmove),
        }
        .err()
        .unwrap_or_else(|| panic!("{boundary}: un-moving the parked slot must fail the transition"));

        assert_eq!(
            error.stage(),
            if boundary == "transaction" {
                "transaction triggers"
            } else {
                "system triggers"
            },
            "{boundary}: {error:#?}"
        );
        assert!(
            format!("{error:?}").contains("PostEffectEvidence"),
            "{boundary}: the un-move was not caught as post-effect evidence: {error:#?}"
        );

        // The un-move stands as the trigger left it — the transition refuses to
        // advance, it does not fight the mutation.
        assert!(!parked.exists(), "{boundary}: the parked slot reappeared");
        assert_eq!(
            fs::symlink_metadata(slot_marker_path(&canonical, &fixture, &token))
                .unwrap()
                .ino(),
            marker_inode,
            "{boundary}: the moved-back slot lost its marker"
        );
    }
}
