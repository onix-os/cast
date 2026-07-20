const NINJA_SHELL_DASH_PACKAGE_ID: &str =
    "dae7fed922f25eac0ef68aa9a5f6709f0725d2c4e9edb578475bf3624db2034c";
const NINJA_PACKAGE_ID: &str = "41f481df19218b770873e507191f388b57b7d79aaae908e37b7e26112db4e22e";
const ZLIB_DEVEL_PACKAGE_ID: &str = "0d5c833db4a2874dd09368215ed24cd45e3f9850102720926dbd8f10e7ea9a15";
const ZLIB_RUNTIME_PACKAGE_ID: &str = "72f68a72d866271aa2f3db09dd636aed30faedf8ddc92f1c73b6ba0a24f29da8";
const CMAKE_DERIVED_EXECUTION_FIXTURES: [&str; 7] = [
    "cmake",
    "daemon-generated",
    "external-test-vectors",
    "factory-override",
    "hooks-patch",
    "post-install-smoke-test",
    "split",
];
const MESON_DERIVED_EXECUTION_FIXTURES: [&str; 2] = ["meson", "multiple-sources"];

fn assert_cmake_zlib_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("missing bootstrap fixture `{name}`"))
    };
    let cmake = fixture("cmake");
    assert_eq!(cmake.package_ids.len(), 96, "cmake: package closure size drift");
    assert!(cmake.package_ids.iter().any(|id| id == ZLIB_DEVEL_PACKAGE_ID));
    assert!(cmake.package_ids.iter().any(|id| id == ZLIB_RUNTIME_PACKAGE_ID));

    for sibling in [
        "daemon-generated",
        "external-test-vectors",
        "factory-override",
        "post-install-smoke-test",
        "split",
    ] {
        let sibling = fixture(sibling);
        assert_eq!(sibling.package_ids.len(), 95, "{}: shared CMake closure size drift", sibling.name);
        assert!(
            !sibling.package_ids.iter().any(|id| id == ZLIB_DEVEL_PACKAGE_ID),
            "{}: CMake-only zlib-devel input leaked into its closure",
            sibling.name
        );
        let mut expected = sibling.package_ids.clone();
        expected.push(ZLIB_DEVEL_PACKAGE_ID.to_owned());
        expected.sort();
        assert_eq!(cmake.package_ids, expected, "cmake: dedicated zlib closure drift");
    }
    assert_eq!(fixture("hooks-patch").package_ids.len(), 96, "hooks-patch: package closure size drift");

    for name in CMAKE_DERIVED_EXECUTION_FIXTURES {
        let derived = fixture(name);
        let shell_providers = derived
            .package_ids
            .iter()
            .filter(|id| {
                indexed[*id]
                    .providers
                    .iter()
                    .any(|provider| provider.to_name() == "binary(sh)")
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            shell_providers,
            [NINJA_SHELL_DASH_PACKAGE_ID],
            "{name}: binary(sh) must have one exact provider in its frozen closure"
        );
    }

    let dash = &indexed[NINJA_SHELL_DASH_PACKAGE_ID];
    assert_eq!(dash.name.as_str(), "dash");
    assert_eq!(dash.version_identifier, "0.5.13.4");
    assert_eq!(dash.source_release, 19);
    assert_eq!(dash.build_release, 1);
    assert_eq!(dash.download_size, Some(87_879));
    assert_eq!(
        dash.uri.as_deref(),
        Some("../../../legacy/pool/d/dash/dash-0.5.13.4-19-1-x86_64.stone")
    );
    assert_eq!(
        dash.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "binary(dash)".to_owned(),
            "binary(sh)".to_owned(),
            "dash".to_owned(),
        ])
    );

    let zlib_devel = &indexed[ZLIB_DEVEL_PACKAGE_ID];
    assert_eq!(zlib_devel.name.as_str(), "zlib-devel");
    assert_eq!(zlib_devel.version_identifier, "2.3.3");
    assert_eq!(zlib_devel.source_release, 23);
    assert_eq!(zlib_devel.build_release, 1);
    assert_eq!(zlib_devel.download_size, Some(28_831));
    assert_eq!(
        zlib_devel.uri.as_deref(),
        Some("../../../legacy/pool/z/zlib-ng/zlib-devel-2.3.3-23-1-x86_64.stone")
    );
    assert_eq!(
        zlib_devel.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "cmake(zlib)".to_owned(),
            "pkgconfig(zlib)".to_owned(),
            "zlib-devel".to_owned(),
        ])
    );
    assert_eq!(
        zlib_devel.dependencies.iter().map(|dependency| dependency.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from(["zlib".to_owned()])
    );

    let zlib = &indexed[ZLIB_RUNTIME_PACKAGE_ID];
    assert_eq!(zlib.name.as_str(), "zlib");
    assert_eq!(zlib.version_identifier, "2.3.3");
    assert_eq!(zlib.source_release, 23);
    assert_eq!(zlib.build_release, 1);
    assert_eq!(zlib.download_size, Some(85_409));
    assert!(
        zlib.providers
            .iter()
            .any(|provider| provider.to_name() == "soname(libz.so.1(x86_64))"),
        "zlib runtime package lost its libz ABI provider"
    );
}
