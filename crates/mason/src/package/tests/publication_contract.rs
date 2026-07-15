#[test]
fn frozen_bundle_contract_names_every_declared_output_and_both_manifests() {
    let mut plan = test_derivation_plan();
    plan.outputs.push(OutputPlan {
        name: "dev".to_owned(),
        package_name: "example-devel".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    });
    plan.validate().unwrap();

    assert_eq!(
        expected_bundle_files(&plan).into_iter().collect::<Vec<_>>(),
        [
            OsString::from("example-1.2.3-1-1-x86_64.stone"),
            OsString::from("example-devel-1.2.3-1-1-x86_64.stone"),
            OsString::from("manifest.x86_64.bin"),
            OsString::from("manifest.x86_64.jsonc"),
        ]
    );
}

#[test]
fn publication_requires_the_execution_lock_for_its_exact_derivation_workspace() {
    let (_root, plan, paths) = publication_fixture();
    let (_other_root, other_plan, other_paths) = publication_fixture();
    let wrong_lock = other_paths.acquire_execution_lock(&other_plan).unwrap();
    let staged_anchor = paths.prepare_private_host_directory(&paths.artefacts().host).unwrap();

    let error =
        super::publish_artefacts(&paths, &plan, &wrong_lock, &staged_anchor, ManifestVerification::None).unwrap_err();

    assert!(matches!(error, PublishError::InvalidExecutionLock(_)));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn production_publication_rejects_staged_path_substitution_after_anchor_is_pinned() {
    let (root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let execution_lock = paths.acquire_execution_lock(&plan).unwrap();
    let staged_path = paths.artefacts().host;
    let staged_anchor = paths.prepare_private_host_directory(&staged_path).unwrap();
    let detached = root.path().join("detached-staged");
    fs::rename(&staged_path, &detached).unwrap();
    let replacement_anchor = paths.prepare_private_host_directory(&staged_path).unwrap();

    let error = super::publish_artefacts(
        &paths,
        &plan,
        &execution_lock,
        &staged_anchor,
        ManifestVerification::None,
    )
    .unwrap_err();

    assert!(matches!(error, PublishError::OwnershipChanged { path } if path == staged_path));
    assert!(output_entries(&paths).is_empty());
    assert_ne!(
        staged_anchor.metadata().unwrap().ino(),
        replacement_anchor.metadata().unwrap().ino()
    );
}

#[test]
fn publication_rejects_group_or_other_writable_roots() {
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    let execution_lock = paths.acquire_execution_lock(&plan).unwrap();
    let staged_anchor = paths.prepare_private_host_directory(&paths.artefacts().host).unwrap();
    fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o775)).unwrap();

    let error = super::publish_artefacts(
        &paths,
        &plan,
        &execution_lock,
        &staged_anchor,
        ManifestVerification::None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PublishError::WritableRoot {
            role: "output",
            found: 0o775,
            ..
        }
    ));
    assert!(output_entries(&paths).is_empty());

    fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&paths.artefacts().host, std::fs::Permissions::from_mode(0o777)).unwrap();
    let error = super::publish_artefacts(
        &paths,
        &plan,
        &execution_lock,
        &staged_anchor,
        ManifestVerification::None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PublishError::WritableRoot {
            role: "staged",
            found: 0o777,
            ..
        }
    ));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn publishes_and_reuses_one_complete_derivation_bundle() {
    let (_root, plan, paths) = publication_fixture();
    let staged = paths.artefacts().host;
    let names = stage_expected_bundle(&plan, &paths);
    assert_eq!(
        names,
        [
            OsString::from("example-1.2.3-1-1-x86_64.stone"),
            OsString::from("manifest.x86_64.bin"),
            OsString::from("manifest.x86_64.jsonc"),
        ]
    );
    let package = staged.join(stone_name(&names));

    assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Published);

    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    assert_eq!(
        fs::metadata(&bundle).unwrap().permissions().mode() & 0o7777,
        PUBLISHED_BUNDLE_MODE
    );
    for name in &names {
        assert_eq!(fs::read(bundle.join(name)).unwrap(), b"frozen artefact bytes");
        let metadata = fs::metadata(bundle.join(name)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, PUBLISHED_ARTEFACT_MODE);
        assert_eq!(metadata.mtime(), plan.source_date_epoch);
        assert_eq!(metadata.mtime_nsec(), 0);
    }
    assert_ne!(
        fs::metadata(&package).unwrap().ino(),
        fs::metadata(bundle.join(stone_name(&names))).unwrap().ino(),
        "published files must not retain mutable staging inodes"
    );
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    assert!(!paths.output_dir().join(stone_name(&names)).exists());

    assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Reused);
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn publication_normalizes_authenticated_creation_modes_under_adverse_umask() {
    const CHILD: &str = "MASON_PUBLICATION_UMASK_TEST_CHILD";
    const TEST: &str = "package::tests::publication_normalizes_authenticated_creation_modes_under_adverse_umask";

    // umask is process-global. Isolate the mutation in a child test process
    // so this regression cannot race unrelated tests in the harness.
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "adverse-umask child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    // Removes owner-write at creation: the directory starts as 0500 and
    // files as 0400. Publication must authenticate each descriptor before
    // restoring its private construction mode and sealing it read-only.
    // SAFETY: this is the sole test selected in the isolated child process.
    let previous = unsafe { nix::libc::umask(0o277) };
    let result = publish_artefacts(&paths, &plan);
    // SAFETY: restore the child process mask before assertions can panic.
    unsafe { nix::libc::umask(previous) };

    assert_eq!(result.unwrap(), Publication::Published);
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    assert_eq!(
        fs::metadata(&bundle).unwrap().permissions().mode() & 0o7777,
        PUBLISHED_BUNDLE_MODE
    );
    assert!(names.iter().all(|name| {
        fs::metadata(bundle.join(name)).unwrap().permissions().mode() & 0o7777 == PUBLISHED_ARTEFACT_MODE
    }));
}

