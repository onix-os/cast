const DESKTOP_FILE_UTILS_PACKAGE_ID: &str =
    "e113587ef30d87c694e5240d8cbe36f440bccee72aa17f431915910fa729db7a";
const GLIB2_PACKAGE_ID: &str = "7c9d70535f23d9493d5ec1fd5666ce504ebfe62e222201ad8efa79e3ed88b55a";
const APPSTREAM_PACKAGE_ID: &str = "c414287fd5592ccf82bc3e5b0498779670b3a372d078f6a811967f7ac700e442";
const SHARED_MIME_INFO_PACKAGE_ID: &str =
    "e3dc2678a867c18553a3d86efa12981becf453fdd511392d7a75aebb17f4a386";
const HICOLOR_ICON_THEME_PACKAGE_ID: &str =
    "3e8e4f53f768174db200099a193d09f2f7bf9b18b23d28b8e428feb639023419";
const LIBFYAML_PACKAGE_ID: &str = "a035a7509f5d58b2d4072ccd0f8b450e5b58504bdf61857bd3d2b08d1cd641eb";
const LIBXMLB_PACKAGE_ID: &str = "fb34f3d06b4dfb9a068584fc2210b3b540fb273b090d3366dc43b7c75cc300a9";

fn assert_desktop_integration_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    assert_eq!(closure.packages.sha256.len(), 147, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 341_660_667,
        "bootstrap download byte total drift"
    );
    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "desktop-integration")
        .expect("missing bootstrap fixture `desktop-integration`");
    assert_eq!(fixture.package_ids.len(), 99, "desktop-integration: package closure size drift");
    let download_bytes = fixture
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("desktop integration package has no declared size"))
        .sum::<u64>();
    assert_eq!(
        download_bytes, 228_724_143,
        "desktop-integration: closure download bytes drifted"
    );

    for (request, expected_id, expected_name) in [
        ("binary(dash)", DASH_PACKAGE_ID, "dash"),
        ("binary(install)", INSTALL_PACKAGE_ID, "uutils-coreutils"),
        (
            "binary(desktop-file-validate)",
            DESKTOP_FILE_UTILS_PACKAGE_ID,
            "desktop-file-utils",
        ),
        ("binary(glib-compile-schemas)", GLIB2_PACKAGE_ID, "glib2"),
        ("binary(appstreamcli)", APPSTREAM_PACKAGE_ID, "appstream"),
        (
            "binary(update-mime-database)",
            SHARED_MIME_INFO_PACKAGE_ID,
            "shared-mime-info",
        ),
        ("binary(xmllint)", LIBXML2_PACKAGE_ID, "libxml2"),
        ("glib2", GLIB2_PACKAGE_ID, "glib2"),
        ("shared-mime-info", SHARED_MIME_INFO_PACKAGE_ID, "shared-mime-info"),
        ("hicolor-icon-theme", HICOLOR_ICON_THEME_PACKAGE_ID, "hicolor-icon-theme"),
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
            "desktop-integration: {request} must have one exact provider in its frozen closure"
        );
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
            DESKTOP_FILE_UTILS_PACKAGE_ID,
            "desktop-file-utils",
            "0.28",
            5,
            40_250,
            "../../../legacy/pool/d/desktop-file-utils/desktop-file-utils-0.28-5-1-x86_64.stone",
        ),
        (
            GLIB2_PACKAGE_ID,
            "glib2",
            "2.88.2",
            39,
            2_983_349,
            "../../../pool/v0/g/glib2/glib2-2.88.2-39-1-x86_64.stone",
        ),
        (
            APPSTREAM_PACKAGE_ID,
            "appstream",
            "1.1.3",
            18,
            912_459,
            "../../../pool/v0/a/appstream/appstream-1.1.3-18-1-x86_64.stone",
        ),
        (
            SHARED_MIME_INFO_PACKAGE_ID,
            "shared-mime-info",
            "2.5.1",
            11,
            754_844,
            "../../../pool/v0/s/shared-mime-info/shared-mime-info-2.5.1-11-1-x86_64.stone",
        ),
        (
            HICOLOR_ICON_THEME_PACKAGE_ID,
            "hicolor-icon-theme",
            "0.18",
            3,
            6_319,
            "../../../legacy/pool/h/hicolor-icon-theme/hicolor-icon-theme-0.18-3-1-x86_64.stone",
        ),
        (
            LIBXML2_PACKAGE_ID,
            "libxml2",
            "2.15.3",
            21,
            547_695,
            "../../../legacy/pool/libx/libxml2/libxml2-2.15.3-21-1-x86_64.stone",
        ),
        (
            LIBFYAML_PACKAGE_ID,
            "libfyaml",
            "0.9.6",
            7,
            271_630,
            "../../../pool/v0/libf/libfyaml/libfyaml-0.9.6-7-1-x86_64.stone",
        ),
        (
            LIBXMLB_PACKAGE_ID,
            "libxmlb",
            "0.3.28",
            12,
            88_829,
            "../../../pool/v0/libx/libxmlb/libxmlb-0.3.28-12-1-x86_64.stone",
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

    let desktop_only = [
        HICOLOR_ICON_THEME_PACKAGE_ID,
        LIBFYAML_PACKAGE_ID,
        APPSTREAM_PACKAGE_ID,
        DESKTOP_FILE_UTILS_PACKAGE_ID,
        SHARED_MIME_INFO_PACKAGE_ID,
        LIBXMLB_PACKAGE_ID,
    ];
    for sibling in closure.fixtures.iter().filter(|candidate| candidate.name != fixture.name) {
        for package_id in desktop_only {
            assert!(
                !sibling.package_ids.iter().any(|id| id == package_id),
                "{}: desktop-integration-only package {package_id} leaked into an unrelated closure",
                sibling.name
            );
        }
    }
}
