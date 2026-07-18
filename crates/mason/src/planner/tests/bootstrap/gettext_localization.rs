const GETTEXT_BASH_PACKAGE_ID: &str = "20a6cfc76001152c45a7f77f1ee50bfdb816d0b67408cd6857f023022f37f0d9";
const GETTEXT_COREUTILS_PACKAGE_ID: &str = "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf";
const GETTEXT_CLANG_PACKAGE_ID: &str = "3422ccbe5a97f4344081793d15e1f0b36870ea2d61c07aac7f29816ce2d01776";
const GETTEXT_PACKAGE_ID: &str = "2fd48ed7722fb7ec4da4fcc6ec9e8709915a6b5b17f5c58499a32d3303e76891";
const GETTEXT_LIBS_PACKAGE_ID: &str = "39d4b0efb3531a2c5e7f7cda8567782ee2691534a4f2b129bba24e7d2307e203";
const GETTEXT_LIBUNISTRING_PACKAGE_ID: &str =
    "96d3833bdc5c54030cad4be531714f23e6d04f87346964e293db75fe463235c4";
const GETTEXT_GLIBC_PACKAGE_ID: &str = "ae5d2ec54e5776dfdae0b5b1b54fd00308031b197644380926b7cb7422b13e9e";
const GETTEXT_LIBTEXTSTYLE_PACKAGE_ID: &str =
    "e36b8b814e6eb8483a39f49b83795b420ebbb99832d9a41c5b487cf9b871a17e";

fn assert_gettext_localization_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    assert_eq!(closure.packages.sha256.len(), 147, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 341_660_667,
        "bootstrap download byte total drift"
    );
    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "gettext-localization")
        .expect("missing bootstrap fixture `gettext-localization`");
    assert_eq!(fixture.package_ids.len(), 63, "gettext-localization: package closure size drift");
    let download_bytes = fixture
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("gettext package has no declared size"))
        .sum::<u64>();
    assert_eq!(
        download_bytes, 218_293_997,
        "gettext-localization: closure download bytes drifted"
    );

    for (request, expected_id, expected_name) in [
        ("binary(mkdir)", GETTEXT_COREUTILS_PACKAGE_ID, "uutils-coreutils"),
        ("binary(msgfmt)", GETTEXT_PACKAGE_ID, "gettext"),
        ("binary(cc)", GETTEXT_CLANG_PACKAGE_ID, "clang"),
        ("binary(bash)", GETTEXT_BASH_PACKAGE_ID, "bash"),
        ("binary(install)", GETTEXT_COREUTILS_PACKAGE_ID, "uutils-coreutils"),
    ] {
        let providers = fixture
            .package_ids
            .iter()
            .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == request))
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            providers,
            [expected_id],
            "gettext-localization: {request} must have one exact provider in its frozen closure"
        );
        assert_eq!(indexed[expected_id].name.as_str(), expected_name);
    }

    for (id, name) in [
        (GETTEXT_PACKAGE_ID, "gettext"),
        (GETTEXT_LIBS_PACKAGE_ID, "gettext-libs"),
        (GETTEXT_LIBUNISTRING_PACKAGE_ID, "libunistring"),
        (GETTEXT_GLIBC_PACKAGE_ID, "glibc"),
        (GETTEXT_LIBTEXTSTYLE_PACKAGE_ID, "libtextstyle"),
    ] {
        assert!(fixture.package_ids.iter().any(|candidate| candidate == id));
        assert_eq!(indexed[id].name.as_str(), name);
    }
    assert!(
        fixture.package_ids.iter().all(|id| indexed[id].name.as_str() != "gettext-devel"),
        "gettext-localization: unrequested gettext-devel leaked into the closure"
    );

    for (id, name, version, source_release, download_size, uri) in [
        (
            GETTEXT_PACKAGE_ID,
            "gettext",
            "1.0",
            11,
            2_550_728,
            "../../../legacy/pool/g/gettext/gettext-1.0-11-1-x86_64.stone",
        ),
        (
            GETTEXT_LIBS_PACKAGE_ID,
            "gettext-libs",
            "1.0",
            11,
            347_642,
            "../../../legacy/pool/g/gettext/gettext-libs-1.0-11-1-x86_64.stone",
        ),
        (
            GETTEXT_LIBTEXTSTYLE_PACKAGE_ID,
            "libtextstyle",
            "1.0",
            11,
            234_789,
            "../../../legacy/pool/g/gettext/libtextstyle-1.0-11-1-x86_64.stone",
        ),
    ] {
        let package = &indexed[id];
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, 1);
        assert_eq!(package.download_size, Some(download_size));
        assert_eq!(package.uri.as_deref(), Some(uri));
    }

    for sibling in closure.fixtures.iter().filter(|candidate| candidate.name != fixture.name) {
        for gettext_only in [GETTEXT_PACKAGE_ID, GETTEXT_LIBS_PACKAGE_ID, GETTEXT_LIBTEXTSTYLE_PACKAGE_ID] {
            assert!(
                !sibling.package_ids.iter().any(|id| id == gettext_only),
                "{}: gettext-localization-only package {gettext_only} leaked into an unrelated closure",
                sibling.name
            );
        }
    }

    let glibc = &indexed[GETTEXT_GLIBC_PACKAGE_ID];
    assert_eq!(glibc.name.as_str(), "glibc");
    assert_eq!(glibc.version_identifier, "2.43+git.dae425b5");
    assert_eq!(glibc.source_release, 40);
    assert_eq!(glibc.build_release, 1);
    assert_eq!(glibc.download_size, Some(18_898_433));
    assert_eq!(
        glibc.uri.as_deref(),
        Some("../../../pool/v0/g/glibc/glibc-2.43+git.dae425b5-40-1-x86_64.stone")
    );
}
