fn assert_external_test_vectors_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    assert_eq!(closure.packages.sha256.len(), 172, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 383_747_528,
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
    assert_eq!(external.package_ids.len(), 94, "external-test-vectors: CMake closure size drift");
    assert_eq!(
        external.package_ids, cmake.package_ids,
        "external-test-vectors: existing CMake closure must already cover Bash and the explicit copy tool"
    );

    for (request, expected_name) in [
        ("binary(bash)", "bash"),
        ("binary(cmake)", "cmake"),
        ("binary(cp)", "uutils-coreutils"),
        ("binary(ctest)", "cmake"),
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
    }
}
