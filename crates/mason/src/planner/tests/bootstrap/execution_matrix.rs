#[test]
#[ignore = "requires the package store prepared by make bootstrap-fixtures"]
fn contentful_bootstrap_materializes_a_complete_offline_root_mirror() {
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
}

#[test]
fn bootstrap_planning_matrix_normalizes_output_root_under_umask_0002() {
    const CHILD: &str = "MASON_BOOTSTRAP_MATRIX_UMASK_0002_CHILD";
    const TEST: &str =
        "planner::hermetic_tests::bootstrap::bootstrap_planning_matrix_normalizes_output_root_under_umask_0002";

    if std::env::var_os(CHILD).is_some() {
        // umask is process-global. This branch is the sole exact test in an
        // isolated child, so it cannot perturb parallel filesystem tests.
        // SAFETY: no other test runs in this exact child process.
        let previous = unsafe { nix::libc::umask(0o002) };
        let (closure, _) = validated_bootstrap();
        let matrix = BootstrapPlanningMatrix::new(&closure);
        let metadata = fs::symlink_metadata(&matrix.output_dir).unwrap();
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        let owner = unsafe { nix::libc::geteuid() };
        assert!(metadata.file_type().is_dir());
        assert_eq!(metadata.uid(), owner);
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        drop(matrix);
        // SAFETY: restore the isolated child's original mask before returning.
        unsafe { nix::libc::umask(previous) };
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(TEST)
        .arg("--nocapture")
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "umask-0002 bootstrap matrix child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bootstrap_temp_root_cleanup_removes_read_only_published_trees() {
    let temporary = BootstrapTempRoot::new(crate::private_tempdir());
    let retained_path = temporary.path().to_owned();
    let outside = crate::private_tempdir();
    let sentinel = outside.path().join("must-survive");
    fs::write(&sentinel, b"outside").unwrap();
    let published = retained_path.join("output/derivation");
    let nested = published.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("fixture.stone"), b"published").unwrap();
    symlink(outside.path(), nested.join("outside-link")).unwrap();
    fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&published, std::fs::Permissions::from_mode(0o555)).unwrap();

    temporary.close().unwrap();
    assert!(
        !retained_path.exists(),
        "bootstrap cleanup guard leaked a read-only published tree"
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"outside");
}

