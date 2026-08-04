// Adversarial namespace-escape proofs for coordinated candidate metadata.
//
// `finish_candidate_prepare` is the only seam that *writes into* the candidate
// tree before the usr exchange, so it is the only seam an attacker-controlled
// candidate can use to reach outside itself. These proofs drive the composed
// NewState prefix and assert on the filesystem: a symlinked `lib`, a symlinked
// output name, a pre-existing output inode, and a final-name race must each
// fail the transition without following the link, without truncating the
// external target, and without replacing the occupying inode.
//
// The legacy adapter quarantined the candidate on failure, so its ancestors of
// these tests asserted against a quarantine directory. The coordinated route
// has no such step: the journal simply stops at `CandidatePrepareStarted` and
// recovery owns the disposition, so the escape artifacts are asserted in place.

/// Drive the composed NewState prefix over a fixture whose candidate tree has
/// already been made adversarial, and require it to fail at metadata
/// publication for exactly the named reason.
///
/// The refusal variant is asserted, not merely the stage: an escape that failed
/// for an unrelated reason (a fixture mistake, a database refusal) would
/// otherwise satisfy a stage-only assertion and prove nothing about the guard.
/// The source is boxed to keep authority-bearing types off the crate facade, so
/// the discrimination is made on its rendered form.
fn expect_metadata_refusal(
    fixture: &CoordinatorFixture,
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    expected: &[&str],
) -> NewStateForwardError {
    let previous = NewStatePrevious::Active(fixture.previous_state);
    let error = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        previous,
        &[],
        "candidate metadata escape slice",
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
    .expect_err("adversarial candidate tree must fail the forward prefix");
    assert_eq!(
        error.stage(),
        "candidate metadata publication",
        "escape must be refused at publication, not later: {error:#?}"
    );
    let rendered = format!("{error:?}");
    for fragment in expected {
        assert!(rendered.contains(fragment), "refusal missing `{fragment}`: {error:#?}");
    }
    error
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

#[test]
fn coordinated_candidate_metadata_never_follows_lib_or_os_info_symlinks() {
    for escape in ["lib", "os-info.json"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
        let live_identity = inode_identity(&fixture.installation.root.join("usr"));
        let external = fixture.installation.root.join(format!("external-{escape}-target"));
        let candidate_lib = fixture.candidate_path.join("lib");

        if escape == "lib" {
            create_canonical_directory(&external);
            write_canonical_file(&external.join("sentinel"), b"external-directory");
            std::os::unix::fs::symlink(&external, &candidate_lib).unwrap();
        } else {
            write_canonical_file(&external, b"external-input");
            create_canonical_directory(&candidate_lib);
            std::os::unix::fs::symlink(&external, candidate_lib.join("os-info.json")).unwrap();
        }

        // A symlinked `lib` is refused as an unsafe output directory; a
        // symlinked `os-info.json` is refused as an unsafe policy input.
        let refusal = if escape == "lib" {
            ["UnsafeDirectory", "\"symlink\""]
        } else {
            ["UnsafeInput", "\"symlink\""]
        };
        expect_metadata_refusal(&fixture, identity, authority, &refusal);

        // The live tree is never a party to a candidate-side escape.
        assert_eq!(inode_identity(&fixture.installation.root.join("usr")), live_identity);

        if escape == "lib" {
            // Publication through the symlink would have written both outputs
            // into the external directory and left the sentinel beside them.
            assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"external-directory");
            assert!(!external.join("os-release").exists());
            assert!(!external.join("system-model.glu").exists());
            assert!(fs::symlink_metadata(&candidate_lib).unwrap().file_type().is_symlink());
        } else {
            // Reading os-info through the symlink would have consumed the
            // external file as policy input.
            assert_eq!(fs::read(&external).unwrap(), b"external-input");
            assert!(
                fs::symlink_metadata(candidate_lib.join("os-info.json"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }
}

#[test]
fn coordinated_candidate_metadata_never_follows_output_symlinks() {
    for output in ["os-release", "system-model.glu"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
        let external = fixture
            .installation
            .root
            .join(format!("external-{output}-symlink-target"));
        write_canonical_file(&external, format!("external-{output}").as_bytes());
        let external_identity = inode_identity(&external);
        let lib = fixture.candidate_path.join("lib");
        create_canonical_directory(&lib);
        std::os::unix::fs::symlink(&external, lib.join(output)).unwrap();

        expect_metadata_refusal(&fixture, identity, authority, &["DestinationExists", "\"symlink\""]);

        // Following the link would have replaced the target's bytes in place
        // (same inode) or unlinked it for a fresh one (different inode).
        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("external-{output}").as_bytes());
        assert!(fs::symlink_metadata(lib.join(output)).unwrap().file_type().is_symlink());
    }
}

#[test]
fn coordinated_candidate_metadata_never_replaces_existing_output_inodes() {
    for output in ["os-release", "system-model.glu"] {
        for hardlinked in [false, true] {
            let (fixture, identity, authority) =
                fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
            let lib = fixture.candidate_path.join("lib");
            create_canonical_directory(&lib);
            let candidate_output = lib.join(output);

            let external = fixture
                .installation
                .root
                .join(format!("external-{output}-{hardlinked}"));
            if hardlinked {
                // A multiply-linked occupant reaches outside the candidate: an
                // in-place write would rewrite the external name's bytes too.
                write_canonical_file(&external, format!("external-{output}").as_bytes());
                fs::hard_link(&external, &candidate_output).unwrap();
            } else {
                write_canonical_file(&candidate_output, format!("candidate-occupant-{output}").as_bytes());
            }
            let occupant_identity = inode_identity(&candidate_output);
            let occupant_bytes = fs::read(&candidate_output).unwrap();

            // The occupant is a plain file either way; the hardlinked case
            // differs only in that replacing it would reach outside the tree.
            expect_metadata_refusal(
                &fixture,
                identity,
                authority,
                &["DestinationExists", "\"regular-file\""],
            );

            assert_eq!(inode_identity(&candidate_output), occupant_identity);
            assert_eq!(fs::read(&candidate_output).unwrap(), occupant_bytes);
            if hardlinked {
                assert_eq!(inode_identity(&external), occupant_identity);
                assert_eq!(fs::read(&external).unwrap(), occupant_bytes);
            }
        }
    }
}

#[test]
fn coordinated_candidate_metadata_final_name_races_are_no_replace() {
    for output in ["os-release", "system-model.glu"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
        let lib = fixture.candidate_path.join("lib");
        create_canonical_directory(&lib);
        let external = fixture.installation.root.join(format!("external-{output}-race"));
        write_canonical_file(&external, format!("racing-{output}").as_bytes());
        let external_identity = inode_identity(&external);

        // Occupy the final name in the window between the staged write and the
        // rename that publishes it, so publication cannot be no-replace by
        // accident of an empty directory.
        let hook_external = external.clone();
        let hook_output = lib.join(output);
        crate::transition_identity::candidate_metadata::arm_before_publication(output, move || {
            fs::hard_link(&hook_external, &hook_output).unwrap();
        });

        // EEXIST from the publishing rename is the proof that it is
        // no-replace: a replacing rename would have succeeded here.
        expect_metadata_refusal(
            &fixture,
            identity,
            authority,
            &["PublicationCollision", "AlreadyExists"],
        );

        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("racing-{output}").as_bytes());
        assert_eq!(inode_identity(&lib.join(output)), external_identity);
    }
}

// The retained metadata proof must reject every shape of post-trigger mutation,
// not only the rename-and-replace the substitution proofs already cover. A
// rewrite in particular keeps the inode, so it is the shape most likely to slip
// past an identity-only revalidation.
#[test]
fn coordinated_retained_metadata_proof_rejects_every_post_trigger_mutation() {
    for mutation in ["rewrite", "delete", "replace", "hardlink", "substitute"] {
        let (fixture, identity, authority) =
            fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
        let output = fixture.candidate_path.join("lib/system-model.glu");
        let external = fixture.installation.root.join(format!("external-proof-{mutation}"));
        let ran = std::cell::Cell::new(false);

        let error = execute_new_state_forward(
            identity,
            authority,
            &fixture.database,
            NewStatePrevious::Active(fixture.previous_state),
            &[],
            "post-trigger mutation slice",
            false,
            |_| {
                crate::transition_identity::CandidateMetadataOutputs::from_policy(
                    COORDINATOR_OS_RELEASE,
                    crate::system_model::snapshot_authorities(),
                    COORDINATOR_SYSTEM_SNAPSHOT,
                )
            },
            |_view| {
                ran.set(true);
                match mutation {
                    // Rewritten in place: same inode, same mode, same link
                    // count. Only content-bound evidence can catch this one.
                    "rewrite" => fs::write(&output, b"rewritten-after-trigger").unwrap(),
                    "delete" => fs::remove_file(&output).unwrap(),
                    // The replacement is written canonically so the refusal
                    // must come from its identity, not from a stray mode.
                    "replace" => {
                        write_canonical_file(&external, b"replacement-after-trigger");
                        fs::remove_file(&output).unwrap();
                        fs::hard_link(&external, &output).unwrap();
                    }
                    "hardlink" => fs::hard_link(&output, &external).unwrap(),
                    // A fresh canonical file at the canonical name: singly
                    // linked, correctly moded, only the inode differs. Neither
                    // the mode nor the link-count guard can see this one.
                    "substitute" => {
                        let bytes = fs::read(&output).unwrap();
                        fs::remove_file(&output).unwrap();
                        write_canonical_file(&output, &bytes);
                    }
                    _ => unreachable!(),
                }
                Ok::<(), TriggerEffectError>(())
            },
            |_view| Ok::<(), TriggerEffectError>(()),
        )
        .expect_err("post-trigger metadata mutation must fail the forward prefix");

        assert!(ran.get(), "transaction trigger did not run for {mutation}");
        assert_eq!(
            error.stage(),
            "transaction triggers",
            "{mutation} must be caught as post-effect evidence: {error:#?}"
        );
        // Which guard catches each shape is part of the proof: a shape caught
        // only by an incidental mode or link-count check would still pass a
        // "did it fail" assertion while leaving the identity guard untested.
        let expected = match mutation {
            // Content-bound evidence: the inode is unchanged for `rewrite`, and
            // the mode and link count are canonical for `substitute`.
            "rewrite" | "delete" | "substitute" => "FileChanged",
            // Both reach outside the candidate, so both raise the link count.
            "replace" | "hardlink" => "UnexpectedHardlink",
            _ => unreachable!(),
        };
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("PostEffectEvidence") && rendered.contains(expected),
            "{mutation} was not refused by `{expected}`: {error:#?}"
        );
    }
}

// System triggers run after the usr exchange, so by then the candidate tree is
// the live tree and a trigger that mutates its own metadata is mutating what
// the retained proof is bound to. The forward prefix must refuse to record
// SystemTriggersComplete over it, leaving the journal parked for recovery to
// reverse rather than committing a tree whose metadata no longer matches.
#[test]
fn coordinated_retained_metadata_proof_rejects_post_system_trigger_mutation() {
    let (fixture, identity, authority) = fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let live_release = fixture.installation.root.join("usr/lib/os-release");
    let ran = std::cell::Cell::new(false);

    let error = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        NewStatePrevious::Active(fixture.previous_state),
        &[],
        "post-system-trigger mutation slice",
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
            ran.set(true);
            fs::write(&live_release, b"rewritten-after-system-trigger").unwrap();
            Ok::<(), TriggerEffectError>(())
        },
    )
    .expect_err("post-system-trigger metadata mutation must fail the forward prefix");

    assert!(ran.get(), "system trigger did not run");
    assert_eq!(error.stage(), "system triggers", "{error:#?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("PostEffectEvidence") && rendered.contains("FileChanged"),
        "post-system-trigger mutation was not caught by the retained proof: {error:#?}"
    );
    // The proof is descriptor-bound, so it names the staging path it was taken
    // at even though the write went to the live path. That the write was seen
    // at all is itself the evidence that the exchange already happened and the
    // two names are one inode.
    assert!(
        rendered.contains("staging/usr/lib/os-release"),
        "refusal did not name the retained candidate path: {error:#?}"
    );
    // The journal is left parked at the started phase, not advanced over the
    // mutation, so recovery owns the reversal.
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::SystemTriggersStarted,
        11,
    );
}

