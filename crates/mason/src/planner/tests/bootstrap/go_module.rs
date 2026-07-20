const GO_COMPILER_PACKAGE_ID: &str =
    "210231d9f3f937bf6190d7900ef197268dcc0b37b048d186661a84b5adf26c43";

fn assert_go_module_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    assert_eq!(closure.packages.sha256.len(), 179, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 388_713_448,
        "bootstrap download byte total drift"
    );
    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "go-module")
        .expect("missing bootstrap fixture `go-module`");
    assert_eq!(fixture.package_ids.len(), 71, "go-module: package closure size drift");
    let download_bytes = fixture
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("Go closure package has no declared size"))
        .sum::<u64>();
    assert_eq!(download_bytes, 254_299_874, "go-module: closure download bytes drifted");

    let userspace = closure
        .fixtures
        .iter()
        .find(|candidate| candidate.name == "userspace-profile")
        .expect("missing bootstrap fixture `userspace-profile`");
    assert_eq!(userspace.package_ids.len(), 70, "userspace baseline size drifted");
    let userspace_download_bytes = userspace
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("userspace package has no declared size"))
        .sum::<u64>();
    assert_eq!(userspace_download_bytes, 219_068_731, "userspace baseline bytes drifted");
    let userspace_ids = userspace.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let go_ids = fixture.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    assert!(userspace_ids.is_subset(&go_ids));
    assert_eq!(
        go_ids.difference(&userspace_ids).copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([GO_COMPILER_PACKAGE_ID]),
        "go-module: closure must be exactly userspace-profile plus the pinned Go compiler"
    );
    assert_eq!(fixture.package_ids[18], GO_COMPILER_PACKAGE_ID, "Go package sorted offset drifted");

    let providers = fixture
        .package_ids
        .iter()
        .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == "binary(go)"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(providers, [GO_COMPILER_PACKAGE_ID], "binary(go) provider drifted");

    let package = &indexed[GO_COMPILER_PACKAGE_ID];
    assert_eq!(package.name.as_str(), "golang");
    assert_eq!(package.version_identifier, "1.26.5");
    assert_eq!(package.source_release, 35);
    assert_eq!(package.build_release, 1);
    assert_eq!(package.architecture, "x86_64");
    assert_eq!(package.download_size, Some(35_231_143));
    assert_eq!(
        package.uri.as_deref(),
        Some("../../../pool/v0/g/golang/golang-1.26.5-35-1-x86_64.stone")
    );
    assert_eq!(
        package.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from(["binary(go)".to_owned(), "binary(gofmt)".to_owned(), "golang".to_owned()])
    );
    assert_eq!(
        package.dependencies.iter().map(|dependency| dependency.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from(["binary(ld.so)".to_owned(), "ca-certificates".to_owned()])
    );

    for sibling in closure.fixtures.iter().filter(|candidate| candidate.name != fixture.name) {
        assert!(
            !sibling.package_ids.iter().any(|id| id == GO_COMPILER_PACKAGE_ID),
            "{}: Go-only compiler leaked into an unrelated closure",
            sibling.name
        );
    }
}
