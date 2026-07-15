#[test]
fn package_budget_accepts_exact_metadata_and_text_limits() {
    let mut exact_metadata = package();
    let mut limits = PackageValidationLimits {
        max_metadata_bytes: exact_metadata.meta.homepage.len(),
        ..PackageValidationLimits::default()
    };
    exact_metadata.validate_with_limits(limits).unwrap();

    exact_metadata.meta.homepage.push('/');
    let error = exact_metadata.validate_with_limits(limits).unwrap_err();
    assert_limit(
        error,
        "meta.homepage",
        exact_metadata.meta.homepage.len(),
        exact_metadata.meta.homepage.len() - 1,
        "bytes",
    );

    let mut exact_text = package();
    exact_text.outputs[0].summary = Some("12345678".to_owned());
    limits.max_metadata_bytes = PackageValidationLimits::default().max_metadata_bytes;
    limits.max_text_bytes = 8;
    exact_text.validate_with_limits(limits).unwrap();

    exact_text.outputs[0].summary = Some("123456789".to_owned());
    let error = exact_text.validate_with_limits(limits).unwrap_err();
    assert_limit(error, "outputs[0].summary", 9, 8, "bytes");
}

#[test]
fn package_budget_caps_major_package_collections_at_the_boundary() {
    let output_limits = PackageValidationLimits {
        max_outputs: 1,
        ..PackageValidationLimits::default()
    };
    let mut outputs = package();
    outputs.validate_with_limits(output_limits).unwrap();
    let mut output = outputs.outputs[0].clone();
    output.name = "dev".to_owned();
    outputs.outputs.push(output);
    assert_limit(
        outputs.validate_with_limits(output_limits).unwrap_err(),
        "outputs",
        2,
        1,
        "items",
    );

    let profile_limits = PackageValidationLimits {
        max_profiles: 0,
        ..PackageValidationLimits::default()
    };
    let mut profiles = package();
    profiles.validate_with_limits(profile_limits).unwrap();
    profiles.profiles.push(profile("native"));
    assert_limit(
        profiles.validate_with_limits(profile_limits).unwrap_err(),
        "profiles",
        1,
        0,
        "items",
    );

    let source_limits = PackageValidationLimits {
        max_sources: 0,
        ..PackageValidationLimits::default()
    };
    let mut sources = package();
    sources.validate_with_limits(source_limits).unwrap();
    sources.sources.push(archive_source());
    assert_limit(
        sources.validate_with_limits(source_limits).unwrap_err(),
        "sources",
        1,
        0,
        "items",
    );
}

#[test]
fn package_budget_caps_nested_and_aggregate_item_counts() {
    let per_collection = PackageValidationLimits {
        max_collection_items: 1,
        ..PackageValidationLimits::default()
    };
    package().validate_with_limits(per_collection).unwrap();

    let mut over_collection = package();
    over_collection.check_inputs = vec![
        DependencySpec::Binary("first".to_owned()),
        DependencySpec::Binary("second".to_owned()),
    ];
    let error = over_collection.validate_with_limits(per_collection).unwrap_err();
    assert_limit(error, "check_inputs", 2, 1, "items");

    let aggregate = PackageValidationLimits {
        max_total_items: 4,
        ..PackageValidationLimits::default()
    };
    package().validate_with_limits(aggregate).unwrap();

    let mut over_aggregate = package();
    over_aggregate.architectures.push("native".to_owned());
    let error = over_aggregate.validate_with_limits(aggregate).unwrap_err();
    assert_limit(error, "architectures", 5, 4, "total items");
}

#[test]
fn package_budget_caps_total_evaluated_text() {
    let aggregate = PackageValidationLimits {
        max_total_text_bytes: 50,
        ..PackageValidationLimits::default()
    };
    package().validate_with_limits(aggregate).unwrap();

    let mut over = package();
    over.architectures.push("native".to_owned());
    let error = over.validate_with_limits(aggregate).unwrap_err();
    assert_limit(error, "architectures[0]", 56, 50, "total text bytes");
}

