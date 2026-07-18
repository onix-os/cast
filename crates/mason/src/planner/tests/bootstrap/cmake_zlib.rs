const ZLIB_DEVEL_PACKAGE_ID: &str = "0d5c833db4a2874dd09368215ed24cd45e3f9850102720926dbd8f10e7ea9a15";
const ZLIB_RUNTIME_PACKAGE_ID: &str = "72f68a72d866271aa2f3db09dd636aed30faedf8ddc92f1c73b6ba0a24f29da8";

fn assert_cmake_zlib_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("missing bootstrap fixture `{name}`"))
    };
    let cmake = fixture("cmake");
    assert_eq!(cmake.package_ids.len(), 95, "cmake: package closure size drift");
    assert!(cmake.package_ids.iter().any(|id| id == ZLIB_DEVEL_PACKAGE_ID));
    assert!(cmake.package_ids.iter().any(|id| id == ZLIB_RUNTIME_PACKAGE_ID));

    for sibling in ["daemon-generated", "factory-override", "split"] {
        let sibling = fixture(sibling);
        assert_eq!(sibling.package_ids.len(), 94, "{}: shared CMake closure size drift", sibling.name);
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
