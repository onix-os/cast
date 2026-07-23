pub(super) fn expected(work_dir: &str) -> Vec<FrozenPhaseShape> {
    vec![
        phase("Prepare", vec![extract("cast-go-module-fixture")]),
        phase(
            "Setup",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: Vec::new(),
                script: super::super::GO_MODULE_SETUP_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Build",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/go".to_owned()],
                script: super::super::GO_MODULE_BUILD_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: super::super::GO_MODULE_INSTALL_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Check",
            vec![
                FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: vec!["/usr/bin/go".to_owned()],
                    script: super::super::GO_MODULE_CHECK_SCRIPT.to_owned(),
                },
                FrozenStepShape::RunBuilt {
                    program: Path::new(work_dir).join("cast-go-module-fixture").display().to_string(),
                    first_argument: Some("--self-test".to_owned()),
                },
            ],
        ),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(plan.execution.network, stone_recipe::derivation::NetworkMode::Disabled);
    assert_eq!(plan.sources.len(), 1);
    let [output] = plan.outputs.as_slice() else {
        panic!("go-module: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-go-module-fixture");
    assert!(output.include_in_manifest);
    assert!(output.runtime_inputs.is_empty());
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(bash)", "binary(go)", "binary(install)"]
    );

    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let executable = |
        phase,
        phase_name: &str,
        role: stone_recipe::derivation::JobExecutableRole,
    | stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase,
        phase_name: phase_name.to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step: 0,
        role,
    };
    let shell = |phase, name| {
        executable(
            phase,
            name,
            stone_recipe::derivation::JobExecutableRole::ShellInterpreter,
        )
    };
    let declared = |phase, name| {
        executable(
            phase,
            name,
            stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 },
        )
    };
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
            panic!("go-module: frozen lock must contain exactly one {request}");
        };
        assert_eq!(locked.package_id, package_id, "go-module: {request} provider drifted");
        assert_eq!(locked.output, "out");
        assert_eq!(locked.origins, origins, "go-module: {request} origins drifted");
    };
    assert_request(
        "binary(bash)",
        GETTEXT_BASH_PACKAGE_ID,
        vec![builder(0), shell(1, "Setup"), shell(2, "Build"), shell(3, "Install"), shell(4, "Check")],
    );
    assert_request(
        "binary(go)",
        GO_COMPILER_PACKAGE_ID,
        vec![builder(1), declared(2, "Build"), declared(4, "Check")],
    );
    assert_request(
        "binary(install)",
        GETTEXT_COREUTILS_PACKAGE_ID,
        vec![builder(2), declared(3, "Install")],
    );

    let go = plan
        .build_lock
        .packages
        .iter()
        .find(|package| package.package_id == GO_COMPILER_PACKAGE_ID)
        .expect("go-module: pinned Go compiler package is absent");
    assert_eq!(go.name, "golang");
    assert_eq!(go.version, "1.26.5-35-1");
    assert_eq!(go.architecture, "x86_64");
    assert_eq!(go.repository, "bootstrap");
    assert_eq!(go.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(), ["out"]);

    let check = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [
        stone_recipe::derivation::StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            working_dir,
            ..
        },
        stone_recipe::derivation::StepPlan::RunBuilt {
            program,
            args,
            working_dir: self_test_working_dir,
            ..
        },
    ] = check.steps.as_slice()
    else {
        panic!("go-module: frozen Check topology drifted");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(interpreter.requirement.canonical_name(), "binary(bash)");
    assert_eq!(script, super::super::GO_MODULE_CHECK_SCRIPT);
    assert_eq!(working_dir, "/mason/build/x86_64/cast-go-module-fixture");
    let [go_program] = declared_programs.as_slice() else {
        panic!("go-module: frozen Check Go declaration drifted");
    };
    assert_eq!(go_program.path, "/usr/bin/go");
    assert_eq!(go_program.requirement.canonical_name(), "binary(go)");
    assert_eq!(program, "/mason/build/x86_64/cast-go-module-fixture/cast-go-module-fixture");
    assert_eq!(args.as_slice(), ["--self-test"]);
    assert_eq!(self_test_working_dir, working_dir);
}