#[test]
fn typed_dependencies_use_the_shared_relation_model() {
    let package = package();
    package.validate().unwrap();
    assert_eq!(
        package.native_build_inputs[0].dependency().unwrap().to_name(),
        "binary(cmake)"
    );
    assert_eq!(package.build_inputs[0].dependency().unwrap().to_name(), "zlib");
    assert_eq!(
        DependencySpec::Soname("libz.so.1".to_owned())
            .dependency()
            .unwrap()
            .kind,
        RelationKind::SharedLibrary
    );
}

#[test]
fn canonical_relation_errors_keep_the_typed_package_field() {
    let mut dependency = package();
    dependency.native_build_inputs = vec![DependencySpec::Binary(String::new())];
    let error = dependency.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidDependency { .. }));
    assert_eq!(error.field(), "native_build_inputs[0]");

    let mut provider = package();
    provider.outputs[0].conflicts = vec![DependencySpec::PkgConfig(String::new())];
    let error = provider.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidProvider { .. }));
    assert_eq!(error.field(), "outputs[0].conflicts[0]");

    let mut output = package();
    output.build_inputs = vec![DependencySpec::Output(OutputRef {
        package: PackageRef {
            name: "zlib".to_owned(),
        },
        output: String::new(),
    })];
    let error = output.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidDependency { .. }));
    assert_eq!(error.field(), "build_inputs[0]");
}

#[test]
fn executable_steps_bind_normalized_paths_to_capabilities() {
    let mut valid = package();
    valid.builder.phases.build = PhaseSpec::new([StepSpec::Run {
        program: ProgramSpec {
            path: "/opt/tools/bin/codegen".to_owned(),
            requirement: dependency("codegen-tools"),
        },
        args: vec!["--frozen".to_owned()],
    }]);
    valid.validate().unwrap();

    let mut relative = package();
    relative.builder.phases.build = PhaseSpec::new([StepSpec::Run {
        program: ProgramSpec {
            path: "usr/bin/tool".to_owned(),
            requirement: DependencySpec::Binary("tool".to_owned()),
        },
        args: Vec::new(),
    }]);
    let error = relative.validate().unwrap_err();
    assert!(matches!(error, PackageConversionError::InvalidProgramPath { .. }));
    assert_eq!(error.field(), "builder.phases.build.steps[0].program.path");

    let mut mismatch = package();
    mismatch.builder.phases.build = PhaseSpec::new([StepSpec::Run {
        program: ProgramSpec {
            path: "/usr/bin/other".to_owned(),
            requirement: DependencySpec::Binary("tool".to_owned()),
        },
        args: Vec::new(),
    }]);
    let error = mismatch.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::ProgramRequirementPathMismatch { .. }
    ));
    assert_eq!(error.field(), "builder.phases.build.steps[0].program.path");

    let mut unsupported = package();
    unsupported.hooks.pre_install = vec![StepSpec::Shell {
        interpreter: binary_program("bash"),
        declared_programs: vec![ProgramSpec {
            path: "/usr/bin/pkg-config".to_owned(),
            requirement: DependencySpec::PkgConfig("libexample".to_owned()),
        }],
        script: "pkg-config --exists libexample".to_owned(),
    }];
    let error = unsupported.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::UnsupportedProgramRequirement { .. }
    ));
    assert_eq!(error.field(), "hooks.pre_install[0].declared_programs[0].requirement");
}