#[test]
fn mismatched_existing_bundle_is_never_modified() {
    let (_root, plan, paths) = publication_fixture();
    let staged = paths.artefacts().host;
    let names = stage_expected_bundle(&plan, &paths);
    let package_name = stone_name(&names);
    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());

    let staged_package = staged.join(package_name);
    fs::set_permissions(&staged_package, std::fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&staged_package, b"different").unwrap();
    fs::set_permissions(
        &staged_package,
        std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
    )
    .unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(error, PublishError::ContentMismatch { .. }));
    assert_eq!(fs::read(bundle.join(package_name)).unwrap(), b"frozen artefact bytes");

    fs::set_permissions(&staged_package, std::fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&staged_package, b"frozen artefact bytes").unwrap();
    fs::set_permissions(
        &staged_package,
        std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
    )
    .unwrap();
    fs::write(staged.join("extra.stone"), b"extra").unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::FrozenFileSetMismatch { role: "staged", .. }
    ));
    assert!(!bundle.join("extra.stone").exists());
    assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
}

#[test]
fn missing_or_extra_staged_files_are_rejected_before_publication() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let staged = paths.artefacts().host;
    let missing = names[0].clone();
    fs::remove_file(staged.join(&missing)).unwrap();

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        &error,
        PublishError::FrozenFileSetMismatch {
            role: "staged",
            expected,
            found,
            ..
        } if expected.contains(&missing) && !found.contains(&missing)
    ));
    assert!(output_entries(&paths).is_empty());

    fs::write(staged.join(&missing), b"frozen artefact bytes").unwrap();
    fs::set_permissions(
        staged.join(&missing),
        std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
    )
    .unwrap();
    let extra = OsString::from("undeclared-debug-output.stone");
    fs::write(staged.join(&extra), b"undeclared").unwrap();

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        &error,
        PublishError::FrozenFileSetMismatch {
            role: "staged",
            expected,
            found,
            ..
        } if !expected.contains(&extra) && found.contains(&extra)
    ));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn unsealed_staged_modes_are_rejected_before_publication() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let staged_path = paths.artefacts().host.join(&names[0]);
    fs::set_permissions(&staged_path, std::fs::Permissions::from_mode(0o6755)).unwrap();

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::ModeMismatch {
            role: "staged",
            expected: PUBLISHED_ARTEFACT_MODE,
            found: 0o6755,
            ..
        }
    ));
    assert_eq!(fs::metadata(staged_path).unwrap().permissions().mode() & 0o7777, 0o6755);
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn existing_bundle_mode_mismatch_is_never_reused() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    let published = bundle.join(&names[0]);
    fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::ModeMismatch {
            role: "published",
            expected: PUBLISHED_ARTEFACT_MODE,
            found: 0o600,
            ..
        }
    ));
    assert_eq!(fs::metadata(published).unwrap().permissions().mode() & 0o7777, 0o600);
}

