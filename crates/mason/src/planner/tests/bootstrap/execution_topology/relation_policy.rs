pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![phase(
        "Install",
        vec![FrozenStepShape::Shell {
            interpreter: "/usr/bin/bash".to_owned(),
            declared_programs: vec!["/usr/bin/install".to_owned()],
            script: super::super::RELATION_POLICY_INSTALL_SCRIPT.to_owned(),
        }],
    )]
}

pub(super) fn assert_contract(plan: &stone_recipe::derivation::DerivationPlan) {
    const FIXTURE: &str = "relation-policy";
    assert!(plan.sources.is_empty(), "{FIXTURE}: frozen sources must be empty");
    assert_eq!(
        plan.execution.network,
        stone_recipe::derivation::NetworkMode::Disabled,
        "{FIXTURE}: frozen execution must not admit network access"
    );
    assert_eq!(plan.package.name, "cast-relation-policy-fixture");
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(bash)",
            "binary(install)",
            "sysbinary(ldconfig)",
            "pkgconfig32(zlib)",
        ],
        "{FIXTURE}: exact manifest BuildDepends roles drifted"
    );

    let [output] = plan.outputs.as_slice() else {
        panic!("{FIXTURE}: plan must freeze exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-relation-policy-fixture");
    assert!(output.include_in_manifest);
    assert_eq!(
        output.summary.as_deref(),
        Some("Typed dependency-role policy fixture")
    );
    assert_eq!(
        output.description.as_deref(),
        Some("Pinned build and runtime capabilities with exact provider identities.")
    );
    assert!(output.conflicts.is_empty());
    assert_eq!(
        output
            .runtime_inputs
            .iter()
            .map(|relation| match relation {
                stone_recipe::derivation::OutputRelation::Locked {
                    relation,
                    reference,
                } => (relation.canonical_name(), reference.package_id.as_str()),
                stone_recipe::derivation::OutputRelation::Planned { output } => {
                    panic!("{FIXTURE}: runtime relation unexpectedly targets local output {output}")
                }
            })
            .collect::<Vec<_>>(),
        [
            (
                "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))".to_owned(),
                RELATION_POLICY_GLIBC_PACKAGE_ID,
            ),
            (
                "soname(libz.so.1(x86_64))".to_owned(),
                RELATION_POLICY_ZLIB_PACKAGE_ID,
            ),
        ]
    );

    let request = |requirement: &str| {
        let matches = plan
            .build_lock
            .requests
            .iter()
            .filter(|request| request.request == requirement)
            .collect::<Vec<_>>();
        let [request] = matches.as_slice() else {
            panic!("{FIXTURE}: build lock must contain exactly one {requirement} request");
        };
        *request
    };
    let native = request("sysbinary(ldconfig)");
    assert_eq!(native.package_id, RELATION_POLICY_GLIBC_PACKAGE_ID);
    assert_eq!(native.output, "out");
    assert_eq!(
        native.origins,
        [stone_recipe::derivation::InputOrigin::NativeBuild {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        }]
    );
    let build = request("pkgconfig32(zlib)");
    assert_eq!(build.package_id, RELATION_POLICY_ZLIB_32BIT_DEVEL_PACKAGE_ID);
    assert_eq!(build.output, "out");
    assert_eq!(
        build.origins,
        [stone_recipe::derivation::InputOrigin::Build {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        }]
    );
    for (index, relation, package_id) in [
        (
            0,
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))",
            RELATION_POLICY_GLIBC_PACKAGE_ID,
        ),
        (
            1,
            "soname(libz.so.1(x86_64))",
            RELATION_POLICY_ZLIB_PACKAGE_ID,
        ),
    ] {
        let runtime = request(relation);
        assert_eq!(runtime.package_id, package_id);
        assert_eq!(runtime.output, "out");
        assert_eq!(
            runtime.origins,
            [stone_recipe::derivation::InputOrigin::OutputRuntime {
                output: "out".to_owned(),
                index,
            }]
        );
    }
}
