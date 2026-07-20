const FONTCONFIG_PACKAGE_ID: &str =
    "13176ec161af3ef04bc317491634a7a518addb28ebefe429261d6ce02ae412f1";
const BROTLI_LIBS_PACKAGE_ID: &str =
    "508b08cab0d181f2da112463d2347ab3277937b3ea6853d68b72d8bba398e0a2";
const FREETYPE_PACKAGE_ID: &str =
    "5f3d91a7bc277253f0b7be784fca3e51a8b0f5e9c3356c00afca0cda83d8cc2b";
const LIBPNG_PACKAGE_ID: &str =
    "81a0304440fa51734e83e3b4c24cde4a2c0bfbb58290ccc0c3c5c408788929b5";

fn assert_font_family_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    assert_eq!(closure.packages.sha256.len(), 175, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 385_535_265,
        "bootstrap download byte total drift"
    );
    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "font-family")
        .expect("missing bootstrap fixture `font-family`");
    assert_eq!(fixture.package_ids.len(), 63, "font-family: package closure size drift");
    let download_bytes = fixture
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("font package has no declared size"))
        .sum::<u64>();
    assert_eq!(download_bytes, 213_892_544, "font-family: closure download bytes drifted");

    let custom = closure
        .fixtures
        .iter()
        .find(|candidate| candidate.name == "custom")
        .expect("missing bootstrap fixture `custom`");
    let custom_ids = custom.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let font_ids = fixture.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    assert!(custom_ids.is_subset(&font_ids));
    assert_eq!(
        font_ids.difference(&custom_ids).copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            FONTCONFIG_PACKAGE_ID,
            BROTLI_LIBS_PACKAGE_ID,
            FREETYPE_PACKAGE_ID,
            LIBPNG_PACKAGE_ID,
        ]),
        "font-family: closure must be exactly the custom baseline plus font scanning libraries"
    );

    for (request, expected_id, expected_name) in [
        ("binary(dash)", DASH_PACKAGE_ID, "dash"),
        ("binary(install)", INSTALL_PACKAGE_ID, "uutils-coreutils"),
        ("binary(fc-scan)", FONTCONFIG_PACKAGE_ID, "fontconfig"),
    ] {
        let providers = fixture
            .package_ids
            .iter()
            .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == request))
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(providers, [expected_id], "font-family: {request} provider drifted");
        assert_eq!(indexed[expected_id].name.as_str(), expected_name);
    }

    for (id, name, version, source_release, download_size, uri) in [
        (
            DASH_PACKAGE_ID,
            "dash",
            "0.5.13.4",
            19,
            87_879,
            "../../../legacy/pool/d/dash/dash-0.5.13.4-19-1-x86_64.stone",
        ),
        (
            INSTALL_PACKAGE_ID,
            "uutils-coreutils",
            "0.9.0",
            36,
            3_852_451,
            "../../../legacy/pool/u/uutils-coreutils/uutils-coreutils-0.9.0-36-1-x86_64.stone",
        ),
        (
            FONTCONFIG_PACKAGE_ID,
            "fontconfig",
            "2.18.2",
            20,
            203_637,
            "../../../pool/v0/f/fontconfig/fontconfig-2.18.2-20-1-x86_64.stone",
        ),
        (
            BROTLI_LIBS_PACKAGE_ID,
            "brotli-libs",
            "1.2.0",
            10,
            356_614,
            "../../../legacy/pool/b/brotli/brotli-libs-1.2.0-10-1-x86_64.stone",
        ),
        (
            FREETYPE_PACKAGE_ID,
            "freetype",
            "2.14.3",
            19,
            413_683,
            "../../../legacy/pool/f/freetype/freetype-2.14.3-19-1-x86_64.stone",
        ),
        (
            LIBPNG_PACKAGE_ID,
            "libpng",
            "1.6.58",
            15,
            106_790,
            "../../../legacy/pool/libp/libpng/libpng-1.6.58-15-1-x86_64.stone",
        ),
    ] {
        assert!(fixture.package_ids.iter().any(|candidate| candidate == id));
        let package = &indexed[id];
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, 1);
        assert_eq!(package.download_size, Some(download_size));
        assert_eq!(package.uri.as_deref(), Some(uri));
    }

    for sibling in closure.fixtures.iter().filter(|candidate| candidate.name != fixture.name) {
        for package_id in [FONTCONFIG_PACKAGE_ID, FREETYPE_PACKAGE_ID, LIBPNG_PACKAGE_ID] {
            assert!(
                !sibling.package_ids.iter().any(|id| id == package_id),
                "{}: font-family-only package {package_id} leaked into an unrelated closure",
                sibling.name
            );
        }
    }
}