// Whole-tree substitution: the candidate directory is renamed away and a
// foreign directory takes its name in the window before the runtime proof. The
// identity is descriptor-bound, so the substitution must be refused outright —
// and, critically, neither tree may be decorated. Decorating the replacement
// would write this transition's metadata into an attacker's directory;
// decorating the displaced original would leave metadata on a tree the journal
// no longer names.
#[test]
fn coordinated_candidate_substitution_before_metadata_decorates_neither_tree() {
    let (fixture, identity, authority) = fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let live_identity = inode_identity(&fixture.installation.root.join("usr"));
    let candidate = fixture.candidate_path.clone();
    let displaced = fixture.installation.root.join("displaced-metadata-candidate");

    let hook_candidate = candidate.clone();
    let hook_displaced = displaced.clone();
    arm_before_finish_candidate_runtime_proof(move || {
        fs::rename(&hook_candidate, &hook_displaced).unwrap();
        create_canonical_directory(&hook_candidate);
        write_canonical_file(&hook_candidate.join("foreign"), b"replacement-candidate");
    });

    let error = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        NewStatePrevious::Active(fixture.previous_state),
        &[],
        "candidate substitution slice",
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
    .expect_err("whole-tree candidate substitution must fail the forward prefix");

    // The retained directory descriptor no longer resolves to the name it was
    // taken at, which is what a whole-tree rename looks like from the inside.
    assert_eq!(error.stage(), "candidate metadata publication", "{error:#?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("TreeMarker") && rendered.contains("DirectoryChanged"),
        "substitution was not caught by the descriptor-bound marker: {error:#?}"
    );

    // Neither tree carries this transition's metadata.
    assert!(!displaced.join("lib").exists(), "the displaced original was decorated");
    assert!(!candidate.join("lib").exists(), "the replacement was decorated");
    assert_eq!(
        fs::read(candidate.join("foreign")).unwrap(),
        b"replacement-candidate",
        "the replacement tree was mutated"
    );
    // The displaced original is still intact and still the real candidate.
    assert_eq!(
        fs::read(displaced.join("payload-sentinel")).unwrap(),
        NEW_STATE_PAYLOAD_SENTINEL
    );
    assert_eq!(inode_identity(&fixture.installation.root.join("usr")), live_identity);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::CandidatePrepareStarted,
        4,
    );
}

