fn assert_external_test_vectors_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    assert_eq!(closure.packages.sha256.len(), 175, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 385_535_265,
        "bootstrap download byte total drift"
    );
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("missing bootstrap fixture `{name}`"))
    };
    let external = fixture("external-test-vectors");
    let cmake = fixture("daemon-generated");
    assert_eq!(external.package_ids.len(), 95, "external-test-vectors: CMake closure size drift");
    assert_eq!(
        external.package_ids, cmake.package_ids,
        "external-test-vectors: existing CMake closure must already cover Bash, Dash, and the explicit copy tool"
    );

    for (request, expected_name) in [
        ("binary(bash)", "bash"),
        ("binary(cmake)", "cmake"),
        ("binary(cp)", "uutils-coreutils"),
        ("binary(ctest)", "cmake"),
        ("binary(sh)", "dash"),
        ("binary(ninja)", "ninja"),
    ] {
        let providers = external
            .package_ids
            .iter()
            .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == request))
            .collect::<Vec<_>>();
        let [provider] = providers.as_slice() else {
            panic!("external-test-vectors: {request} must have one exact provider in its frozen closure");
        };
        assert_eq!(indexed[*provider].name.as_str(), expected_name);
        if request == "binary(sh)" {
            assert_eq!(provider.as_str(), NINJA_SHELL_DASH_PACKAGE_ID);
        }
    }
}
