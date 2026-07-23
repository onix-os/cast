const PYTHON_PACKAGE_ID: &str = "57c7b5a7bda8628ee1b5943b58e9a672354e3948fb08f23cf6b137dde01bcc10";
const PYTHON_BUILD_PACKAGE_ID: &str = "72ece186ca5952eb4e2ded78b4a8f62bf61a606515c37352d1f74c204848eaed";
const PYTHON_INSTALLER_PACKAGE_ID: &str = "4a39d1b53afdf3505d0a349b4627c2386e9228ce0cab7aecbec735ce4c603af9";
const PYTHON_SETUPTOOLS_PACKAGE_ID: &str = "61c66d8caa536f2dd26ffe0724a79b6a9209a88ad9e040762413510e7afa5b0e";
const PYTHON_PYTEST_PACKAGE_ID: &str = "68732b606f6873f26b80c120225188c1a49036ed0abac9f9efdc6062523b7a36";
const PYTHON_TYPING_EXTENSIONS_PACKAGE_ID: &str = "64c5765414a46d8519e7c32b827ab3ac44ee71c9230e18b189a4dcf556ec3507";

pub(super) fn expected() -> Vec<FrozenPhaseShape> {
    let shell = |script: &str, declared_python: bool| FrozenStepShape::Shell {
        interpreter: "/usr/bin/bash".to_owned(),
        declared_programs: if declared_python {
            vec!["/usr/bin/python3".to_owned()]
        } else {
            Vec::new()
        },
        script: script.to_owned(),
    };
    vec![
        phase("Prepare", vec![extract("cast-python-module-fixture")]),
        phase("Setup", vec![shell(super::super::PYTHON_MODULE_SETUP_SCRIPT, false)]),
        phase("Build", vec![shell(super::super::PYTHON_MODULE_BUILD_SCRIPT, true)]),
        phase("Install", vec![shell(super::super::PYTHON_MODULE_INSTALL_SCRIPT, true)]),
        phase("Check", vec![shell(super::super::PYTHON_MODULE_CHECK_SCRIPT, true)]),
    ]
}

