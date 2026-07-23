pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![
        phase(
            "Prepare",
            vec![extract("cast-system-integration-assets-fixture")],
        ),
        phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/dash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: super::super::SYSTEM_INTEGRATION_INSTALL_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Check",
            vec![
                FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: Vec::new(),
                    script: super::super::SYSTEM_INTEGRATION_STAGED_HELPER_CHECK.to_owned(),
                },
                FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec![
                        "/usr/bin/systemd-analyze".to_owned(),
                        "/usr/bin/systemd-sysusers".to_owned(),
                        "/usr/bin/systemd-tmpfiles".to_owned(),
                        "/usr/bin/udevadm".to_owned(),
                        "/usr/bin/xmllint".to_owned(),
                    ],
                    script: super::super::SYSTEM_INTEGRATION_VALIDATION_SCRIPT.to_owned(),
                },
            ],
        ),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(plan.sources.len(), 1);
    let [output] = plan.outputs.as_slice() else {
        panic!("system-integration-assets: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-system-integration-assets-fixture");
    assert!(output.include_in_manifest);
    assert_eq!(
        output
            .runtime_inputs
            .iter()
            .map(|relation| match relation {
                stone_recipe::derivation::OutputRelation::Locked { relation, .. } => relation.canonical_name(),
                stone_recipe::derivation::OutputRelation::Planned { output } => {
                    panic!("system-integration-assets: unexpected local runtime output {output}")
                }
            })
            .collect::<Vec<_>>(),
        [
            "binary(dash)",
            "systemd",
            "systemd-sysusers",
            "systemd-tmpfiles",
            "systemd-udev",
            "polkit",
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(dash)",
            "binary(install)",
            "binary(systemd-analyze)",
            "binary(systemd-sysusers)",
            "binary(systemd-tmpfiles)",
            "binary(udevadm)",
            "binary(xmllint)",
        ]
    );

    let assert_request = |
        request: &str,
        package_id: &str,
        origins: Vec<stone_recipe::derivation::InputOrigin>,
    | {
        let matches = plan
            .build_lock
            .requests
            .iter()
            .filter(|candidate| candidate.request == request)
            .collect::<Vec<_>>();
        let [locked] = matches.as_slice() else {
            panic!("system-integration-assets: frozen lock must contain exactly one {request}");
        };
        assert_eq!(locked.package_id, package_id, "system-integration-assets: {request} provider drifted");
        assert_eq!(locked.output, "out", "system-integration-assets: {request} output drifted");
        assert_eq!(locked.origins, origins, "system-integration-assets: {request} origins drifted");
    };
    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let runtime = |index| stone_recipe::derivation::InputOrigin::OutputRuntime {
        output: "out".to_owned(),
        index,
    };
    let shell = |phase, phase_name: &str, step| stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase,
        phase_name: phase_name.to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step,
        role: stone_recipe::derivation::JobExecutableRole::ShellInterpreter,
    };
    assert_request(
        "binary(dash)",
        DASH_PACKAGE_ID,
        vec![builder(0), runtime(0), shell(1, "Install", 0), shell(2, "Check", 0), shell(2, "Check", 1)],
    );
    assert_request(
        "binary(install)",
        INSTALL_PACKAGE_ID,
        vec![
            builder(1),
            stone_recipe::derivation::InputOrigin::JobExecutable {
                job: 0,
                phase: 1,
                phase_name: "Install".to_owned(),
                section: stone_recipe::derivation::JobStepSection::Steps,
                step: 0,
                role: stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 },
            },
        ],
    );
    for (index, (request, package_id)) in [
        ("systemd", SYSTEMD_PACKAGE_ID),
        ("systemd-sysusers", SYSTEMD_SYSUSERS_PACKAGE_ID),
        ("systemd-tmpfiles", SYSTEMD_TMPFILES_PACKAGE_ID),
        ("systemd-udev", SYSTEMD_UDEV_PACKAGE_ID),
        ("polkit", POLKIT_PACKAGE_ID),
    ]
    .into_iter()
    .enumerate()
    {
        assert_request(request, package_id, vec![runtime(u32::try_from(index + 1).unwrap())]);
    }

    let check = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [
        stone_recipe::derivation::StepPlan::Shell {
            interpreter: helper_interpreter,
            declared_programs: helper_programs,
            script: helper_script,
            working_dir: helper_working_dir,
            ..
        },
        stone_recipe::derivation::StepPlan::Shell {
            interpreter: validator_interpreter,
            declared_programs: validators,
            script: validator_script,
            working_dir: validator_working_dir,
            ..
        },
    ] = check.steps.as_slice()
    else {
        panic!("system-integration-assets: frozen Check topology drifted");
    };
    assert_eq!(helper_interpreter.path, "/usr/bin/dash");
    assert_eq!(helper_interpreter.requirement.canonical_name(), "binary(dash)");
    assert!(helper_programs.is_empty());
    assert_eq!(helper_script, super::super::SYSTEM_INTEGRATION_STAGED_HELPER_CHECK);
    assert_eq!(helper_working_dir, "/mason/build/x86_64/cast-system-integration-assets-fixture");
    assert_eq!(validator_interpreter.path, "/usr/bin/dash");
    assert_eq!(validator_interpreter.requirement.canonical_name(), "binary(dash)");
    assert_eq!(validator_script, super::super::SYSTEM_INTEGRATION_VALIDATION_SCRIPT);
    assert_eq!(validator_working_dir, helper_working_dir);

    let expected = [
        ("/usr/bin/systemd-analyze", "binary(systemd-analyze)", SYSTEMD_PACKAGE_ID),
        (
            "/usr/bin/systemd-sysusers",
            "binary(systemd-sysusers)",
            SYSTEMD_SYSUSERS_PACKAGE_ID,
        ),
        (
            "/usr/bin/systemd-tmpfiles",
            "binary(systemd-tmpfiles)",
            SYSTEMD_TMPFILES_PACKAGE_ID,
        ),
        ("/usr/bin/udevadm", "binary(udevadm)", SYSTEMD_UDEV_PACKAGE_ID),
        ("/usr/bin/xmllint", "binary(xmllint)", LIBXML2_PACKAGE_ID),
    ];
    assert_eq!(validators.len(), expected.len());
    for (index, (validator, (path, request, package_id))) in validators.iter().zip(expected).enumerate() {
        let index = u32::try_from(index).unwrap();
        assert_eq!(validator.path, path);
        assert_eq!(validator.requirement.canonical_name(), request);
        assert_request(
            request,
            package_id,
            vec![
            stone_recipe::derivation::InputOrigin::Check {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index,
            },
            stone_recipe::derivation::InputOrigin::JobExecutable {
                job: 0,
                phase: 2,
                phase_name: "Check".to_owned(),
                section: stone_recipe::derivation::JobStepSection::Steps,
                step: 1,
                role: stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index },
            },
            ],
        );
    }
}