#[test]
fn base_and_selected_profile_semantics_stay_structural() {
    let mut package = package();
    package.builder = structural_builder(
        BuilderEnvironmentSpec::CMake,
        vec![DependencySpec::Binary("cmake".to_owned())],
        PhasesSpec {
            setup: PhaseSpec::new([StepSpec::CMakeConfigure {
                flags: vec!["-DBASE=ON".to_owned()],
            }]),
            build: PhaseSpec::new([StepSpec::CMakeBuild]),
            install: PhaseSpec::new([StepSpec::CMakeInstall]),
            ..PhasesSpec::default()
        },
    );
    package.native_build_inputs = vec![dependency("base-native")];
    package.build_inputs = vec![dependency("base-build")];
    package.check_inputs = vec![dependency("base-check")];
    package.profiles.push(ProfileSpec {
        name: "emul32/x86_64".to_owned(),
        builder: structural_builder(
            BuilderEnvironmentSpec::Cargo,
            vec![DependencySpec::Binary("cargo".to_owned())],
            PhasesSpec {
                build: PhaseSpec::new([StepSpec::CargoBuild {
                    features: vec!["profile".to_owned()],
                }]),
                install: PhaseSpec::new([StepSpec::CargoInstall {
                    binaries: vec!["example".to_owned()],
                }]),
                check: PhaseSpec::new([StepSpec::CargoTest {
                    features: vec!["profile".to_owned()],
                }]),
                ..PhasesSpec::default()
            },
        ),
        hooks: HooksSpec {
            pre_build: vec![shell("prepare-profile")],
            ..HooksSpec::default()
        },
        native_build_inputs: vec![dependency("profile-native")],
        build_inputs: vec![dependency("profile-build")],
        check_inputs: vec![dependency("profile-check")],
    });

    assert_eq!(
        package.builder_for_profile(None).environment,
        [BuilderEnvironmentSpec::CMake]
    );
    assert_eq!(
        package.builder_for_profile(None).required_tools(),
        [DependencySpec::Binary("cmake".to_owned())]
    );
    assert_eq!(
        package.builder_for_profile(Some("emul32/x86_64")).environment,
        [BuilderEnvironmentSpec::Cargo]
    );
    assert_eq!(
        package.builder_for_profile(Some("emul32/x86_64")).required_tools(),
        [DependencySpec::Binary("cargo".to_owned())]
    );
    assert_eq!(
        package.phases_for_profile(Some("emul32/x86_64")).build.steps,
        [
            shell("prepare-profile"),
            StepSpec::CargoBuild {
                features: vec!["profile".to_owned()]
            }
        ]
    );
    assert_eq!(
        package.native_build_inputs_for_profile(None),
        [dependency("base-native")]
    );
    assert_eq!(
        package.build_inputs_for_profile(Some("emul32/x86_64")),
        [dependency("profile-build")]
    );
    assert_eq!(
        package.check_inputs_for_profile(Some("emul32/x86_64")),
        [dependency("profile-check")]
    );
    assert_eq!(
        package.builder_for_profile(Some("missing")).environment,
        [BuilderEnvironmentSpec::CMake]
    );
}

#[test]
fn profile_names_are_unique_normalized_target_keys() {
    for name in ["x86_64", "x86_64-v3x", "emul32/x86_64", "tier/.hidden"] {
        let mut spec = package();
        spec.profiles.push(profile(name));

        spec.validate()
            .unwrap_or_else(|error| panic!("profile name `{name}` was rejected: {error}"));
    }

    for name in [
        "",
        "/x86_64",
        "//x86_64",
        "emul32//x86_64",
        "emul32/",
        "./x86_64",
        "emul32/./x86_64",
        ".",
        "../x86_64",
        "emul32/../x86_64",
        "..",
        "emul32\\x86_64",
        "emul32\nx86_64",
    ] {
        let mut spec = package();
        spec.profiles.push(profile(name));

        let error = spec.validate().unwrap_err();
        assert!(
            matches!(
                error,
                PackageConversionError::InvalidProfileName {
                    index: 0,
                    name: ref found,
                }
                    if found == name
            ),
            "profile name `{name}` was not rejected as an invalid target key: {error}"
        );
        assert_eq!(error.field(), "profiles");
        assert!(error.to_string().starts_with("profiles[0].name:"));
    }

    let mut duplicate = package();
    duplicate.profiles = vec![profile("native"), profile("emul32/x86_64"), profile("native")];

    let error = duplicate.validate().unwrap_err();
    assert!(matches!(
        error,
        PackageConversionError::DuplicateProfileName {
            first_index: 0,
            duplicate_index: 2,
            ref name,
        } if name == "native"
    ));
    assert_eq!(error.field(), "profiles");
    assert_eq!(
        error.to_string(),
        "profiles[2].name: duplicate profile name `native`; first declared at profiles[0].name"
    );
}
