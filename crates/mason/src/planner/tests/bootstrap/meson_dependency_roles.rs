const FILE_PACKAGE_ID: &str = "035aaedec26b735d854d3d17ab82fec0ca88829346fbae764250e6eda07de7f5";
const LIBSECCOMP_PACKAGE_ID: &str = "8db61cf368b0425c5eec49273dec58bf99894fd10b2987bfeda7214ef3cbb43e";

fn assert_meson_dependency_role_bootstrap_contract(
    closure: &BootstrapClosure,
    indexed: &BTreeMap<String, Meta>,
) {
    let fixture = |name: &str| {
        closure
            .fixtures
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap_or_else(|| panic!("missing bootstrap fixture `{name}`"))
    };
    let meson = fixture("meson");
    assert_eq!(meson.package_ids.len(), 99, "meson: package closure size drift");
    for required in [
        NINJA_SHELL_DASH_PACKAGE_ID,
        ZLIB_DEVEL_PACKAGE_ID,
        ZLIB_RUNTIME_PACKAGE_ID,
        FILE_PACKAGE_ID,
        LIBSECCOMP_PACKAGE_ID,
    ] {
        assert!(
            meson.package_ids.iter().any(|id| id == required),
            "meson: dependency-role closure is missing {required}"
        );
    }
    let meson_download_bytes = meson
        .package_ids
        .iter()
        .map(|id| indexed[id].download_size.unwrap())
        .sum::<u64>();
    assert_eq!(meson_download_bytes, 235_944_987, "meson: closure download bytes drifted");

    for name in MESON_DERIVED_EXECUTION_FIXTURES {
        let derived = fixture(name);
        let shell_providers = derived
            .package_ids
            .iter()
            .filter(|id| {
                indexed[*id]
                    .providers
                    .iter()
                    .any(|provider| provider.to_name() == "binary(sh)")
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            shell_providers,
            [NINJA_SHELL_DASH_PACKAGE_ID],
            "{name}: binary(sh) must have one exact provider in its frozen closure"
        );
    }

    for sibling in closure.fixtures.iter().filter(|fixture| fixture.name != "meson") {
        assert!(
            !sibling.package_ids.iter().any(|id| id == FILE_PACKAGE_ID),
            "{}: Meson check-only package {FILE_PACKAGE_ID} leaked into an unrelated closure",
            sibling.name
        );
        if sibling.name != "system-integration-assets" {
            assert!(
                !sibling.package_ids.iter().any(|id| id == LIBSECCOMP_PACKAGE_ID),
                "{}: Meson check-only package {LIBSECCOMP_PACKAGE_ID} leaked into an unrelated closure",
                sibling.name
            );
        }
    }

    let file = &indexed[FILE_PACKAGE_ID];
    assert_eq!(file.name.as_str(), "file");
    assert_eq!(file.version_identifier, "5.48");
    assert_eq!(file.source_release, 12);
    assert_eq!(file.build_release, 1);
    assert_eq!(file.download_size, Some(477_257));
    assert_eq!(
        file.uri.as_deref(),
        Some("../../../legacy/pool/f/file/file-5.48-12-1-x86_64.stone")
    );
    assert_eq!(
        file.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "binary(file)".to_owned(),
            "file".to_owned(),
            "soname(libmagic.so.1(x86_64))".to_owned(),
        ])
    );
    assert_eq!(
        file.dependencies.iter().map(|dependency| dependency.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))".to_owned(),
            "soname(libbz2.so.1.0(x86_64))".to_owned(),
            "soname(libc.so.6(x86_64))".to_owned(),
            "soname(liblzma.so.5(x86_64))".to_owned(),
            "soname(libseccomp.so.2(x86_64))".to_owned(),
            "soname(libz.so.1(x86_64))".to_owned(),
            "soname(libzstd.so.1(x86_64))".to_owned(),
        ])
    );

    let libseccomp = &indexed[LIBSECCOMP_PACKAGE_ID];
    assert_eq!(libseccomp.name.as_str(), "libseccomp");
    assert_eq!(libseccomp.version_identifier, "2.6.1");
    assert_eq!(libseccomp.source_release, 7);
    assert_eq!(libseccomp.build_release, 1);
    assert_eq!(libseccomp.download_size, Some(46_135));
    assert_eq!(
        libseccomp.uri.as_deref(),
        Some("../../../pool/v0/libs/libseccomp/libseccomp-2.6.1-7-1-x86_64.stone")
    );
    assert_eq!(
        libseccomp.providers.iter().map(|provider| provider.to_name()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "libseccomp".to_owned(),
            "soname(libseccomp.so.2(x86_64))".to_owned(),
        ])
    );
    assert_eq!(
        libseccomp
            .dependencies
            .iter()
            .map(|dependency| dependency.to_name())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["soname(libc.so.6(x86_64))".to_owned()])
    );
}
