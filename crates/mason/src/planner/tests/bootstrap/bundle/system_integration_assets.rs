fn assert_system_integration_assets_fixture(
    planned: &super::super::Planned,
    packages: &BTreeMap<String, PackageImage>,
) {
    const FIXTURE: &str = "system-integration-assets";
    const TREE: &str = "cast-system-integration-assets-fixture-1.0.0";
    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain exactly one package");
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-system-integration-assets-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert_eq!(root_plan.summary.as_deref(), Some("Declarative system integration assets fixture"));
    assert_eq!(
        root_plan.description.as_deref(),
        Some(
            "A staged helper plus systemd, sysusers, tmpfiles, udev, and self-contained polkit declarations with offline syntax checks."
        )
    );

    let assets = [
        (
            "libexec/cast-system-integration-fixture",
            "integration/cast-system-integration-fixture",
            0o755,
        ),
        (
            "lib/systemd/system/cast-system-integration-fixture.service",
            "integration/cast-system-integration-fixture.service",
            0o644,
        ),
        (
            "lib/sysusers.d/cast-system-integration-fixture.conf",
            "integration/cast-system-integration-fixture.sysusers",
            0o644,
        ),
        (
            "lib/tmpfiles.d/cast-system-integration-fixture.conf",
            "integration/cast-system-integration-fixture.tmpfiles",
            0o644,
        ),
        (
            "lib/udev/rules.d/70-cast-system-integration-fixture.rules",
            "integration/70-cast-system-integration-fixture.rules",
            0o644,
        ),
        (
            "share/polkit-1/rules.d/io.cast.SystemIntegrationFixture.rules",
            "integration/io.cast.SystemIntegrationFixture.rules",
            0o644,
        ),
        (
            "share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy",
            "integration/io.cast.SystemIntegrationFixture.policy",
            0o644,
        ),
        (
            "share/licenses/cast-system-integration-assets-fixture/LICENSE",
            "LICENSE",
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
    assert_exact_relations(
        FIXTURE,
        root,
        planned_output_dependencies(planned, root_plan),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
