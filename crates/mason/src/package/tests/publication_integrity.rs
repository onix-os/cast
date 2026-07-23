#[test]
fn verified_manifest_publishes_and_reuses_exact_bytes() {
    let (root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");

    assert_eq!(
        publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
        Publication::Published
    );
    assert_eq!(
        publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
        Publication::Reused
    );
    let published_manifest = paths
        .output_dir()
        .join(plan.derivation_id().as_str())
        .join(binary_manifest_name(&names));
    assert_eq!(
        publish_artefacts_verifying(&paths, &plan, &published_manifest).unwrap(),
        Publication::Reused,
        "a published manifest is a useful independent reference for staged bytes"
    );
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn manifest_mismatch_rolls_back_new_output_and_preserves_reused_output() {
    let (root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"different artefact bytes");

    let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
    assert!(matches!(error, PublishError::ManifestVerificationMismatch { .. }));
    assert!(output_entries(&paths).is_empty());

    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    let manifest = bundle.join(binary_manifest_name(&names));
    let corrupted = vec![b'X'; b"frozen artefact bytes".len()];
    fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&manifest, &corrupted).unwrap();
    fs::set_permissions(&manifest, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
    filetime::set_file_mtime(&manifest, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
    fs::write(&expected, b"frozen artefact bytes").unwrap();
    let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
    assert!(matches!(error, PublishError::ManifestVerificationMismatch { .. }));
    assert_eq!(fs::read(manifest).unwrap(), corrupted);
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn manifest_verification_limit_accepts_n_rejects_n_plus_one_and_expires() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let bytes = b"frozen artefact bytes";
    let expected = reference_manifest(root.path(), bytes);
    let limits = PublishLimits::with_manifest_verification(bytes.len() as u64, Duration::from_secs(30));
    assert_eq!(
        publish_artefacts_verifying_with(&paths, &plan, &expected, limits, |_| Ok(())).unwrap(),
        Publication::Published
    );

    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let oversized = reference_manifest(root.path(), b"frozen artefact bytes+");
    let limit = b"frozen artefact bytes".len() as u64;
    let error = publish_artefacts_verifying_with(
        &paths,
        &plan,
        &oversized,
        PublishLimits::with_manifest_verification(limit, Duration::from_secs(30)),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(error, PublishError::ArtifactTooLarge { maximum, found, .. } if maximum == limit && found == limit + 1)
    );
    assert!(output_entries(&paths).is_empty());

    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), bytes);
    let error = publish_artefacts_verifying_with(
        &paths,
        &plan,
        &expected,
        PublishLimits::with_manifest_verification(bytes.len() as u64, Duration::ZERO),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(error, PublishError::Deadline { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn manifest_reference_rejects_symlink_directory_fifo_and_socket_without_blocking() {
    fn assert_rejected<F>(label: &str, make: F)
    where
        F: FnOnce(&Path),
    {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_path(root.path(), label);
        make(&expected);
        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(
            error,
            PublishError::UnexpectedEntry {
                role: "expected manifest",
                ..
            }
        ));
        assert!(output_entries(&paths).is_empty());
    }

    assert_rejected("reference-symlink", |expected| symlink("missing", expected).unwrap());
    assert_rejected("reference-directory", |expected| fs::create_dir(expected).unwrap());
    assert_rejected("reference-fifo", |expected| {
        nix::unistd::mkfifo(expected, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
    });
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_path(root.path(), "reference-socket");
    match std::os::unix::net::UnixListener::bind(&expected) {
        Ok(listener) => {
            drop(listener);
            let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
            assert!(matches!(
                error,
                PublishError::UnexpectedEntry {
                    role: "expected manifest",
                    ..
                }
            ));
            assert!(output_entries(&paths).is_empty());
        }
        // Some CI sandboxes prohibit AF_UNIX creation. The production
        // rejection is still exercised whenever the kernel permits the
        // fixture; FIFO covers the nonblocking special-file path always.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => panic!("create manifest reference socket: {error}"),
    }
}

#[test]
fn manifest_reference_cannot_alias_the_staged_manifest() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let expected = paths.artefacts().host.join(binary_manifest_name(&names));

    let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
    assert!(matches!(error, PublishError::ReferenceAliasesStagedManifest { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn manifest_reference_accepts_protected_hardlinks_and_trusted_owners() {
    assert!(publish::reference_owner_is_trusted(1000, 1000));
    assert!(publish::reference_owner_is_trusted(0, 1000));
    assert!(!publish::reference_owner_is_trusted(1001, 1000));

    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_path(root.path(), "hardlinked-reference");
    let original = expected.parent().unwrap().join("original");
    fs::write(&original, b"frozen artefact bytes").unwrap();
    fs::set_permissions(&original, std::fs::Permissions::from_mode(0o644)).unwrap();
    fs::hard_link(&original, &expected).unwrap();
    assert_eq!(fs::metadata(&expected).unwrap().nlink(), 2);

    assert_eq!(
        publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
        Publication::Published
    );
}

#[test]
fn manifest_reference_rejects_group_or_other_writable_parent_and_file() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");
    fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o666)).unwrap();
    let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
    assert!(matches!(
        error,
        PublishError::WritableReferenceManifest { found: 0o666, .. }
    ));
    assert!(output_entries(&paths).is_empty());

    fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(expected.parent().unwrap(), std::fs::Permissions::from_mode(0o770)).unwrap();
    let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
    assert!(matches!(
        error,
        PublishError::WritableRoot {
            role: "expected manifest parent",
            found: 0o770,
            ..
        }
    ));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn same_inode_reference_mutation_before_rename_rolls_back() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");
    let mut mutated = false;

    let error = publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            mutated = true;
            fs::write(&expected, b"changed artefact bytes").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(mutated);
    assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn replaced_reference_path_is_rejected_before_publication() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");
    let mut replaced = false;

    let error = publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::SourcesPinned {
            replaced = true;
            fs::remove_file(&expected).unwrap();
            fs::write(&expected, b"frozen artefact bytes").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(replaced);
    assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn reference_change_after_rename_removes_the_new_final_bundle() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");
    let mut mutated = false;

    let error = publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::AfterRename {
            mutated = true;
            fs::write(&expected, b"changed artefact bytes").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(mutated);
    assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn staged_manifest_mutation_before_rename_rolls_back_verified_publication() {
    let (root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let staged = paths.artefacts().host.join(binary_manifest_name(&names));
    let expected = reference_manifest(root.path(), b"frozen artefact bytes");

    let error = publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&staged, b"changed artefact bytes").unwrap();
            fs::set_permissions(&staged, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error, PublishError::ArtifactChanged { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn publication_limits_accept_exact_n_and_reject_n_plus_one() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let file_bytes = fs::metadata(paths.artefacts().host.join(&names[0])).unwrap().len();
    let aggregate = file_bytes * names.len() as u64;
    let limits = PublishLimits::with_file_and_bundle_bytes(file_bytes, aggregate);
    assert_eq!(
        publish_artefacts_with(&paths, &plan, limits, |_| Ok(())).unwrap(),
        Publication::Published
    );

    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let error = publish_artefacts_with(
        &paths,
        &plan,
        PublishLimits::with_file_and_bundle_bytes(file_bytes - 1, aggregate),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(error, PublishError::ArtifactTooLarge { maximum, found, .. } if maximum + 1 == found));

    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let error = publish_artefacts_with(
        &paths,
        &plan,
        PublishLimits::with_file_and_bundle_bytes(file_bytes, aggregate - 1),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(error, PublishError::BundleTooLarge { maximum, found } if maximum + 1 == found));

    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    assert_eq!(
        publish_artefacts_with(&paths, &plan, PublishLimits::with_max_artefacts(3), |_| Ok(())).unwrap(),
        Publication::Published
    );
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let error = publish_artefacts_with(&paths, &plan, PublishLimits::with_max_artefacts(2), |_| Ok(())).unwrap_err();
    assert!(matches!(
        error,
        PublishError::ResourceLimit {
            resource: "published artefact count",
            limit: 2
        }
    ));
}

#[test]
fn same_inode_staged_mutation_before_rename_rolls_back_every_owned_output() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let source = paths.artefacts().host.join(&names[0]);
    let length = fs::metadata(&source).unwrap().len() as usize;
    let mut mutated = false;
    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            mutated = true;
            fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&source, vec![b'X'; length]).unwrap();
            fs::set_permissions(&source, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(mutated);
    assert!(matches!(error, PublishError::ArtifactChanged { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn replaced_output_root_is_rejected_before_any_publication_write() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let output = paths.output_dir().clone();
    let displaced = root.path().join("displaced-output");
    let mut replaced = false;
    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::SourcesPinned {
            replaced = true;
            fs::rename(&output, &displaced).unwrap();
            fs::create_dir(&output).unwrap();
            fs::write(output.join("sentinel"), b"replacement").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(replaced);
    assert!(matches!(error, PublishError::OwnershipChanged { .. }));
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"replacement");
    assert!(fs::read_dir(displaced).unwrap().next().is_none());
}

#[test]
fn exact_concurrent_bundle_is_reused_and_private_stage_is_removed() {
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let mut collided = false;
    let publication = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            collided = true;
            create_competing_bundle(&plan, &paths, false);
        }
        Ok(())
    })
    .unwrap();
    assert!(collided);
    assert_eq!(publication, Publication::Reused);
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn collision_reuse_cannot_forget_the_byte_set_prepared_by_this_build() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let staged = paths.artefacts().host.join(&names[0]);
    let replacement = vec![b'B'; fs::metadata(&staged).unwrap().len() as usize];
    let mut collided = false;

    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            collided = true;
            fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&staged, &replacement).unwrap();
            fs::set_permissions(&staged, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
            create_competing_bundle(&plan, &paths, false);
        }
        Ok(())
    })
    .unwrap_err();

    assert!(collided);
    assert!(matches!(error, PublishError::ArtifactChanged { .. }));
    let final_bundle = paths.output_dir().join(plan.derivation_id().as_str());
    assert_eq!(fs::read(final_bundle.join(&names[0])).unwrap(), replacement);
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn mismatched_concurrent_bundle_is_preserved_and_private_stage_is_removed() {
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let mut collided = false;
    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeRename {
            collided = true;
            create_competing_bundle(&plan, &paths, true);
        }
        Ok(())
    })
    .unwrap_err();
    assert!(collided);
    assert!(matches!(error, PublishError::ContentMismatch { .. }));
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    assert!(fs::read_dir(bundle).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".mason-publish-")
    }));
}

#[test]
fn reuse_rejects_same_size_mutation_between_digest_rounds() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let published = paths.output_dir().join(plan.derivation_id().as_str()).join(&names[0]);
    let length = fs::metadata(&published).unwrap().len() as usize;
    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::BeforeReuseConfirmation {
            fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&published, vec![b'Y'; length]).unwrap();
            fs::set_permissions(&published, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error, PublishError::ArtifactChanged { .. }));
}

