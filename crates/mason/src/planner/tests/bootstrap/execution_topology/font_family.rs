pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![
        phase("Prepare", vec![extract("cast-font-family-fixture")]),
        phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/dash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: super::super::FONT_FAMILY_INSTALL_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Check",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/dash".to_owned(),
                declared_programs: vec!["/usr/bin/fc-scan".to_owned()],
                script: super::super::FONT_FAMILY_CHECK_SCRIPT.to_owned(),
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
        panic!("font-family: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-font-family-fixture");
    assert!(output.include_in_manifest);
    assert!(output.runtime_inputs.is_empty());
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(dash)", "binary(install)", "binary(fc-scan)"]
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
            panic!("font-family: frozen lock must contain exactly one {request}");
        };
        assert_eq!(locked.package_id, package_id, "font-family: {request} provider drifted");
        assert_eq!(locked.output, "out", "font-family: {request} output drifted");
        assert_eq!(locked.origins, origins, "font-family: {request} origins drifted");
    };
    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let check = |index| stone_recipe::derivation::InputOrigin::Check {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
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
        vec![builder(0), shell(1, "Install"), shell(2, "Check")],
    );
    assert_request(
        "binary(install)",
        INSTALL_PACKAGE_ID,
        vec![builder(1), declared(1, "Install", 0)],
    );
    assert_request(
        "binary(fc-scan)",
        FONTCONFIG_PACKAGE_ID,
        vec![check(0), declared(2, "Check", 0)],
    );

    let check_phase = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        working_dir,
        ..
    }] = check_phase.steps.as_slice()
    else {
        panic!("font-family: frozen Check topology drifted");
    };
    assert_eq!(interpreter.path, "/usr/bin/dash");
    assert_eq!(interpreter.requirement.canonical_name(), "binary(dash)");
    assert_eq!(script, super::super::FONT_FAMILY_CHECK_SCRIPT);
    assert_eq!(working_dir, "/mason/build/x86_64/cast-font-family-fixture");
    let [fc_scan] = declared_programs.as_slice() else {
        panic!("font-family: frozen Check declaration drifted");
    };
    assert_eq!(fc_scan.path, "/usr/bin/fc-scan");
    assert_eq!(fc_scan.requirement.canonical_name(), "binary(fc-scan)");
}
