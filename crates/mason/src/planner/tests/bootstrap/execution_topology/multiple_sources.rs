pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![
        phase(
            "Prepare",
            vec![extract("application"), run("mkdir", "-p"), run("cp", "-Ra")],
        ),
        phase_with_pre(
            "Setup",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/cp".to_owned()],
                script: super::super::MULTIPLE_SOURCES_RAW_COPY_SCRIPT.to_owned(),
            }],
            vec![run("meson", "setup")],
        ),
        phase("Build", vec![run("meson", "compile")]),
        phase("Install", vec![run("meson", "install")]),
        phase("Check", vec![run("meson", "test")]),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(cmake)", "binary(ninja)", "binary(pkgconf)", "binary(cp)"],
        "multiple-sources: explicit raw copy must remain a native build input"
    );

    let prepare = job.phases.iter().find(|phase| phase.name == "Prepare").unwrap();
    let [
        stone_recipe::derivation::StepPlan::ExtractArchive {
            source,
            destination,
            strip_components,
        },
        stone_recipe::derivation::StepPlan::Run {
            program: mkdir,
            args: mkdir_args,
            working_dir: mkdir_working_dir,
            ..
        },
        stone_recipe::derivation::StepPlan::Run {
            program: copy_git,
            args: copy_git_args,
            working_dir: copy_git_working_dir,
            ..
        },
    ] = prepare.steps.as_slice()
    else {
        panic!("multiple-sources: Prepare must extract archive 0 and materialize Git source 1");
    };
    assert_eq!((*source, destination.as_str(), *strip_components), (0, "application", 1));
    assert_eq!(mkdir.path, "/usr/bin/mkdir");
    assert_eq!(mkdir.requirement.canonical_name(), "binary(mkdir)");
    assert_eq!(mkdir_args.as_slice(), ["-p", "vendor-protocol"]);
    assert_eq!(mkdir_working_dir, "/mason/build/x86_64");
    assert_eq!(copy_git.path, "/usr/bin/cp");
    assert_eq!(copy_git.requirement.canonical_name(), "binary(cp)");
    assert_eq!(
        copy_git_args.as_slice(),
        [
            "-Ra",
            "--no-preserve=ownership",
            "/mason/sourcedir/vendor-protocol/.",
            "vendor-protocol",
        ]
    );
    assert_eq!(copy_git_working_dir, "/mason/build/x86_64");

    let setup = job.phases.iter().find(|phase| phase.name == "Setup").unwrap();
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        working_dir,
        ..
    }] = setup.pre.as_slice()
    else {
        panic!("multiple-sources: Setup must have exactly one typed raw-source copy prelude");
    };
    let [copy_raw] = declared_programs.as_slice() else {
        panic!("multiple-sources: raw-source prelude must declare exactly one copy program");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(interpreter.requirement.canonical_name(), "binary(bash)");
    assert_eq!(copy_raw.path, "/usr/bin/cp");
    assert_eq!(copy_raw.requirement.canonical_name(), "binary(cp)");
    assert_eq!(script, super::super::MULTIPLE_SOURCES_RAW_COPY_SCRIPT);
    assert_eq!(working_dir, "/mason/build/x86_64/application");

    let copy_request = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "binary(cp)")
        .expect("multiple-sources: frozen build lock omitted binary(cp)");
    assert_eq!(
        copy_request.package_id,
        "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf"
    );
    for origin in [
        stone_recipe::derivation::InputOrigin::NativeBuild {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        },
        stone_recipe::derivation::InputOrigin::JobExecutable {
            job: 0,
            phase: 0,
            phase_name: "Prepare".to_owned(),
            section: stone_recipe::derivation::JobStepSection::Steps,
            step: 2,
            role: stone_recipe::derivation::JobExecutableRole::RunProgram,
        },
        stone_recipe::derivation::InputOrigin::JobExecutable {
            job: 0,
            phase: 1,
            phase_name: "Setup".to_owned(),
            section: stone_recipe::derivation::JobStepSection::Pre,
            step: 0,
            role: stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 },
        },
    ] {
        assert!(
            copy_request.origins.contains(&origin),
            "multiple-sources: binary(cp) lost origin {origin:?}"
        );
    }
}
