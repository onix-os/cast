const RELATION_POLICY_GLIBC_PACKAGE_ID: &str =
    "ae5d2ec54e5776dfdae0b5b1b54fd00308031b197644380926b7cb7422b13e9e";
const RELATION_POLICY_ZLIB_PACKAGE_ID: &str =
    "72f68a72d866271aa2f3db09dd636aed30faedf8ddc92f1c73b6ba0a24f29da8";
const RELATION_POLICY_ZLIB_DEVEL_PACKAGE_ID: &str =
    "0d5c833db4a2874dd09368215ed24cd45e3f9850102720926dbd8f10e7ea9a15";
const RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID: &str =
    "597a9aee2c90fdb7c8eeb388b5b069906988e3ca2956e5e811d652206e51f6f5";
const RELATION_POLICY_ZLIB_32BIT_PACKAGE_ID: &str =
    "81f83043804d36dda99e50754801bd9dad01e264965f088481e26d3bc5986338";
const RELATION_POLICY_GLIBC_32BIT_PACKAGE_ID: &str =
    "b2e2a7432b1cf38d517ad07bf72d90db6c20078ee0323f2a163553922ad6b02a";
const RELATION_POLICY_LIBGCC_32BIT_PACKAGE_ID: &str =
    "b78129859b6ddac37c881ab977a36078397cf3613e8e76fe03e5a33dc3a318d6";

const RELATION_POLICY_EXACT_PROVIDERS: [(&str, &str); 4] = [
    ("sysbinary(ldconfig)", RELATION_POLICY_GLIBC_PACKAGE_ID),
    (
        "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))",
        RELATION_POLICY_GLIBC_PACKAGE_ID,
    ),
    (
        "soname(libz.so.1(x86_64))",
        RELATION_POLICY_ZLIB_PACKAGE_ID,
    ),
    (
        "pkgconfig32(zlib)",
        RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID,
    ),
];

fn assert_relation_policy_bootstrap_contract(closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
    let fixture = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "relation-policy")
        .expect("missing bootstrap fixture `relation-policy`");
    assert_eq!(fixture.package_ids.len(), 64, "relation-policy: package closure size drift");
    let generated = closure
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "generated-config")
        .expect("missing bootstrap fixture `generated-config`");
    let fixture_ids = fixture.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let generated_ids = generated
        .package_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert!(generated_ids.is_subset(&fixture_ids));
    assert_eq!(
        fixture_ids.difference(&generated_ids).copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            RELATION_POLICY_ZLIB_DEVEL_PACKAGE_ID,
            RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID,
            RELATION_POLICY_ZLIB_32BIT_PACKAGE_ID,
            RELATION_POLICY_GLIBC_32BIT_PACKAGE_ID,
            RELATION_POLICY_LIBGCC_32BIT_PACKAGE_ID,
        ]),
        "relation-policy: closure must be exactly generated-config plus the typed relation providers"
    );

    for (relation, expected_id) in RELATION_POLICY_EXACT_PROVIDERS {
        let providers = fixture
            .package_ids
            .iter()
            .filter(|id| {
                indexed[*id]
                    .providers
                    .iter()
                    .any(|provider| provider.to_name() == relation)
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            providers,
            [expected_id],
            "relation-policy: {relation} must resolve to one exact pinned provider"
        );
    }

    for (id, name, version, source_release, download_size, uri) in [
        (
            RELATION_POLICY_GLIBC_PACKAGE_ID,
            "glibc",
            "2.43+git.dae425b5",
            40,
            18_898_433,
            "../../../pool/v0/g/glibc/glibc-2.43+git.dae425b5-40-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_ZLIB_PACKAGE_ID,
            "zlib",
            "2.3.3",
            23,
            85_409,
            "../../../legacy/pool/z/zlib-ng/zlib-2.3.3-23-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_ZLIB_DEVEL_PACKAGE_ID,
            "zlib-devel",
            "2.3.3",
            23,
            28_831,
            "../../../legacy/pool/z/zlib-ng/zlib-devel-2.3.3-23-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID,
            "zlib-32bit-devel",
            "2.3.3",
            23,
            3_220,
            "../../../legacy/pool/z/zlib-ng/zlib-32bit-devel-2.3.3-23-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_ZLIB_32BIT_PACKAGE_ID,
            "zlib-32bit",
            "2.3.3",
            23,
            87_015,
            "../../../legacy/pool/z/zlib-ng/zlib-32bit-2.3.3-23-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_GLIBC_32BIT_PACKAGE_ID,
            "glibc-32bit",
            "2.43+git.dae425b5",
            40,
            2_998_142,
            "../../../pool/v0/g/glibc/glibc-32bit-2.43+git.dae425b5-40-1-x86_64.stone",
        ),
        (
            RELATION_POLICY_LIBGCC_32BIT_PACKAGE_ID,
            "libgcc-32bit",
            "16.1.0+git.0daeef2c",
            25,
            89_806,
            "../../../pool/v0/g/gcc/libgcc-32bit-16.1.0+git.0daeef2c-25-1-x86_64.stone",
        ),
    ] {
        assert!(fixture.package_ids.iter().any(|candidate| candidate == id));
        let package = &indexed[id];
        assert_eq!(package.name.as_str(), name);
        assert_eq!(package.version_identifier, version);
        assert_eq!(package.source_release, source_release);
        assert_eq!(package.build_release, 1);
        assert_eq!(package.architecture, "x86_64");
        assert_eq!(package.download_size, Some(download_size));
        assert_eq!(package.uri.as_deref(), Some(uri));
    }

    let relation_names = |id: &str, providers: bool| {
        let package = &indexed[id];
        if providers {
            package.providers.iter().map(|relation| relation.to_name()).collect::<BTreeSet<_>>()
        } else {
            package
                .dependencies
                .iter()
                .map(|relation| relation.to_name())
                .collect::<BTreeSet<_>>()
        }
    };
    assert_eq!(
        relation_names(RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID, true),
        BTreeSet::from([
            "cmake(zlib)".to_owned(),
            "pkgconfig32(zlib)".to_owned(),
            "zlib-32bit-devel".to_owned(),
        ])
    );
    assert_eq!(
        relation_names(RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID, false),
        BTreeSet::from(["zlib-32bit".to_owned(), "zlib-devel".to_owned()])
    );
    assert_eq!(
        relation_names(RELATION_POLICY_ZLIB_32BIT_PACKAGE_ID, false),
        BTreeSet::from([
            "soname(libc.so.6(386))".to_owned(),
            "soname(libgcc_s.so.1(386))".to_owned(),
        ])
    );
    assert_eq!(
        relation_names(RELATION_POLICY_GLIBC_32BIT_PACKAGE_ID, false),
        BTreeSet::from(["glibc".to_owned()])
    );
    assert_eq!(
        relation_names(RELATION_POLICY_LIBGCC_32BIT_PACKAGE_ID, false),
        BTreeSet::from(["soname(libc.so.6(386))".to_owned()])
    );

    for sibling in closure.fixtures.iter().filter(|candidate| candidate.name != "relation-policy") {
        for relation_only in [
            RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID,
            RELATION_POLICY_ZLIB_32BIT_PACKAGE_ID,
            RELATION_POLICY_GLIBC_32BIT_PACKAGE_ID,
            RELATION_POLICY_LIBGCC_32BIT_PACKAGE_ID,
        ] {
            assert!(
                !sibling.package_ids.iter().any(|id| id == relation_only),
                "{}: relation-policy-only package {relation_only} leaked into an unrelated closure",
                sibling.name
            );
        }
    }
}