pub(super) fn assert_contract(
    plan: &stone_recipe::derivation::DerivationPlan,
    job: &stone_recipe::derivation::JobPlan,
) {
    assert_eq!(plan.execution.network, stone_recipe::derivation::NetworkMode::Disabled);
    assert_eq!(plan.sources.len(), 1);
    let [output] = plan.outputs.as_slice() else {
        panic!("python-module: frozen plan must emit exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-python-module-fixture");
    assert!(output.include_in_manifest);
    assert_eq!(
        output
            .runtime_inputs
            .iter()
            .map(|relation| match relation {
                stone_recipe::derivation::OutputRelation::Locked { relation, reference } => {
                    (relation.canonical_name(), reference.package_id.as_str())
                }
                stone_recipe::derivation::OutputRelation::Planned { output } => {
                    panic!("python-module: unexpected planned runtime output {output}")
                }
            })
            .collect::<Vec<_>>(),
        [
            ("binary(python3)".to_owned(), PYTHON_PACKAGE_ID),
            (
                "python(typing-extensions)".to_owned(),
                PYTHON_TYPING_EXTENSIONS_PACKAGE_ID,
            ),
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(bash)",
            "binary(python3)",
            "python(build)",
            "python(installer)",
            "python(setuptools)",
            "python(pytest)",
        ],
        "python-module: manifest build/check boundary drifted"
    );

    let builder = |index| stone_recipe::derivation::InputOrigin::BuilderTool {
        selection: stone_recipe::derivation::PackageInputSelection::Package,
        index,
    };
    let native = |index| stone_recipe::derivation::InputOrigin::NativeBuild {
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
    let python_policy = stone_recipe::derivation::InputOrigin::Policy {
        source: "policy.glu".to_owned(),
        field: "build_root.analyzer_tools.python".to_owned(),
        index: 0,
    };
    let python_analyzer = stone_recipe::derivation::InputOrigin::Analyzer {
        role: stone_recipe::derivation::AnalyzerRole::Python,
    };
    let executable = |phase, phase_name: &str, role: stone_recipe::derivation::JobExecutableRole| {
        stone_recipe::derivation::InputOrigin::JobExecutable {
            job: 0,
            phase,
            phase_name: phase_name.to_owned(),
            section: stone_recipe::derivation::JobStepSection::Steps,
            step: 0,
            role,
        }
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
    let assert_request = |request: &str, package_id: &str, origins: Vec<stone_recipe::derivation::InputOrigin>| {
        let matches = plan
            .build_lock
            .requests
            .iter()
            .filter(|candidate| candidate.request == request)
            .collect::<Vec<_>>();
        let [locked] = matches.as_slice() else {
            panic!("python-module: frozen lock must contain exactly one {request}");
        };
        assert_eq!(
            locked.package_id, package_id,
            "python-module: {request} provider drifted"
        );
        assert_eq!(locked.output, "out");
        assert_eq!(locked.origins, origins, "python-module: {request} origins drifted");
    };
    assert_request(
        "binary(bash)",
        GETTEXT_BASH_PACKAGE_ID,
        vec![
            builder(0),
            shell(1, "Setup"),
            shell(2, "Build"),
            shell(3, "Install"),
            shell(4, "Check"),
        ],
    );
    assert_request(
        "binary(python3)",
        PYTHON_PACKAGE_ID,
        vec![
            builder(1),
            runtime(0),
            python_policy,
            declared(2, "Build"),
            declared(3, "Install"),
            declared(4, "Check"),
            python_analyzer,
        ],
    );
    assert_request("python(build)", PYTHON_BUILD_PACKAGE_ID, vec![native(0)]);
    assert_request("python(installer)", PYTHON_INSTALLER_PACKAGE_ID, vec![native(1)]);
    assert_request("python(setuptools)", PYTHON_SETUPTOOLS_PACKAGE_ID, vec![native(2)]);
    assert_request("python(pytest)", PYTHON_PYTEST_PACKAGE_ID, vec![check(0)]);
    assert_request(
        "python(typing-extensions)",
        PYTHON_TYPING_EXTENSIONS_PACKAGE_ID,
        vec![runtime(1)],
    );

    for (package_id, name) in [
        (PYTHON_PACKAGE_ID, "python"),
        (PYTHON_BUILD_PACKAGE_ID, "python-build"),
        (PYTHON_INSTALLER_PACKAGE_ID, "python-installer"),
        (PYTHON_SETUPTOOLS_PACKAGE_ID, "python-setuptools"),
        (PYTHON_PYTEST_PACKAGE_ID, "python-pytest"),
        (PYTHON_TYPING_EXTENSIONS_PACKAGE_ID, "python-typing_extensions"),
    ] {
        let package = plan
            .build_lock
            .packages
            .iter()
            .find(|package| package.package_id == package_id)
            .unwrap_or_else(|| panic!("python-module: pinned package {name} is absent"));
        assert_eq!(package.name, name);
        assert_eq!(package.architecture, "x86_64");
        assert_eq!(package.repository, "bootstrap");
        assert_eq!(
            package
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["out"]
        );
    }

    let phase = |name: &str| job.phases.iter().find(|phase| phase.name == name).unwrap();
    for (name, script, has_python) in [
        ("Setup", super::super::PYTHON_MODULE_SETUP_SCRIPT, false),
        ("Build", super::super::PYTHON_MODULE_BUILD_SCRIPT, true),
        ("Install", super::super::PYTHON_MODULE_INSTALL_SCRIPT, true),
        ("Check", super::super::PYTHON_MODULE_CHECK_SCRIPT, true),
    ] {
        let [
            stone_recipe::derivation::StepPlan::Shell {
                interpreter,
                declared_programs,
                script: actual_script,
                working_dir,
                ..
            },
        ] = phase(name).steps.as_slice()
        else {
            panic!("python-module: frozen {name} topology drifted");
        };
        assert_eq!(interpreter.path, "/usr/bin/bash");
        assert_eq!(interpreter.requirement.canonical_name(), "binary(bash)");
        assert_eq!(actual_script, script);
        assert_eq!(working_dir, "/mason/build/x86_64/cast-python-module-fixture");
        if has_python {
            let [python] = declared_programs.as_slice() else {
                panic!("python-module: {name} must declare exactly one Python program");
            };
            assert_eq!(python.path, "/usr/bin/python3");
            assert_eq!(python.requirement.canonical_name(), "binary(python3)");
        } else {
            assert!(declared_programs.is_empty());
        }
    }
}