// The success counterpart to the refusals above. Published metadata is not
// merely present with the right bytes — it is sealed: a regular file owned by
// the running user, canonically moded, and singly linked. Each of those is a
// property some refusal above depends on, so a regression that published a
// group-writable or multiply-linked output would quietly weaken every one of
// them while leaving the byte assertions green.
#[test]
fn coordinated_published_candidate_metadata_is_sealed() {
    let (fixture, identity, authority) = fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let scopes = std::cell::RefCell::new(Vec::new());

    let (complete, allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        NewStatePrevious::Active(fixture.previous_state),
        &[],
        "sealed metadata slice",
        false,
        |_| {
            crate::transition_identity::CandidateMetadataOutputs::from_policy(
                COORDINATOR_OS_RELEASE,
                crate::system_model::snapshot_authorities(),
                COORDINATOR_SYSTEM_SNAPSHOT,
            )
        },
        |_view| {
            scopes.borrow_mut().push("transaction");
            Ok::<(), TriggerEffectError>(())
        },
        |_view| {
            scopes.borrow_mut().push("system");
            Ok::<(), TriggerEffectError>(())
        },
    )
    .expect("clean candidate reaches system-triggers complete");

    assert_eq!(*scopes.borrow(), ["transaction", "system"]);
    assert_record_prefix(
        complete.record(),
        Operation::NewState,
        Phase::SystemTriggersComplete,
        12,
    );

    // Published past the exchange, so the outputs are asserted on the live tree.
    let live_lib = fixture.installation.root.join("usr/lib");
    assert_eq!(fs::read(live_lib.join("os-release")).unwrap(), COORDINATOR_OS_RELEASE);
    assert_eq!(
        fs::read(live_lib.join("system-model.glu")).unwrap(),
        COORDINATOR_SYSTEM_SNAPSHOT
    );
    assert_eq!(
        fs::read(fixture.installation.root.join("usr/.stateID")).unwrap(),
        allocated.to_string().as_bytes()
    );

    for output in ["os-release", "system-model.glu"] {
        let metadata = fs::symlink_metadata(live_lib.join(output)).unwrap();
        assert!(metadata.file_type().is_file(), "{output} is not a regular file");
        assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() }, "{output} owner");
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o644, "{output} mode");
        assert_eq!(metadata.nlink(), 1, "{output} link count");
    }
}

