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
