const DASH_PACKAGE_ID: &str = "dae7fed922f25eac0ef68aa9a5f6709f0725d2c4e9edb578475bf3624db2034c";
const SYSTEMD_PACKAGE_ID: &str = "a977eedc7597ad835f117a108c8f0d601d10c75ff8769d2ade0af46e463f68bf";
const SYSTEMD_SYSUSERS_PACKAGE_ID: &str = "6eac4ec6cf3ef935bd541a79be66c9fb439d5db751aec926f7a2ac43ac2391b0";
const SYSTEMD_TMPFILES_PACKAGE_ID: &str = "c379878b882aeebb388fb85adc4b8b9b2beb4dc77d19fdf9445fc04352b5c6b7";
const SYSTEMD_UDEV_PACKAGE_ID: &str = "99c2f9f2f1c51b90275c64e93a86e922e448f8e973e100cec68c8d52efaf83cd";
const LIBXML2_PACKAGE_ID: &str = "f9ca3142d2264f46d387e0af1136ec5f85d917ee1f0c7fb5c8ba08861d77dfa5";
const POLKIT_PACKAGE_ID: &str = "d748fdbe4d5461db6d9927fad74c461b65bb77dbe3e732487c91d074ac4bb6a4";

fn assert_system_integration_assets_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    assert_eq!(closure.packages.sha256.len(), 179, "bootstrap package count drift");
    assert_eq!(
        closure.packages.total_download_bytes, 388_713_448,
        "bootstrap download byte total drift"
    );
    let system = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "system-integration-assets")
        .expect("missing bootstrap fixture `system-integration-assets`");
    assert_eq!(system.package_ids.len(), 121, "system-integration-assets: package closure size drift");
    let download_bytes = system
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.expect("system integration package has no declared size"))
        .sum::<u64>();
    assert_eq!(
        download_bytes, 243_949_875,
        "system-integration-assets: closure download bytes drifted"
    );

    for (request, expected_id, expected_name) in [
        ("binary(dash)", DASH_PACKAGE_ID, "dash"),
        ("binary(install)", INSTALL_PACKAGE_ID, "uutils-coreutils"),
        ("systemd", SYSTEMD_PACKAGE_ID, "systemd"),
        ("systemd-sysusers", SYSTEMD_SYSUSERS_PACKAGE_ID, "systemd-sysusers"),
        ("systemd-tmpfiles", SYSTEMD_TMPFILES_PACKAGE_ID, "systemd-tmpfiles"),
        ("systemd-udev", SYSTEMD_UDEV_PACKAGE_ID, "systemd-udev"),
        ("polkit", POLKIT_PACKAGE_ID, "polkit"),
        ("binary(systemd-analyze)", SYSTEMD_PACKAGE_ID, "systemd"),
        ("binary(systemd-sysusers)", SYSTEMD_SYSUSERS_PACKAGE_ID, "systemd-sysusers"),
        ("binary(systemd-tmpfiles)", SYSTEMD_TMPFILES_PACKAGE_ID, "systemd-tmpfiles"),
        ("binary(udevadm)", SYSTEMD_UDEV_PACKAGE_ID, "systemd-udev"),
        ("binary(xmllint)", LIBXML2_PACKAGE_ID, "libxml2"),
    ] {
        let providers = system
            .package_ids
            .iter()
            .filter(|id| indexed[*id].providers.iter().any(|provider| provider.to_name() == request))
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            providers,
            [expected_id],
            "system-integration-assets: {request} must have one exact provider in its frozen closure"
        );
        assert_eq!(indexed[expected_id].name.as_str(), expected_name);
    }

    for (package_id, name, version, source_release, build_release, download_size, uri) in [
        (
            DASH_PACKAGE_ID,
            "dash",
            "0.5.13.4",
            19,
            1,
            87_879,
            "../../../legacy/pool/d/dash/dash-0.5.13.4-19-1-x86_64.stone",
        ),
        (
            INSTALL_PACKAGE_ID,
            "uutils-coreutils",
            "0.9.0",
            36,
            1,
            3_852_451,
            "../../../legacy/pool/u/uutils-coreutils/uutils-coreutils-0.9.0-36-1-x86_64.stone",
        ),
        (
            SYSTEMD_PACKAGE_ID,
            "systemd",
            "259.7",
            87,
            1,
            2_698_283,
            "../../../pool/v0/s/systemd/systemd-259.7-87-1-x86_64.stone",
        ),
        (
            SYSTEMD_SYSUSERS_PACKAGE_ID,
            "systemd-sysusers",
            "259.7",
            87,
            1,
            32_584,
            "../../../pool/v0/s/systemd/systemd-sysusers-259.7-87-1-x86_64.stone",
        ),
        (
            SYSTEMD_TMPFILES_PACKAGE_ID,
            "systemd-tmpfiles",
            "259.7",
            87,
            1,
            69_391,
            "../../../pool/v0/s/systemd/systemd-tmpfiles-259.7-87-1-x86_64.stone",
        ),
        (
            SYSTEMD_UDEV_PACKAGE_ID,
            "systemd-udev",
            "259.7",
            87,
            1,
            1_494_520,
            "../../../pool/v0/s/systemd/systemd-udev-259.7-87-1-x86_64.stone",
        ),
        (
            LIBXML2_PACKAGE_ID,
            "libxml2",
            "2.15.3",
            21,
            1,
            547_695,
            "../../../legacy/pool/libx/libxml2/libxml2-2.15.3-21-1-x86_64.stone",
        ),
        (
            POLKIT_PACKAGE_ID,
            "polkit",
            "127",
            10,
            1,
            98_229,
            "../../../legacy/pool/p/polkit/polkit-127-10-1-x86_64.stone",
        ),
    ] {
        assert!(system.package_ids.iter().any(|id| id == package_id));
        let package = &indexed[package_id];
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, build_release);
        assert_eq!(package.download_size, Some(download_size));
        assert_eq!(package.uri.as_deref(), Some(uri));
    }
}
