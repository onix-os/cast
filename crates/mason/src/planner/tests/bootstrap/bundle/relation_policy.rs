fn assert_relation_policy_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "relation-policy";
    const TARGET: &str = "share/cast/relation-policy.conf";

    assert!(planned.plan.sources.is_empty(), "{FIXTURE}: source list must be empty");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: source-less package must freeze exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, "cast-relation-policy-fixture");
    assert!(root_plan.include_in_manifest);
    assert_eq!(root_plan.summary.as_deref(), Some("Typed dependency-role policy fixture"));
    assert_eq!(
        root_plan.description.as_deref(),
        Some("Pinned build and runtime capabilities with exact provider identities.")
    );
    assert!(root_plan.conflicts.is_empty());
    assert_eq!(
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))".to_owned(),
            "soname(libz.so.1(x86_64))".to_owned(),
        ])
    );

    let root = &packages[&root_plan.package_name];
    assert_leaf_paths(FIXTURE, "out", root, [TARGET]);
    assert_no_directories(FIXTURE, "out", root);
    assert_regular(
        FIXTURE,
        root,
        TARGET,
        0o644,
        super::super::RELATION_POLICY_CONTENT.to_vec(),
    );
    assert_exact_relations(
        FIXTURE,
        root,
        BTreeSet::from([
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))".to_owned(),
            "soname(libz.so.1(x86_64))".to_owned(),
        ]),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
