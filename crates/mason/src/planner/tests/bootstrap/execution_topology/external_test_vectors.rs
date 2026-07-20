const BASH_PACKAGE_ID: &str = "20a6cfc76001152c45a7f77f1ee50bfdb816d0b67408cd6857f023022f37f0d9";
const COREUTILS_PACKAGE_ID: &str = "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf";

pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    vec![
        phase(
            "Prepare",
            vec![extract("cast-external-test-vectors-fixture")],
        ),
        phase("Setup", vec![run("cmake", "-G")]),
        phase("Build", vec![run("cmake", "--build")]),
        phase("Install", vec![run("cmake", "--install")]),
        phase_with_pre(
            "Check",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/cp".to_owned()],
                script: super::super::EXTERNAL_TEST_VECTORS_RAW_COPY_SCRIPT.to_owned(),
            }],
            vec![run("ctest", "--test-dir")],
        ),
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
        ["binary(sh)", "binary(ninja)", "binary(cp)"],
        "external-test-vectors: raw copy must remain an explicit native build input"
    );
    let [primary, vectors] = plan.sources.as_slice() else {
        panic!("external-test-vectors: frozen plan must retain exactly two sources");
    };
    assert!(matches!(
        primary,
        stone_recipe::derivation::LockedSource::Archive {
            order: 0,
            url,
            sha256,
            filename,
        } if url == super::super::EXTERNAL_TEST_VECTORS_ARCHIVE_URL
            && sha256 == super::super::EXTERNAL_TEST_VECTORS_ARCHIVE_SHA256
            && filename == "cast-external-test-vectors-fixture.tar"
    ));
    assert!(matches!(
        vectors,
        stone_recipe::derivation::LockedSource::Archive {
            order: 1,
            url,
            sha256,
            filename,
        } if url == super::super::EXTERNAL_TEST_VECTORS_RAW_URL
            && sha256 == super::super::EXTERNAL_TEST_VECTORS_RAW_SHA256
            && filename == "external-test-vectors.json"
    ));

    let prepare = job.phases.iter().find(|phase| phase.name == "Prepare").unwrap();
    assert!(matches!(
        prepare.steps.as_slice(),
        [stone_recipe::derivation::StepPlan::ExtractArchive {
            source: 0,
            destination,
            strip_components: 1,
        }] if destination == "cast-external-test-vectors-fixture"
    ));
    assert!(job.phases.iter().flat_map(|phase| phase.steps.iter()).all(|step| {
        !matches!(
            step,
            stone_recipe::derivation::StepPlan::ExtractArchive { source: 1, .. }
        )
    }));

    let check = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        working_dir,
        ..
    }] = check.pre.as_slice()
    else {
        panic!("external-test-vectors: Check must have exactly one typed raw-corpus prelude");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(interpreter.requirement.canonical_name(), "binary(bash)");
    let [copy] = declared_programs.as_slice() else {
        panic!("external-test-vectors: raw-corpus prelude must declare exactly one copy program");
    };
    assert_eq!(copy.path, "/usr/bin/cp");
    assert_eq!(copy.requirement.canonical_name(), "binary(cp)");
    assert_eq!(script, super::super::EXTERNAL_TEST_VECTORS_RAW_COPY_SCRIPT);
    assert_eq!(
        working_dir,
        "/mason/build/x86_64/cast-external-test-vectors-fixture"
    );

    let request = |name: &str| {
        let matches = plan
            .build_lock
            .requests
            .iter()
            .filter(|request| request.request == name)
            .collect::<Vec<_>>();
        let [request] = matches.as_slice() else {
            panic!("external-test-vectors: frozen lock must contain exactly one {name} request");
        };
        *request
    };
    let job_executable = |phase, phase_name: &str, section, role| {
        stone_recipe::derivation::InputOrigin::JobExecutable {
            job: 0,
            phase,
            phase_name: phase_name.to_owned(),
            section,
            step: 0,
            role,
        }
    };
    let bash = request("binary(bash)");
    assert_eq!(bash.package_id, BASH_PACKAGE_ID);
    assert_eq!(bash.output, "out");
    assert_eq!(
        bash.origins,
        [job_executable(
            4,
            "Check",
            stone_recipe::derivation::JobStepSection::Pre,
            stone_recipe::derivation::JobExecutableRole::ShellInterpreter,
        )]
    );
    let copy_request = request("binary(cp)");
    assert_eq!(copy_request.package_id, COREUTILS_PACKAGE_ID);
    assert_eq!(copy_request.output, "out");
    assert_eq!(
        copy_request.origins,
        [
            stone_recipe::derivation::InputOrigin::NativeBuild {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index: 0,
            },
            job_executable(
                4,
                "Check",
                stone_recipe::derivation::JobStepSection::Pre,
                stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 },
            ),
        ]
    );

    let run_program = |phase, phase_name: &str| {
        job_executable(
            phase,
            phase_name,
            stone_recipe::derivation::JobStepSection::Steps,
            stone_recipe::derivation::JobExecutableRole::RunProgram,
        )
    };
    for (name, provider_name, origins) in [
        (
            "binary(sh)",
            "dash",
            vec![stone_recipe::derivation::InputOrigin::BuilderTool {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index: 0,
            }],
        ),
        (
            "binary(cmake)",
            "cmake",
            vec![
                run_program(1, "Setup"),
                run_program(2, "Build"),
                run_program(3, "Install"),
            ],
        ),
        (
            "binary(ctest)",
            "cmake",
            vec![run_program(4, "Check")],
        ),
        (
            "binary(ninja)",
            "ninja",
            vec![stone_recipe::derivation::InputOrigin::BuilderTool {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index: 1,
            }],
        ),
    ] {
        let locked = request(name);
        let provider = plan
            .build_lock
            .packages
            .iter()
            .find(|package| package.package_id == locked.package_id)
            .unwrap_or_else(|| panic!("external-test-vectors: {name} provider package is absent"));
        assert_eq!(provider.name, provider_name, "external-test-vectors: {name} provider drifted");
        assert_eq!(locked.output, "out");
        assert_eq!(locked.origins, origins, "external-test-vectors: {name} origins drifted");
    }
}