#[test]
fn reuse_rechecks_bytes_after_syncing_every_durability_boundary() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let published = paths.output_dir().join(plan.derivation_id().as_str()).join(&names[0]);
    let length = fs::metadata(&published).unwrap().len() as usize;
    let mut reached_post_sync_confirmation = false;

    let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
        if checkpoint == PublishCheckpoint::AfterReuseDurabilitySync {
            reached_post_sync_confirmation = true;
            fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&published, vec![b'Z'; length]).unwrap();
            fs::set_permissions(&published, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        Ok(())
    })
    .unwrap_err();

    assert!(reached_post_sync_confirmation);
    assert!(matches!(error, PublishError::ArtifactChanged { .. }));
}

#[test]
fn reuse_rejects_wrong_bundle_timestamp() {
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    filetime::set_file_mtime(
        &bundle,
        filetime::FileTime::from_unix_time(plan.source_date_epoch + 1, 0),
    )
    .unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(error, PublishError::TimestampMismatch { expected, seconds, .. } if expected + 1 == seconds));
}

#[test]
fn rename_noreplace_does_not_replace_even_an_empty_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(source.join("complete"), b"bundle").unwrap();

    let error = test_rename_noreplace(&source, &target).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(source.join("complete").is_file());
    assert!(target.is_dir());
    assert!(fs::read_dir(target).unwrap().next().is_none());
}
