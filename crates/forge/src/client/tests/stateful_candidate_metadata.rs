//! Adversarial proofs for descriptor-bound stateful candidate metadata.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use super::*;

#[test]
fn stateful_candidate_metadata_never_follows_lib_or_os_info_symlinks() {
    for escape in ["lib", "os-info.json"] {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_identity = inode_identity(&installation.root.join("usr"));
        let external = installation.root.join(format!("external-{escape}-target"));
        let candidate_lib = installation.staging_path("usr/lib");

        if escape == "lib" {
            fs::create_dir(&external).unwrap();
            fs::write(external.join("sentinel"), b"external-directory").unwrap();
            symlink(&external, &candidate_lib).unwrap();
        } else {
            fs::write(&external, b"external-input").unwrap();
            create_canonical_candidate_directory(&candidate_lib);
            symlink(&external, candidate_lib.join("os-info.json")).unwrap();
        }

        let error = apply_fresh_candidate(&fixture, |_| Ok(())).unwrap_err();
        let quarantine = assert_preserved_metadata_failure(&fixture, error, live_identity, &[]);

        if escape == "lib" {
            assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"external-directory");
            assert!(!external.join("os-release").exists());
            assert!(!external.join("system-model.glu").exists());
            assert!(
                fs::symlink_metadata(quarantine.join("usr/lib"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        } else {
            assert_eq!(fs::read(&external).unwrap(), b"external-input");
            assert!(
                fs::symlink_metadata(quarantine.join("usr/lib/os-info.json"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }
}

#[test]
fn stateful_candidate_metadata_never_follows_output_symlinks() {
    for output in ["os-release", "system-model.glu"] {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_identity = inode_identity(&installation.root.join("usr"));
        let external = installation.root.join(format!("external-{output}-symlink-target"));
        fs::write(&external, format!("external-{output}")).unwrap();
        let external_identity = inode_identity(&external);
        let lib = installation.staging_path("usr/lib");
        create_canonical_candidate_directory(&lib);
        symlink(&external, lib.join(output)).unwrap();

        let error = apply_fresh_candidate(&fixture, |_| Ok(())).unwrap_err();
        let quarantine = assert_preserved_metadata_failure(&fixture, error, live_identity, &[]);

        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("external-{output}").as_bytes());
        assert!(
            fs::symlink_metadata(quarantine.join("usr/lib").join(output))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn stateful_candidate_metadata_preserves_existing_output_inodes() {
    for output in ["os-release", "system-model.glu"] {
        for hardlinked in [false, true] {
            let fixture = stateful_transition_fixture(false);
            let installation = &fixture.client.installation;
            let live_identity = inode_identity(&installation.root.join("usr"));
            let lib = installation.staging_path("usr/lib");
            let candidate_output = lib.join(output);
            create_canonical_candidate_directory(&lib);

            let external = installation.root.join(format!("external-{output}-{hardlinked}"));
            if hardlinked {
                write_canonical_candidate_file(&external, format!("external-{output}"));
                fs::hard_link(&external, &candidate_output).unwrap();
            } else {
                write_canonical_candidate_file(&candidate_output, format!("candidate-occupant-{output}"));
            }
            let occupant_identity = inode_identity(&candidate_output);
            let occupant_bytes = fs::read(&candidate_output).unwrap();

            let error = apply_fresh_candidate(&fixture, |_| Ok(())).unwrap_err();
            if hardlinked {
                assert_prejournal_hardlink_failure(&fixture, error, live_identity, &candidate_output);
                assert_eq!(inode_identity(&candidate_output), occupant_identity);
                assert_eq!(inode_identity(&external), occupant_identity);
                assert_eq!(fs::read(&candidate_output).unwrap(), occupant_bytes);
                assert_eq!(fs::read(&external).unwrap(), occupant_bytes);
                continue;
            }
            let quarantine = assert_preserved_metadata_failure(&fixture, error, live_identity, &[]);
            let preserved = quarantine.join("usr/lib").join(output);

            assert_eq!(inode_identity(&preserved), occupant_identity);
            assert_eq!(fs::read(&preserved).unwrap(), occupant_bytes);
        }
    }
}

#[test]
fn stateful_candidate_metadata_final_name_races_are_no_replace() {
    for output in ["os-release", "system-model.glu"] {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_identity = inode_identity(&installation.root.join("usr"));
        let external = installation.root.join(format!("external-{output}-race"));
        write_canonical_candidate_file(&external, format!("racing-{output}"));
        let external_identity = inode_identity(&external);
        let hook_external = external.clone();
        let hook_output = installation.staging_path("usr/lib").join(output);
        candidate_metadata::arm_before_publication(output, move || {
            fs::hard_link(&hook_external, &hook_output).unwrap();
        });

        let error = apply_fresh_candidate(&fixture, |_| Ok(())).unwrap_err();
        let quarantine = assert_preserved_metadata_failure(&fixture, error, live_identity, &[]);

        assert_eq!(inode_identity(&external), external_identity);
        assert_eq!(fs::read(&external).unwrap(), format!("racing-{output}").as_bytes());
        assert_eq!(
            inode_identity(&quarantine.join("usr/lib").join(output)),
            external_identity
        );
    }
}

#[test]
fn retained_metadata_proof_rejects_post_trigger_mutation() {
    for mutation in ["rewrite", "delete", "replace", "hardlink"] {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_identity = inode_identity(&installation.root.join("usr"));
        let output = installation.staging_path("usr/lib/system-model.glu");
        let external = installation.root.join(format!("external-proof-{mutation}"));
        let mut mutated = false;

        let error = apply_fresh_candidate(&fixture, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
                mutated = true;
                match mutation {
                    "rewrite" => fs::write(&output, b"rewritten-after-trigger").unwrap(),
                    "delete" => fs::remove_file(&output).unwrap(),
                    "replace" => {
                        fs::write(&external, b"replacement-after-trigger").unwrap();
                        fs::remove_file(&output).unwrap();
                        fs::hard_link(&external, &output).unwrap();
                    }
                    "hardlink" => fs::hard_link(&output, &external).unwrap(),
                    _ => unreachable!(),
                }
            }
            Ok(())
        })
        .unwrap_err();

        assert!(mutated, "transaction boundary did not run for {mutation}");
        let quarantine = assert_preserved_metadata_failure(&fixture, error, live_identity, &["transaction"]);
        match mutation {
            "rewrite" => assert_eq!(
                fs::read(quarantine.join("usr/lib/system-model.glu")).unwrap(),
                b"rewritten-after-trigger"
            ),
            "delete" => assert!(!quarantine.join("usr/lib/system-model.glu").exists()),
            "replace" => {
                assert_eq!(fs::read(&external).unwrap(), b"replacement-after-trigger");
                assert_eq!(
                    inode_identity(&quarantine.join("usr/lib/system-model.glu")),
                    inode_identity(&external)
                );
            }
            "hardlink" => {
                assert_eq!(
                    inode_identity(&quarantine.join("usr/lib/system-model.glu")),
                    inode_identity(&external)
                );
                assert_eq!(fs::metadata(&external).unwrap().nlink(), 2);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn retained_metadata_proof_rejects_post_system_trigger_mutation() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_identity = inode_identity(&installation.root.join("usr"));
    let live_release = installation.root.join("usr/lib/os-release");
    let mut mutated = false;

    let error = apply_fresh_candidate(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggers {
            mutated = true;
            fs::write(&live_release, b"rewritten-after-system-trigger").unwrap();
        }
        Ok(())
    })
    .unwrap_err();

    assert!(mutated);
    match &error {
        Error::StatefulTransitionUsrRestored { primary, .. } => assert!(
            matches!(primary.as_ref(), Error::StatefulCandidateMetadata { .. }),
            "unexpected restored-transition primary: {primary:#?}"
        ),
        error => panic!("post-system-trigger mutation was not compensated: {error:#?}"),
    }
    assert_eq!(take_observed_trigger_scopes(), ["transaction", "system"]);
    assert_live_unchanged(&fixture, live_identity);
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    let quarantine = fs::read_dir(installation.state_quarantine_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read(quarantine.join("usr/lib/os-release")).unwrap(),
        b"rewritten-after-system-trigger"
    );
}

#[test]
fn candidate_usr_substitution_before_metadata_never_decorates_replacement() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_identity = inode_identity(&installation.root.join("usr"));
    let candidate = installation.staging_path("usr");
    let displaced = installation.root.join("displaced-metadata-candidate");
    let hook_candidate = candidate.clone();
    let hook_displaced = displaced.clone();
    arm_before_stateful_candidate_metadata(move || {
        fs::rename(&hook_candidate, &hook_displaced).unwrap();
        fs::create_dir(&hook_candidate).unwrap();
        fs::write(hook_candidate.join("foreign"), b"replacement-candidate").unwrap();
    });

    let error = apply_fresh_candidate(&fixture, |_| Ok(())).unwrap_err();

    assert!(
        matches!(&error, Error::StatefulTransitionRecoveryFailed { .. }),
        "{error:#?}"
    );
    assert!(take_observed_trigger_scopes().is_empty());
    assert_live_unchanged(&fixture, live_identity);
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(displaced.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert!(!displaced.join("lib/os-release").exists());
    assert!(!displaced.join("lib/system-model.glu").exists());
    assert_eq!(fs::read(candidate.join("foreign")).unwrap(), b"replacement-candidate");
    assert!(!candidate.join("lib").exists());
    assert!(
        fs::read_dir(installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn successful_stateful_metadata_is_sealed_and_rollback_capable() {
    let mut fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;

    apply_fresh_candidate(&fixture, |_| Ok(())).unwrap();

    assert_eq!(take_observed_trigger_scopes(), ["transaction", "system"]);
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&installation.root),
        &fixture.candidate_snapshot,
        "candidate-package",
    );
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/lib/os-release")).unwrap(),
        candidate_metadata::GENERIC_OS_RELEASE
    );
    for output in ["os-release", "system-model.glu"] {
        let metadata = fs::symlink_metadata(installation.root.join("usr/lib").join(output)).unwrap();
        assert!(metadata.file_type().is_file(), "metadata {output}");
        assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() }, "metadata {output}");
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o644, "metadata {output}");
        assert_eq!(metadata.nlink(), 1, "metadata {output}");
    }

    let previous = installation.root_path(fixture.previous.id.to_string());
    assert_eq!(
        fs::read_to_string(previous.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&previous),
        &fixture.previous_snapshot,
        "previous-package",
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );

    // The low-level adapter commits the filesystem transition directly. Keep
    // its cached installation view in sync before exercising public rollback.
    fixture.client.installation.active_state = Some(fixture.candidate.id);
    let installation = &fixture.client.installation;
    let displaced = fixture.client.activate_state(fixture.previous.id, true, true).unwrap();
    assert_eq!(displaced, fixture.candidate.id);
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(
            installation
                .root_path(fixture.candidate.id.to_string())
                .join("usr/lib/os-release")
        )
        .unwrap(),
        candidate_metadata::GENERIC_OS_RELEASE
    );
}

fn apply_fresh_candidate<F>(fixture: &StatefulTransitionFixture, checkpoint: F) -> Result<(), Error>
where
    F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
{
    fixture.client.apply_stateful_blit_with_checkpoint(
        vfs(Vec::new()).unwrap(),
        &fixture.candidate,
        Some(fixture.previous.id),
        generated_system_snapshot("candidate-package"),
        checkpoint,
    )
}

fn assert_preserved_metadata_failure(
    fixture: &StatefulTransitionFixture,
    error: Error,
    live_identity: (u64, u64),
    trigger_scopes: &[&'static str],
) -> PathBuf {
    match error {
        Error::StatefulCandidatePreserved { primary, .. } => assert!(
            matches!(primary.as_ref(), Error::StatefulCandidateMetadata { .. }),
            "unexpected preserved-candidate primary: {primary:#?}"
        ),
        error => panic!("candidate metadata failure was not preserved: {error:#?}"),
    }
    assert_eq!(take_observed_trigger_scopes(), trigger_scopes);
    assert_live_unchanged(fixture, live_identity);
    assert!(fixture.client.state_db.get(fixture.candidate.id).is_err());
    assert!(!fixture.client.installation.staging_path("usr").exists());

    let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1);
    let quarantine = quarantines.into_iter().next().unwrap();
    assert_eq!(
        fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    quarantine
}

fn assert_prejournal_hardlink_failure(
    fixture: &StatefulTransitionFixture,
    error: Error,
    live_identity: (u64, u64),
    expected_path: &Path,
) {
    let Error::StatefulTreeIdentityPreparationFailed {
        candidate,
        previous: Some(previous),
        location,
        source,
    } = error
    else {
        panic!("expected pre-journal candidate identity failure");
    };
    assert_eq!(candidate, fixture.candidate.id);
    assert_eq!(previous, fixture.previous.id);
    assert_eq!(location, fixture.client.installation.staging_path("usr"));
    let Error::StatefulTreeIdentity { source } = *source else {
        panic!("expected retained tree-identity source");
    };
    assert!(matches!(
        source.downcast_ref::<crate::transition_identity::Error>(),
        Some(crate::transition_identity::Error::CandidateInventory(
            crate::transition_identity::CandidateInventoryError::UnexpectedHardlink { path, links }
        )) if path == expected_path && *links == 2
    ));

    assert!(take_observed_trigger_scopes().is_empty());
    assert_live_unchanged(fixture, live_identity);
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert!(fixture.client.installation.staging_path("usr").exists());
    assert!(!fixture.client.installation.staging_path("usr/.cast-tree-id").exists());
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

fn assert_live_unchanged(fixture: &StatefulTransitionFixture, expected_identity: (u64, u64)) {
    let live = fixture.client.installation.root.join("usr");
    assert_eq!(inode_identity(&live), expected_identity);
    assert_eq!(
        fs::read_to_string(live.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&fixture.client.installation.root),
        &fixture.previous_snapshot,
        "previous-package",
    );
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn create_canonical_candidate_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

fn write_canonical_candidate_file(path: &Path, contents: impl AsRef<[u8]>) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o644)).unwrap();
}
