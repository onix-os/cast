const PGO_BASH_PACKAGE_ID: &str = "20a6cfc76001152c45a7f77f1ee50bfdb816d0b67408cd6857f023022f37f0d9";
const PGO_CLANG_PACKAGE_ID: &str = "3422ccbe5a97f4344081793d15e1f0b36870ea2d61c07aac7f29816ce2d01776";
const PGO_LLVM_PACKAGE_ID: &str = "c5115c8ca49c97f7d6aa49fca6797e5a6e3b959abc66f98e63f8049f3c2bdad6";
const PGO_COREUTILS_PACKAGE_ID: &str = "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf";

fn assert_pgo_workload_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("{name}: exact bootstrap closure is absent"))
    };
    let custom = fixture("custom");
    let pgo = fixture("pgo-workload");
    let mut expected = custom.package_ids.clone();
    let insertion = expected.binary_search_by(|candidate| candidate.as_str().cmp(PGO_BASH_PACKAGE_ID));
    assert!(insertion.is_err(), "custom baseline unexpectedly already contains the PGO shell");
    expected.insert(insertion.unwrap_err(), PGO_BASH_PACKAGE_ID.to_owned());
    assert_eq!(
        pgo.package_ids, expected,
        "pgo-workload: closure must be exactly the custom baseline plus the PGO merge shell"
    );

    for (relation, package_id, name, version, source_release, build_release) in [
        (
            "binary(bash)",
            PGO_BASH_PACKAGE_ID,
            "bash",
            "5.3.15",
            32,
            1,
        ),
        (
            "binary(clang)",
            PGO_CLANG_PACKAGE_ID,
            "clang",
            "22.1.8",
            57,
            1,
        ),
        (
            "binary(llvm-profdata)",
            PGO_LLVM_PACKAGE_ID,
            "llvm",
            "22.1.8",
            57,
            1,
        ),
        (
            "binary(cp)",
            PGO_COREUTILS_PACKAGE_ID,
            "uutils-coreutils",
            "0.9.0",
            36,
            1,
        ),
    ] {
        let package = indexed
            .get(package_id)
            .unwrap_or_else(|| panic!("pgo-workload: pinned provider {package_id} is absent from the index"));
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, build_release);
        assert_eq!(package.architecture, "x86_64");
        assert!(
            package.providers.iter().any(|provider| provider.to_name() == relation),
            "pgo-workload: {package_id} no longer provides {relation}"
        );
    }
}