#[test]
fn bootstrap_temp_root_cleanup_failure_never_follows_a_replacement_symlink() {
    let temporary = BootstrapTempRoot::new(crate::private_tempdir());
    let retained_path = temporary.path().to_owned();
    let detached_path = retained_path.with_extension("detached");
    let outside = crate::private_tempdir();
    let sentinel = outside.path().join("must-survive");
    fs::write(&sentinel, b"outside").unwrap();
    fs::write(retained_path.join("owned"), b"retained").unwrap();

    fs::rename(&retained_path, &detached_path).unwrap();
    symlink(outside.path(), &retained_path).unwrap();
    let error = temporary
        .close()
        .expect_err("detached bootstrap root must fail closed instead of following its replacement");
    assert!(error.to_string().contains("no longer denotes"));
    assert!(fs::symlink_metadata(&retained_path).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
    assert_eq!(fs::read(detached_path.join("owned")).unwrap(), b"retained");

    fs::remove_file(&retained_path).unwrap();
    fs::rename(&detached_path, &retained_path).unwrap();
    crate::paths::BoundedTempTree::retain(&retained_path)
        .unwrap()
        .remove()
        .unwrap();
    assert!(!retained_path.exists());
}

#[cfg(feature = "delegated-fixture-test-support")]
pub(super) fn run_delegated_execution_fixture() -> DelegatedExecutionOutcome {
    run_execution_fixtures_from_contentful_closure()
}

// Keep the existing implementation type-checked by ordinary unit-test builds
// without registering it with libtest. Only the feature-gated harness-free
// entry point above is allowed to execute it.
#[cfg(test)]
const _: fn() -> DelegatedExecutionOutcome = run_execution_fixtures_from_contentful_closure;

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
fn run_execution_fixtures_from_contentful_closure() -> DelegatedExecutionOutcome {
    let selection = execution_fixture_selection_from_env()
        .unwrap_or_else(|error| panic!("invalid execution-fixture selector: {error}"));
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
    let mut evidence = execution_evidence::ExecutionEvidenceBuilder::new(selection);
    let mut executed = 0usize;

    for (name, recipe) in matrix.recipes.iter().filter(|(name, _)| selection.includes(name)) {
        let first = plan_for_build(matrix.env(), matrix.request(recipe, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: plan contentful execution: {error:#}"));
        assert_fixture_package_closure(name, &first.plan, &closure);
        assert_execution_fixture_topology(name, &first.plan);
        matrix.import_sources(&first);
        assert_eq!(
            first.lock_outcome,
            Some(WriteOutcome::Written),
            "{name}: first plan must publish a fresh build.lock.glu"
        );
        let canonical_plan = first.plan.canonical_bytes();
        let derivation_id = first.plan.derivation_id();
        let first_lock = fs::read(&first.lock_path)
            .unwrap_or_else(|error| panic!("{name}: read first build.lock.glu observation: {error}"));
        assert_eq!(
            first_lock,
            encode_build_lock(&first.plan.build_lock).into_bytes(),
            "{name}: first build.lock.glu presentation does not encode the frozen lock"
        );
        let input_snapshot = ExecutionInputSnapshot::capture(recipe, &first.lock_path);

        let first_publication = match execute_and_publish(&first) {
            Ok(publication) => publication,
            Err(error)
                if container_capability_unavailable(error.as_ref())
                    && !execution_capability_required()
                    && executed == 0 =>
            {
                eprintln!(
                    "skipping selected contentful execution fixture(s): this host cannot create the required user/mount namespaces: {}",
                    error_chain(error.as_ref())
                );
                return evidence.capability_unavailable();
            }
            Err(error) => panic!(
                "{name}: contentful execution failed after successful planning: {}",
                error_chain(error.as_ref())
            ),
        };
        assert_eq!(first_publication, Publication::Published, "{name}: first publication");
        executed += 1;

        let published_root = matrix.output_dir.join(derivation_id.as_str());
        let published = bundle::assert_fixture_bundle(name, &first, &published_root, bundle::BundleRootRole::Published);
        input_snapshot.assert_unchanged(
            name,
            "after the first execution and publication",
            recipe,
            &first.lock_path,
        );

        let locked = plan_for_build(matrix.env(), matrix.request(recipe, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: reuse contentful build lock: {error:#}"));
        let repeated_plan = locked.plan.canonical_bytes();
        let repeated_derivation_id = locked.plan.derivation_id();
        assert_eq!(
            locked.lock_outcome, None,
            "{name}: reuse must not rewrite build.lock.glu"
        );
        assert_eq!(
            repeated_plan,
            canonical_plan,
            "{name}: canonical plan drift"
        );
        assert_eq!(
            repeated_derivation_id,
            derivation_id,
            "{name}: derivation ID drift"
        );
        assert_eq!(
            locked.lock_path, first.lock_path,
            "{name}: repeated planning selected a different build.lock.glu path"
        );
        let repeated_lock = fs::read(&locked.lock_path)
            .unwrap_or_else(|error| panic!("{name}: read repeated build.lock.glu observation: {error}"));
        assert_eq!(repeated_lock, first_lock, "{name}: repeated build.lock.glu bytes drifted");
        input_snapshot.assert_unchanged(name, "after locked replanning", recipe, &locked.lock_path);

        let second_publication = execute_and_publish(&locked).unwrap_or_else(|error| {
            panic!(
                "{name}: repeated contentful execution failed: {}",
                error_chain(error.as_ref())
            )
        });
        assert_eq!(second_publication, Publication::Reused, "{name}: repeated publication");
        let repeated = bundle::assert_fixture_bundle(
            name,
            &locked,
            &locked.runtime.paths.artefacts().host,
            bundle::BundleRootRole::Staged,
        );
        assert_eq!(repeated, published, "{name}: repeated build changed emitted bytes");
        let preserved =
            bundle::assert_fixture_bundle(name, &locked, &published_root, bundle::BundleRootRole::Published);
        assert_eq!(preserved, published, "{name}: published generation changed");
        input_snapshot.assert_unchanged(
            name,
            "after the repeated execution and publication",
            recipe,
            &locked.lock_path,
        );
        evidence.push(execution_evidence::FixtureEvidenceInputs {
            name,
            first_plan: &canonical_plan,
            first_derivation_id: derivation_id.as_str(),
            repeat_plan: &repeated_plan,
            repeat_derivation_id: repeated_derivation_id.as_str(),
            first_build_lock: &first_lock,
            first_build_lock_outcome: first.lock_outcome,
            repeat_build_lock: &repeated_lock,
            repeat_build_lock_outcome: locked.lock_outcome,
            first_publication,
            repeat_publication: second_publication,
            published_after_first: &published,
            staged_after_repeat: &repeated,
            published_after_repeat: &preserved,
        });
    }

    assert_eq!(executed, selection.expected_count());
    evidence.finish()
}

#[test]
fn all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure() {
    let (closure, indexed) = validated_bootstrap();
    let expected_packages = closure.packages.sha256.iter().cloned().collect::<BTreeSet<_>>();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    let mut resolved_packages = BTreeSet::new();

    for (name, recipe) in &matrix.recipes {
        let first = plan_for_build(matrix.env(), matrix.request(recipe, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: plan with pinned contentful bootstrap: {error:#}"));
        first
            .plan
            .validate()
            .unwrap_or_else(|error| panic!("{name}: validate contentful plan: {error:#}"));
        assert_execution_fixture_topology(name, &first.plan);
        matrix.import_sources(&first);
        assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
        assert_eq!(first.plan.build_lock.repositories.len(), 1);
        let repository = &first.plan.build_lock.repositories[0];
        assert_eq!(repository.id, "bootstrap");
        assert_eq!(repository.index_uri, matrix.index_uri);
        assert_eq!(repository.snapshot, closure.repository.index.sha256);
        assert!(
            first
                .plan
                .build_lock
                .packages
                .iter()
                .all(|package| package.repository == "bootstrap" && !package.name.starts_with("planner-provider-")),
            "{name}: a synthetic metadata-only provider entered the real closure"
        );
        assert_fixture_package_closure(name, &first.plan, &closure);
        resolved_packages.extend(
            first
                .plan
                .build_lock
                .packages
                .iter()
                .map(|package| package.package_id.clone()),
        );

        let canonical_plan = first.plan.canonical_bytes();
        let derivation_id = first.plan.derivation_id();
        let lock_bytes = fs::read(&first.lock_path).unwrap();
        assert_eq!(lock_bytes, encode_build_lock(&first.plan.build_lock).into_bytes());
        let locked = plan_for_build(matrix.env(), matrix.request(recipe, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: reuse contentful build lock: {error:#}"));
        assert_eq!(locked.lock_outcome, None);
        assert_eq!(locked.plan.canonical_bytes(), canonical_plan);
        assert_eq!(locked.plan.derivation_id(), derivation_id);
        assert_eq!(fs::read(&locked.lock_path).unwrap(), lock_bytes);
    }

    if resolved_packages != expected_packages {
        let missing_from_manifest = resolved_packages
            .difference(&expected_packages)
            .cloned()
            .collect::<Vec<_>>();
        let unused_in_manifest = expected_packages
            .difference(&resolved_packages)
            .cloned()
            .collect::<Vec<_>>();
        let resolved_total_download_bytes = resolved_packages
            .iter()
            .map(|hash| {
                indexed[hash]
                    .download_size
                    .unwrap_or_else(|| panic!("resolved bootstrap package {hash} has no declared size"))
            })
            .try_fold(0u64, |total, size| total.checked_add(size))
            .expect("resolved bootstrap package byte sum overflowed");
        panic!(
            "the real execution plans differ from the declarative bootstrap closure; \
             missing_from_manifest={missing_from_manifest:?}, unused_in_manifest={unused_in_manifest:?}, \
             resolved_total_download_bytes={resolved_total_download_bytes}, \
             resolved_packages={resolved_packages:?}"
        );
    }
}
