fn assert_external_test_vectors_fixture(
    planned: &super::super::Planned,
    packages: &BTreeMap<String, PackageImage>,
) {
    const FIXTURE: &str = "external-test-vectors";
    const EXECUTABLE: &str = "bin/cast-external-test-vectors-fixture";
    const VECTOR_MARKER: &[u8] = b"cast external test vectors fixture: 3 independently locked vectors verified";
    const SELF_TEST_MARKER: &[u8] = b"cast external test vectors fixture: codec self-test passed";

    assert_eq!(packages.len(), 2, "{FIXTURE}: emitted bundle must contain exactly two packages");
    let (root_plan, root) = output(planned, packages, "out");
    let (debug_plan, debug) = output(planned, packages, "dbginfo");
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, "cast-external-test-vectors-fixture");
    assert!(root_plan.include_in_manifest);
    assert!(root_plan.runtime_inputs.is_empty(), "{FIXTURE}: build/check tools leaked into runtime relations");
    assert_eq!(
        root_plan.summary.as_deref(),
        Some("Frame codec checked against an independently locked corpus")
    );
    assert_eq!(
        root_plan.description.as_deref(),
        Some("A real CMake/CTest build whose check-only corpus never enters the installed output.")
    );
    assert_eq!(debug_plan.name, "dbginfo");
    assert_eq!(debug_plan.package_name, "cast-external-test-vectors-fixture-dbginfo");
    assert!(!debug_plan.include_in_manifest);
    assert!(debug_plan.runtime_inputs.is_empty());
    assert_eq!(debug_plan.summary.as_deref(), Some("Frame codec debugging symbols"));

    assert_leaf_paths(FIXTURE, "out", root, [EXECUTABLE]);
    assert_no_directories(FIXTURE, "out", root);
    assert_eq!(root.layouts[EXECUTABLE].mode & 0o777, 0o755);
    let executable = regular_bytes(FIXTURE, root, EXECUTABLE);
    for marker in [VECTOR_MARKER, SELF_TEST_MARKER] {
        assert!(
            contains_bytes(executable, marker),
            "{FIXTURE}: installed executable lost its exact test identity"
        );
    }
    for forbidden in [
        "bin/external-test-vectors.json",
        "share/external-test-vectors.json",
        "share/cast/external-test-vectors.json",
    ] {
        assert!(
            !root.layouts.contains_key(forbidden),
            "{FIXTURE}: check-only corpus leaked into immutable output at /usr/{forbidden}"
        );
    }

    let executable_elf = assert_runtime_elf(FIXTURE, EXECUTABLE, executable, RuntimeElfKind::Executable);
    let mut dependencies = planned_output_dependencies(planned, root_plan);
    dependencies.extend(executable_elf.dependencies.iter().cloned());
    assert_exact_relations(
        FIXTURE,
        root,
        dependencies,
        BTreeSet::from([
            root_plan.package_name.clone(),
            "binary(cast-external-test-vectors-fixture)".to_owned(),
        ]),
    );
    assert_debug_output(FIXTURE, debug, &[executable_elf]);
    assert_exact_relations(
        FIXTURE,
        debug,
        planned_output_dependencies(planned, debug_plan),
        BTreeSet::from([debug_plan.package_name.clone()]),
    );
}