// Ordering proof: the candidate `usr` clone is taken before anything is
// written, so a clone that fails must leave the candidate tree exactly as it
// was found. If decoration ran first, a failed clone would strand a half-built
// `lib` on a tree the transition then abandons.
#[test]
fn coordinated_candidate_clone_failure_precedes_all_metadata_decoration() {
    let (fixture, identity, authority) = fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let candidate = fixture.candidate_path.clone();
    crate::transition_identity::arm_candidate_usr_clone_fault();

    let error = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        NewStatePrevious::Active(fixture.previous_state),
        &[],
        "clone fault slice",
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
    .expect_err("a failed candidate clone must fail the forward prefix");

    crate::transition_identity::assert_candidate_usr_clone_fault_consumed();
    assert_eq!(error.stage(), "candidate metadata publication", "{error:#?}");
    // Not even the containing directory is created before the clone succeeds.
    assert!(!candidate.join("lib").exists(), "a failed clone still created `lib`");
    assert!(!candidate.join("lib/os-release").exists());
    assert!(!candidate.join("lib/system-model.glu").exists());
    // The untouched candidate payload is still the one the fixture staged.
    assert_eq!(
        fs::read(candidate.join("payload-sentinel")).unwrap(),
        NEW_STATE_PAYLOAD_SENTINEL
    );
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::CandidatePrepareStarted,
        4,
    );
}

