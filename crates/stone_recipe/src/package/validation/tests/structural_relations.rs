#[test]
fn every_direct_relation_group_reports_its_package_field() {
    let invalid = DependencySpec::Binary(String::new());

    let mut spec = package();
    spec.check_inputs = vec![invalid.clone()];
    assert_eq!(spec.validate().unwrap_err().field(), "check_inputs[0]");

    let mut spec = package();
    spec.outputs[0].runtime_inputs = vec![invalid.clone()];
    assert_eq!(spec.validate().unwrap_err().field(), "outputs[0].runtime_inputs[0]");

    let mut spec = package();
    spec.profiles.push(ProfileSpec {
        name: "native".to_owned(),
        builder: BuilderSpec {
            required_tools: vec![invalid.clone()],
            ..BuilderSpec::default()
        },
        hooks: HooksSpec::default(),
        native_build_inputs: Vec::new(),
        build_inputs: Vec::new(),
        check_inputs: Vec::new(),
    });
    assert_eq!(
        spec.validate().unwrap_err().field(),
        "profiles[0].builder.required_tools[0]"
    );
}

#[test]
fn semantically_duplicate_dependencies_are_rejected_per_role() {
    let mut duplicate = package();
    duplicate.build_inputs = vec![
        dependency("zlib"),
        DependencySpec::Output(OutputRef {
            package: PackageRef {
                name: "zlib".to_owned(),
            },
            output: "out".to_owned(),
        }),
    ];

    let error = duplicate.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "build_inputs[1]");
    assert!(error.to_string().contains("build_inputs[0]"));
}

#[test]
fn structural_step_values_are_validated_before_planning() {
    let mut duplicate_feature = package();
    duplicate_feature.builder.phases.build = PhaseSpec::new([StepSpec::CargoBuild {
        features: vec!["pcre2".to_owned(), "pcre2".to_owned()],
    }]);
    let error = duplicate_feature.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "builder.phases.build.steps[0].features[1]");

    let mut invalid_binary = package();
    invalid_binary.builder.phases.install = PhaseSpec::new([StepSpec::CargoInstall {
        binaries: vec!["../escape".to_owned()],
    }]);
    assert_eq!(
        invalid_binary.validate().unwrap_err().field(),
        "builder.phases.install.steps[0].binaries[0]"
    );

    let mut nul_argument = package();
    nul_argument.builder.phases.build = PhaseSpec::new([StepSpec::Run {
        program: binary_program("printf"),
        args: vec!["visible\0hidden".to_owned()],
    }]);
    assert_eq!(
        nul_argument.validate().unwrap_err().field(),
        "builder.phases.build.steps[0].args[0]"
    );

    let mut empty_shell = package();
    empty_shell.hooks.pre_install = vec![shell("  \n")];
    assert_eq!(
        empty_shell.validate().unwrap_err().field(),
        "hooks.pre_install[0].script"
    );

    let mut duplicate_program = package();
    duplicate_program.hooks.pre_install = vec![StepSpec::Shell {
        interpreter: binary_program("bash"),
        declared_programs: vec![binary_program("bash")],
        script: "echo hello".to_owned(),
    }];
    let error = duplicate_program.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::DuplicateValue { .. }));
    assert_eq!(error.field(), "hooks.pre_install[0].declared_programs[0].path");
}

#[test]
fn builder_contract_rejects_duplicate_environments_and_unsupported_hooks() {
    let mut duplicate = package();
    duplicate.builder.environment = vec![BuilderEnvironmentSpec::Cargo, BuilderEnvironmentSpec::Cargo];
    let error = duplicate.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::DuplicateBuilderEnvironment { .. }
    ));
    assert_eq!(error.field(), "builder.environment[1]");

    let mut unsupported = package();
    unsupported.builder.supported_hooks.build = false;
    unsupported.hooks.pre_build = vec![shell("prepare")];
    let error = unsupported.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::UnsupportedBuilderHook { .. }));
    assert_eq!(error.field(), "hooks.pre_build");
}

#[test]
fn duplicate_and_missing_outputs_are_rejected() {
    let mut missing = package();
    missing.outputs[0].name = "dev".to_owned();
    assert!(matches!(
        missing.validate(),
        Err(PackageConversionError::MissingRootOutput)
    ));

    let mut duplicate = package();
    duplicate.outputs.push(duplicate.outputs[0].clone());
    assert!(matches!(
        duplicate.validate(),
        Err(PackageConversionError::DuplicateOutput { .. })
    ));
}

#[test]
fn local_output_references_are_checked_for_missing_values_and_cycles() {
    let mut missing = package();
    missing.outputs[0]
        .runtime_inputs
        .push(DependencySpec::Output(OutputRef {
            package: PackageRef {
                name: "example".to_owned(),
            },
            output: "dev".to_owned(),
        }));
    assert!(matches!(
        missing.validate(),
        Err(PackageConversionError::MissingOutputReference { .. })
    ));

    let mut cyclic = package();
    cyclic.outputs[0].runtime_inputs.push(DependencySpec::Output(OutputRef {
        package: PackageRef {
            name: "example".to_owned(),
        },
        output: "out".to_owned(),
    }));
    assert!(matches!(
        cyclic.validate(),
        Err(PackageConversionError::OutputDependencyCycle { .. })
    ));
}
