use sha2::{Digest as _, Sha256};

fn assert_gettext_localization_fixture(
    planned: &super::super::Planned,
    packages: &BTreeMap<String, PackageImage>,
) {
    const FIXTURE: &str = "gettext-localization";
    const TREE: &str = "cast-gettext-localization-fixture-1.0.0";
    const FR: &str = "share/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo";
    const DE: &str = "share/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo";
    const COPYING: &str = "share/licenses/cast-gettext-localization-fixture/COPYING";

    assert_eq!(packages.len(), 1, "{FIXTURE}: emitted bundle must contain one catalog package");
    let (output_name, root) = packages.first_key_value().unwrap();
    assert_eq!(output_name.as_str(), "cast-gettext-localization-fixture");
    let [root_plan] = planned.plan.outputs.as_slice() else {
        panic!("{FIXTURE}: frozen plan must contain exactly one output");
    };
    assert_eq!(root_plan.name, "out");
    assert_eq!(root_plan.package_name, *output_name);
    assert!(root_plan.include_in_manifest);
    assert_eq!(
        root_plan.summary.as_deref(),
        Some("Compiled French and German gettext catalogs")
    );
    assert_eq!(
        root_plan.description.as_deref(),
        Some("Two validated message catalogs whose translations are exercised by a build-only libc gettext consumer.")
    );
    assert!(root_plan.runtime_inputs.is_empty(), "{FIXTURE}: build tools leaked into runtime relations");

    assert_leaf_paths(FIXTURE, "out", root, [FR, DE, COPYING]);
    assert_no_directories(FIXTURE, "out", root);
    assert_regular(FIXTURE, root, COPYING, 0o644, tracked_bytes(TREE, "COPYING"));

    let assert_catalog = |path: &str, sha256: &str, source_message: &[u8], translated: &[u8]| {
        assert_eq!(root.layouts[path].mode & 0o777, 0o644, "{FIXTURE}: {path} permissions drift");
        let bytes = regular_bytes(FIXTURE, root, path);
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            sha256,
            "{FIXTURE}: {path} deterministic catalog bytes drifted"
        );
        assert!(
            bytes.starts_with(&[0xde, 0x12, 0x04, 0x95]),
            "{FIXTURE}: {path} is not a little-endian GNU MO catalog"
        );
        assert!(
            bytes.windows(source_message.len()).any(|window| window == source_message),
            "{FIXTURE}: {path} lost the source message"
        );
        assert!(
            bytes.windows(translated.len()).any(|window| window == translated),
            "{FIXTURE}: {path} lost its expected translation"
        );
    };
    assert_catalog(
        FR,
        "04b1a9c041b5ab2fe85001ce8403659cdace30989b56ff554d5a26703950f0a0",
        b"Hello from Cast",
        b"Bonjour de Cast",
    );
    assert_catalog(
        DE,
        "9fe202517728c7c64ccfedf2e97ef15467b94c9cbb8c981f7c69aa517fa7c7e4",
        b"Hello from Cast",
        b"Hallo von Cast",
    );
    assert_ne!(regular_bytes(FIXTURE, root, FR), regular_bytes(FIXTURE, root, DE));
    assert!(
        !root.layouts.contains_key("bin/gettext-consumer"),
        "{FIXTURE}: build-only consumer leaked into the package"
    );
    assert_exact_relations(
        FIXTURE,
        root,
        BTreeSet::new(),
        BTreeSet::from([root_plan.package_name.clone()]),
    );
}
