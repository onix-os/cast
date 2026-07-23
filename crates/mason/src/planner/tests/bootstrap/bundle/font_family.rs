fn assert_font_family_fixture(planned: &super::super::Planned, packages: &BTreeMap<String, PackageImage>) {
    const FIXTURE: &str = "font-family";
    const TREE: &str = "cast-font-family-fixture-1.0.0";
    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain exactly one package");
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-font-family-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert!(root_plan.runtime_inputs.is_empty());
    assert_eq!(root_plan.summary.as_deref(), Some("Deterministic install-only TrueType fixture"));
    assert_eq!(
        root_plan.description.as_deref(),
        Some(
            "A self-authored Regular and Bold font family with semantic metadata validation and no generated font cache."
        )
    );

    let assets = [
        (
            "share/fonts/truetype/cast-aster-fixture/CastAsterFixture-Regular.ttf",
            "fonts/CastAsterFixture-Regular.ttf",
        ),
        (
            "share/fonts/truetype/cast-aster-fixture/CastAsterFixture-Bold.ttf",
            "fonts/CastAsterFixture-Bold.ttf",
        ),
        ("share/licenses/cast-font-family-fixture/OFL.txt", "OFL.txt"),
    ];
    assert_leaf_paths(FIXTURE, "out", root, assets.iter().map(|(target, _)| *target));
    assert_no_directories(FIXTURE, "out", root);
    for (target, source) in assets {
        assert_regular(FIXTURE, root, target, 0o644, tracked_bytes(TREE, source));
    }

    for forbidden in [
        "share/fonts/truetype/cast-aster-fixture/fonts.cache-1",
        "share/fonts/truetype/cast-aster-fixture/fonts.dir",
        "share/fonts/truetype/cast-aster-fixture/fonts.scale",
        "share/licenses/cast-font-family-fixture/PROVENANCE",
        "share/cast-font-family-fixture/generate_cast_aster_fixture.rs",
    ] {
        assert!(
            !root.layouts.contains_key(forbidden),
            "{FIXTURE}: build-only or generated data leaked into the immutable output: {forbidden}"
        );
    }
    assert_exact_relations(
        FIXTURE,
        root,
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
