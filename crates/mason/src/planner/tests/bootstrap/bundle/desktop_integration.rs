fn assert_desktop_integration_fixture(
    planned: &super::super::Planned,
    packages: &BTreeMap<String, PackageImage>,
) {
    const FIXTURE: &str = "desktop-integration";
    const TREE: &str = "cast-desktop-integration-fixture-1.0.0";
    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain exactly one package");
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-desktop-integration-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert_eq!(root_plan.summary.as_deref(), Some("Declarative desktop integration assets fixture"));
    assert_eq!(
        root_plan.description.as_deref(),
        Some(
            "A staged helper with validated desktop entry, AppStream, GSettings, MIME, and scalable icon declarations."
        )
    );

    let assets = [
        (
            "libexec/cast-desktop-integration-fixture",
            "integration/cast-desktop-integration-fixture",
            0o755,
        ),
        (
            "share/applications/io.cast.desktop-integration-fixture.desktop",
            "integration/io.cast.desktop-integration-fixture.desktop",
            0o644,
        ),
        (
            "share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml",
            "integration/io.cast.desktop-integration-fixture.metainfo.xml",
            0o644,
        ),
        (
            "share/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml",
            "integration/io.cast.desktop-integration-fixture.gschema.xml",
            0o644,
        ),
        (
            "share/mime/packages/application-x-cast-desktop-integration-fixture.xml",
            "integration/application-x-cast-desktop-integration-fixture.xml",
            0o644,
        ),
        (
            "share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg",
            "integration/io.cast.desktop-integration-fixture.svg",
            0o644,
        ),
        (
            "share/licenses/cast-desktop-integration-fixture/COPYING",
            "COPYING",
            0o644,
        ),
    ];
    assert_leaf_paths(FIXTURE, "out", root, assets.iter().map(|(target, _, _)| *target));
    assert_no_directories(FIXTURE, "out", root);
    for (target, source, permissions) in assets {
        let expected = if permissions == 0o755 {
            tracked_executable_bytes(TREE, source)
        } else {
            tracked_bytes(TREE, source)
        };
        assert_regular(FIXTURE, root, target, permissions, expected);
    }
    for generated in [
        "share/glib-2.0/schemas/gschemas.compiled",
        "share/mime/mime.cache",
        "share/applications/mimeinfo.cache",
        "share/icons/hicolor/icon-theme.cache",
    ] {
        assert!(
            !root.layouts.contains_key(generated),
            "{FIXTURE}: generated host cache leaked into the immutable output: {generated}"
        );
    }
    assert_exact_relations(
        FIXTURE,
        root,
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
