#[test]
fn planner_and_frozen_runtime_share_one_authenticated_repository_snapshot_namespace() {
    let fixture = Fixture::new();
    let repositories = fixture.builder().repositories().clone();
    let planned = plan(fixture.env(), fixture.request()).unwrap();

    assert_runtime_reopens_planner_repository_snapshot(&fixture.forge_dir, &fixture.output_dir, repositories, &planned);
}

#[test]
fn checked_in_metadata_only_example_fails_closed_before_execution() {
    let matrix = PackageExampleMatrix::new();
    let example = matrix
        .examples
        .iter()
        .find(|example| example.name == "minimal")
        .expect("the explicitly inventoried example matrix contains minimal");

    let first = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir).unwrap();
    assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
    assert!(
        first.plan.sources.is_empty(),
        "minimal must remain a source-less execution fixture"
    );
    assert!(
        first
            .plan
            .jobs
            .iter()
            .flat_map(|job| &job.phases)
            .all(|phase| { phase.pre.is_empty() && phase.steps.is_empty() && phase.post.is_empty() }),
        "minimal must isolate frozen-root verification without invoking package build steps"
    );
    assert!(
        first
            .plan
            .build_lock
            .packages
            .iter()
            .all(|package| package.package_id.len() == 64
                && package
                    .package_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
        "the frozen runtime closure must use the real SHA-256 identities of local Stone artifacts"
    );
    let derivation_id = first.plan.derivation_id();

    assert_runtime_reopens_planner_repository_snapshot(
        &matrix.forge_dir,
        &matrix.output_dir,
        matrix.builder(example).repositories().clone(),
        &first,
    );

    let error = execute_and_publish(&first)
        .expect_err("metadata-only providers must never satisfy a frozen executable binding");
    let error = error_chain(error.as_ref());
    assert!(
        error.contains("frozen executable provider") && error.contains("has no regular layout entry"),
        "metadata-only closure must fail at the exact executable boundary, got: {error}"
    );
    let published_root = matrix.output_dir.join(derivation_id.as_str());
    assert!(
        !published_root.exists(),
        "an unauthenticated metadata-only closure must not publish a derivation"
    );
}
