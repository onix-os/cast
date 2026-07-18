pub(super) fn expected(work_dir: &str) -> Vec<FrozenPhaseShape> {
    let run_built = |first_argument: &str| FrozenStepShape::RunBuilt {
        program: Path::new(work_dir).join("build/gettext-consumer").display().to_string(),
        first_argument: Some(first_argument.to_owned()),
    };
    vec![
        phase(
            "Prepare",
            vec![extract("cast-gettext-localization-fixture")],
        ),
        phase("Setup", vec![run("mkdir", "-p"), run("mkdir", "-p")]),
        phase(
            "Build",
            vec![
                run("msgfmt", "--check-format"),
                run("msgfmt", "--check-format"),
                run("cc", "-std=c11"),
            ],
        ),
        phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: super::super::GETTEXT_INSTALL_SCRIPT.to_owned(),
            }],
        ),
        phase(
            "Check",
            vec![run_built("fr_FR.utf8"), run_built("de_DE.utf8")],
        ),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(plan.sources.len(), 1);
    let [output] = plan.outputs.as_slice() else {
        panic!("gettext-localization: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-gettext-localization-fixture");
    assert!(output.include_in_manifest);
    assert!(output.runtime_inputs.is_empty(), "gettext-localization: build tools leaked into runtime inputs");
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(mkdir)",
            "binary(msgfmt)",
            "binary(cc)",
            "binary(bash)",
            "binary(install)",
        ]
    );

    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let run = |phase, phase_name: &str, step| stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase,
        phase_name: phase_name.to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step,
        role: stone_recipe::derivation::JobExecutableRole::RunProgram,
    };
    let shell = |role| stone_recipe::derivation::InputOrigin::JobExecutable {
        job: 0,
        phase: 3,
        phase_name: "Install".to_owned(),
        section: stone_recipe::derivation::JobStepSection::Steps,
        step: 0,
        role,
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
            panic!("gettext-localization: frozen lock must contain exactly one {request}");
        };
        assert_eq!(locked.package_id, package_id, "gettext-localization: {request} provider drifted");
        assert_eq!(locked.output, "out");
        assert_eq!(locked.origins, origins, "gettext-localization: {request} origins drifted");
    };
    assert_request(
        "binary(mkdir)",
        GETTEXT_COREUTILS_PACKAGE_ID,
        vec![builder(0), run(1, "Setup", 0), run(1, "Setup", 1)],
    );
    assert_request(
        "binary(msgfmt)",
        GETTEXT_PACKAGE_ID,
        vec![builder(1), run(2, "Build", 0), run(2, "Build", 1)],
    );
    assert_request(
        "binary(cc)",
        GETTEXT_CLANG_PACKAGE_ID,
        vec![builder(2), run(2, "Build", 2)],
    );
    assert_request(
        "binary(bash)",
        GETTEXT_BASH_PACKAGE_ID,
        vec![
            builder(3),
            shell(stone_recipe::derivation::JobExecutableRole::ShellInterpreter),
        ],
    );
    assert_request(
        "binary(install)",
        GETTEXT_COREUTILS_PACKAGE_ID,
        vec![
            builder(4),
            shell(stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 }),
        ],
    );
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| !matches!(request.request.as_str(), "gettext" | "gettext-devel")),
        "gettext-localization: package-level gettext request bypassed the exact msgfmt capability"
    );

    let check = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [
        stone_recipe::derivation::StepPlan::RunBuilt {
            program: fr_program,
            args: fr_args,
            working_dir: fr_working_dir,
            ..
        },
        stone_recipe::derivation::StepPlan::RunBuilt {
            program: de_program,
            args: de_args,
            working_dir: de_working_dir,
            ..
        },
    ] = check.steps.as_slice()
    else {
        panic!("gettext-localization: frozen Check topology drifted");
    };
    assert_eq!(fr_program, "/mason/build/x86_64/cast-gettext-localization-fixture/build/gettext-consumer");
    assert_eq!(de_program, fr_program);
    assert_eq!(
        fr_args.iter().map(String::as_str).collect::<Vec<_>>(),
        ["fr_FR.utf8", "build/locale", "Bonjour de Cast"]
    );
    assert_eq!(
        de_args.iter().map(String::as_str).collect::<Vec<_>>(),
        ["de_DE.utf8", "build/locale", "Hallo von Cast"]
    );
    assert_eq!(fr_working_dir, "/mason/build/x86_64/cast-gettext-localization-fixture");
    assert_eq!(de_working_dir, fr_working_dir);
}