#[test]
fn existing_bundle_directory_mode_mismatch_is_never_reused() {
    let (_root, plan, paths) = publication_fixture();
    stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o700)).unwrap();

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::ModeMismatch {
            role: "published bundle",
            expected: PUBLISHED_BUNDLE_MODE,
            found: 0o700,
            ..
        }
    ));
    assert_eq!(fs::metadata(bundle).unwrap().permissions().mode() & 0o7777, 0o700);
}

#[test]
fn existing_bundle_file_set_must_still_match_the_frozen_plan() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    publish_artefacts(&paths, &plan).unwrap();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    let missing = names[0].clone();
    fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(bundle.join(&missing)).unwrap();
    seal_test_bundle_directory(&bundle, &plan);

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        &error,
        PublishError::FrozenFileSetMismatch {
            role: "published",
            expected,
            found,
            ..
        } if expected.contains(&missing) && !found.contains(&missing)
    ));

    fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bundle.join(&missing), b"frozen artefact bytes").unwrap();
    fs::set_permissions(
        bundle.join(&missing),
        std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
    )
    .unwrap();
    let extra = OsString::from("undeclared-published-file");
    fs::write(bundle.join(&extra), b"extra").unwrap();
    seal_test_bundle_directory(&bundle, &plan);

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        &error,
        PublishError::FrozenFileSetMismatch {
            role: "published",
            expected,
            found,
            ..
        } if !expected.contains(&extra) && found.contains(&extra)
    ));
}

#[test]
fn rejects_non_regular_or_nested_staged_entries_without_a_final_bundle() {
    let (_root, plan, paths) = publication_fixture();
    let staged = paths.artefacts().host;
    fs::create_dir(staged.join("nested")).unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::FrozenFileSetMismatch { role: "staged", .. }
    ));
    assert!(output_entries(&paths).is_empty());

    fs::remove_dir(staged.join("nested")).unwrap();
    let names = stage_expected_bundle(&plan, &paths);
    let replaced = staged.join(&names[0]);
    fs::remove_file(&replaced).unwrap();
    symlink("missing", &replaced).unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
    assert!(output_entries(&paths).is_empty());

    fs::remove_file(&replaced).unwrap();
    nix::unistd::mkfifo(&replaced, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
    assert!(output_entries(&paths).is_empty());
}

#[test]
fn rejects_unexpected_entries_in_an_existing_final_bundle() {
    let (_root, plan, paths) = publication_fixture();
    let names = stage_expected_bundle(&plan, &paths);
    let package_name = stone_name(&names);
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    fs::create_dir(&bundle).unwrap();
    symlink("missing", bundle.join(package_name)).unwrap();
    seal_test_bundle_directory(&bundle, &plan);

    let error = publish_artefacts(&paths, &plan).unwrap_err();
    assert!(matches!(
        error,
        PublishError::FrozenFileSetMismatch { role: "published", .. }
    ));
    assert!(
        fs::symlink_metadata(bundle.join(package_name))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