// Ported from
// `every_live_root_abi_conflict_precedes_candidate_trigger_and_exchange_mutation`.
//
// A foreign entry occupying a root-ABI name — either the canonical link name or
// its `.next` staging name — must be refused before the transition touches
// anything. In the coordinated route that bound is even tighter than the legacy
// one: the refusal comes from taking the pre-journal client authority, so it
// happens before a journal exists, before any trigger runs, and before the
// candidate is prepared. There is no transition to unwind because none was
// created.
#[test]
fn coordinated_root_abi_conflicts_are_refused_before_any_authority_is_taken() {
    for (name, expected) in [
        ("bin", "RootAbiLinkTypeConflict"),
        ("bin.next", "RootAbiStagingConflict"),
    ] {
        let (_temporary, outcome) = prejournal_authority_over_root_entry(name, b"foreign root ABI entry");
        let rendered = outcome
            .err()
            .unwrap_or_else(|| panic!("a foreign entry at `{name}` must refuse the authority"));
        assert!(
            rendered.contains(expected),
            "`{name}` was not refused as {expected}: {rendered}"
        );
        // The refusal names the offending path and what it actually found, so
        // an operator can act on it without re-deriving the ABI table.
        assert!(
            rendered.contains("regular file") && rendered.contains(name),
            "the refusal does not identify the conflicting entry: {rendered}"
        );
    }
}

