fn assert_pgo_workload_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "pgo-workload";
    const EXECUTABLE: &str = "bin/cast-pgo-workload-fixture";
    const FINAL_IDENTITY: &[u8] = b"cast PGO workload fixture: profile-use binary executed";
    const TRAINING_IDENTITY: &[u8] = b"cast PGO workload fixture: instrumented training completed";

    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain exactly one package");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must emit exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, "cast-pgo-workload-fixture");
    assert!(root_plan.include_in_manifest);
    assert!(root_plan.runtime_inputs.is_empty());
    assert_eq!(
        root_plan.summary.as_deref(),
        Some("Offline LLVM profile-guided optimization fixture")
    );
    assert_eq!(
        root_plan.description.as_deref(),
        Some("A two-job custom build trained structurally before its isolated profile-use rebuild.")
    );

    let root = &packages[&root_plan.package_name];
    assert_leaf_paths(FIXTURE, "out", root, [EXECUTABLE]);
    assert_no_directories(FIXTURE, "out", root);
    assert_eq!(root.layouts[EXECUTABLE].mode & 0o777, 0o755);
    let executable = regular_bytes(FIXTURE, root, EXECUTABLE);
    for identity in [FINAL_IDENTITY, TRAINING_IDENTITY] {
        assert!(
            contains_bytes(executable, identity),
            "{FIXTURE}: installed profile-use executable lost a behavior identity"
        );
    }
    for instrumentation in [
        b"__llvm_profile".as_slice(),
        b"__llvm_prf_cnts".as_slice(),
        b"__llvm_prf_data".as_slice(),
        b".profraw".as_slice(),
    ] {
        assert!(
            !contains_bytes(executable, instrumentation),
            "{FIXTURE}: installed profile-use executable retained LLVM instrumentation state"
        );
    }
    for forbidden in [
        "stage1-only.marker",
        "ir.profdata",
        "combined.profdata",
        "default.profraw",
        "IR",
        "CS",
    ] {
        assert!(
            root.layouts.keys().all(|path| !path.contains(forbidden)),
            "{FIXTURE}: training or stage-one state leaked into /usr: {forbidden}"
        );
    }

    let native = assert_runtime_elf(
        FIXTURE,
        EXECUTABLE,
        executable,
        RuntimeElfKind::Executable,
        &planned.plan.analysis,
    );
    let mut dependencies = planned_output_dependencies(planned, root_plan);
    dependencies.extend(native.dependencies.iter().cloned());
    for build_only in ["binary(bash)", "binary(clang)", "binary(cp)", "binary(llvm-profdata)"] {
        assert!(
            root.meta.dependencies.iter().all(|dependency| dependency.to_name() != build_only),
            "{FIXTURE}: build-only PGO capability leaked into Stone runtime metadata: {build_only}"
        );
    }
    assert_exact_relations(
        FIXTURE,
        root,
        dependencies,
        BTreeSet::from([
            root_plan.package_name.clone(),
            "binary(cast-pgo-workload-fixture)".to_owned(),
        ]),
    );
}
