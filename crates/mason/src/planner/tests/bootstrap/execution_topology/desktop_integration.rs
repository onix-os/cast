pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![
        phase("Prepare", vec![extract("cast-desktop-integration-fixture")]),
        phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/dash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: super::super::DESKTOP_INTEGRATION_INSTALL_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Check",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/dash".to_owned(),
                declared_programs: vec![
                    "/usr/bin/desktop-file-validate".to_owned(),
                    "/usr/bin/glib-compile-schemas".to_owned(),
                    "/usr/bin/appstreamcli".to_owned(),
                    "/usr/bin/update-mime-database".to_owned(),
                    "/usr/bin/xmllint".to_owned(),
                    "/usr/bin/install".to_owned(),
                ],
                script: super::super::DESKTOP_INTEGRATION_CHECK_SCRIPT.to_owned(),
            }],
        ),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(plan.sources.len(), 1);
    let [output] = plan.outputs.as_slice() else {
        panic!("desktop-integration: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-desktop-integration-fixture");
    assert!(output.include_in_manifest);
    assert_eq!(
        output
            .runtime_inputs
            .iter()
            .map(|relation| match relation {
                stone_recipe::derivation::OutputRelation::Locked { relation, .. } => relation.canonical_name(),
                stone_recipe::derivation::OutputRelation::Planned { output } => {
                    panic!("desktop-integration: unexpected local runtime output {output}")
                }
            })
            .collect::<Vec<_>>(),
        ["binary(dash)", "glib2", "shared-mime-info", "hicolor-icon-theme"]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(dash)",
            "binary(install)",
            "binary(desktop-file-validate)",
            "binary(glib-compile-schemas)",
            "binary(appstreamcli)",
            "binary(update-mime-database)",
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
            panic!("desktop-integration: frozen lock must contain exactly one {request}");
        };
        assert_eq!(locked.package_id, package_id, "desktop-integration: {request} provider drifted");
        assert_eq!(locked.output, "out", "desktop-integration: {request} output drifted");
        assert_eq!(locked.origins, origins, "desktop-integration: {request} origins drifted");
    };
    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let check = |index| stone_recipe::derivation::InputOrigin::Check {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let runtime = |index| stone_recipe::derivation::InputOrigin::OutputRuntime {
        output: "out".to_owned(),
        index,
    };
    let shell = |phase, phase_name: &str| stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase,
        phase_name: phase_name.to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step: 0,
        role: stone_recipe::derivation::JobExecutableRole::ShellInterpreter,
    };
    let declared = |phase, phase_name: &str, index| stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase,
        phase_name: phase_name.to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step: 0,
        role: stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index },
    };
    assert_request(
        "binary(dash)",
        DASH_PACKAGE_ID,
        vec![builder(0), runtime(0), shell(1, "Install"), shell(2, "Check")],
    );
    assert_request(
        "binary(install)",
        INSTALL_PACKAGE_ID,
        vec![builder(1), declared(1, "Install", 0), declared(2, "Check", 5)],
    );
    for (index, (request, package_id)) in [
        ("binary(desktop-file-validate)", DESKTOP_FILE_UTILS_PACKAGE_ID),
        ("binary(glib-compile-schemas)", GLIB2_PACKAGE_ID),
        ("binary(appstreamcli)", APPSTREAM_PACKAGE_ID),
        ("binary(update-mime-database)", SHARED_MIME_INFO_PACKAGE_ID),
        ("binary(xmllint)", LIBXML2_PACKAGE_ID),
    ]
    .into_iter()
    .enumerate()
    {
        let index = u32::try_from(index).unwrap();
        assert_request(request, package_id, vec![check(index), declared(2, "Check", index)]);
    }
    for (index, (request, package_id)) in [
        ("glib2", GLIB2_PACKAGE_ID),
        ("shared-mime-info", SHARED_MIME_INFO_PACKAGE_ID),
        ("hicolor-icon-theme", HICOLOR_ICON_THEME_PACKAGE_ID),
    ]
    .into_iter()
    .enumerate()
    {
        assert_request(request, package_id, vec![runtime(u32::try_from(index + 1).unwrap())]);
    }

    let check_phase = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        working_dir,
        ..
    }] = check_phase.steps.as_slice()
    else {
        panic!("desktop-integration: frozen Check topology drifted");
    };
    assert_eq!(interpreter.path, "/usr/bin/dash");
    assert_eq!(interpreter.requirement.canonical_name(), "binary(dash)");
    assert_eq!(script, super::super::DESKTOP_INTEGRATION_CHECK_SCRIPT);
    assert_eq!(working_dir, "/mason/build/x86_64/cast-desktop-integration-fixture");
    let expected = [
        (
            "/usr/bin/desktop-file-validate",
            "binary(desktop-file-validate)",
            DESKTOP_FILE_UTILS_PACKAGE_ID,
        ),
        (
            "/usr/bin/glib-compile-schemas",
            "binary(glib-compile-schemas)",
            GLIB2_PACKAGE_ID,
        ),
        ("/usr/bin/appstreamcli", "binary(appstreamcli)", APPSTREAM_PACKAGE_ID),
        (
            "/usr/bin/update-mime-database",
            "binary(update-mime-database)",
            SHARED_MIME_INFO_PACKAGE_ID,
        ),
        ("/usr/bin/xmllint", "binary(xmllint)", LIBXML2_PACKAGE_ID),
        ("/usr/bin/install", "binary(install)", INSTALL_PACKAGE_ID),
    ];
    assert_eq!(declared_programs.len(), expected.len());
    for (program, (path, request, _package_id)) in declared_programs.iter().zip(expected) {
        assert_eq!(program.path, path);
        assert_eq!(program.requirement.canonical_name(), request);
    }
}