// Ported from `retained_live_root_abi_rejects_replacement_at_the_exchange_boundary`
// and `retained_absent_root_abi_rejects_appearance_at_the_exchange_boundary`.
//
// Both legacy tests mutate a root-ABI entry in the window before the usr
// exchange — one replacing a retained *present* link, one making an entry
// appear where absence was retained — and require the exchange to refuse
// without disturbing the live tree or the foreign entry.
//
// The coordinated route enforces the same outcome through a broader guard. It
// has no root-ABI-specific pre-exchange comparison (`live_root_abi.revalidate()`
// exists only on the legacy route); instead the retained active-state lease
// covers the whole installation root, so *any* mutation of it during the lease
// fails the exchange closed. That strictly subsumes the root-ABI case: you
// cannot replace a root-ABI entry without changing root metadata.
//
// Asserted as a lease proof rather than as a root-ABI proof, because that is
// what actually holds. Claiming a root-ABI-specific check here would describe a
// guard the coordinated route does not have.
#[test]
fn coordinated_root_abi_mutation_at_the_exchange_boundary_fails_closed() {
    for retained_present in [true, false] {
        let (fixture, identity, authority) = fixture_parts_with_root_abi_mask(
            CandidateKind::NewState,
            PreviousKind::Active,
            true,
            false,
            // Bit 0 is `bin -> usr/bin`. It is a *dangling* symlink in this
            // fixture (no `usr/bin` target), so `Path::exists()` reports false
            // for it — presence must be checked with `symlink_metadata`.
            u8::from(retained_present),
        );
        let authority = authority.expect("exchange fixture retained the client authority");
        let foreign = fixture.installation.root.join("bin");
        assert_eq!(
            fs::symlink_metadata(&foreign).is_ok(),
            retained_present,
            "fixture did not establish retained_present={retained_present}"
        );

        let live = fixture.installation.root.join("usr");
        let live_inode = fs::symlink_metadata(&live).unwrap().ino();
        let hook_foreign = foreign.clone();
        arm_before_retained_exchange_rename(move || {
            let _ = fs::remove_file(&hook_foreign);
            fs::write(&hook_foreign, b"foreign at the exchange boundary").unwrap();
        });

        let error = execute_new_state_forward(
            identity,
            authority,
            &fixture.database,
            NewStatePrevious::Active(fixture.previous_state),
            &[],
            "root abi boundary slice",
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
        .expect_err("a root mutation at the exchange boundary must fail closed");

        assert_eq!(error.stage(), "/usr exchange", "{retained_present}: {error:#?}");
        let rendered = format!("{error:?}");
        // `NotApplied` is the load-bearing part: the exchange syscall never
        // ran, so the live tree is intact because it was never replaced.
        assert!(
            rendered.contains("NotApplied"),
            "{retained_present}: the exchange was applied before refusing: {error:#?}"
        );
        assert!(
            rendered.contains("installation-root metadata changed during retained active-state lease"),
            "{retained_present}: refusal did not come from the root lease: {error:#?}"
        );

        // The live tree is untouched and the stranger is left exactly as it
        // was written — the transition refuses, it does not clean up after the
        // actor that raced it.
        assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), live_inode);
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign at the exchange boundary");
    }
}

// Ported from `previous_archive_never_replaces_a_racing_empty_destination`.
//
// The previous tree is archived by renaming it to `<state>/usr`. If something
// races an empty directory into that destination first, the rename must refuse
// rather than replace it — an empty directory is the shape most likely to look
// safe to overwrite, and overwriting it would destroy whatever the racer was
// about to put there.
//
// Only the no-replace claim is ported. The legacy test also asserted
// `StatefulTransitionUsrRestored` and a restored live `.stateID`, i.e. inline
// reversal; the coordinated route has no such disposition and leaves reversal
// to recovery.
#[test]
fn coordinated_previous_archive_never_replaces_a_racing_empty_destination() {
    let (fixture, identity, authority) = fixture_with_exchange_authority(CandidateKind::NewState, PreviousKind::Active);
    let destination = fixture
        .installation
        .root_path(fixture.previous_state.to_string())
        .join("usr");

    let (complete, _allocated) = execute_new_state_forward(
        identity,
        authority,
        &fixture.database,
        NewStatePrevious::Active(fixture.previous_state),
        &[],
        "racing archive destination slice",
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
    .expect("forward prefix reaches system-triggers complete");

    // Race the empty destination in after the exchange, before the archive.
    fs::create_dir_all(&destination).unwrap();
    let occupant = fs::symlink_metadata(&destination).unwrap().ino();

    let error = complete
        .archive_previous_tree()
        .err()
        .expect("archiving over a racing destination must refuse");

    // The occupant is the same inode and still empty: nothing was moved into
    // it and it was not unlinked and recreated.
    assert_eq!(
        fs::symlink_metadata(&destination).unwrap().ino(),
        occupant,
        "the racing destination was replaced: {error:#?}"
    );
    assert_eq!(
        fs::read_dir(&destination).unwrap().count(),
        0,
        "the previous tree was moved into the racing destination: {error:#?}"
    );
}
