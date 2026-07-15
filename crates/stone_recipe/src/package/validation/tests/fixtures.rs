fn dependency(name: &str) -> DependencySpec {
    DependencySpec::Package(PackageRef { name: name.to_owned() })
}

fn binary_program(name: &str) -> ProgramSpec {
    ProgramSpec {
        path: format!("/usr/bin/{name}"),
        requirement: DependencySpec::Binary(name.to_owned()),
    }
}

fn shell(script: &str) -> StepSpec {
    StepSpec::Shell {
        interpreter: binary_program("bash"),
        declared_programs: Vec::new(),
        script: script.to_owned(),
    }
}

fn structural_builder(
    environment: BuilderEnvironmentSpec,
    required_tools: Vec<DependencySpec>,
    phases: PhasesSpec,
) -> BuilderSpec {
    BuilderSpec {
        required_tools,
        environment: vec![environment],
        phases,
        supported_hooks: SupportedHooksSpec::all(),
    }
}

fn profile(name: &str) -> ProfileSpec {
    ProfileSpec {
        name: name.to_owned(),
        builder: BuilderSpec::default(),
        hooks: HooksSpec::default(),
        native_build_inputs: Vec::new(),
        build_inputs: Vec::new(),
        check_inputs: Vec::new(),
    }
}

fn archive_source() -> UpstreamSpec {
    UpstreamSpec::Archive {
        url: "https://example.com/source.tar.xz".to_owned(),
        hash: "a".repeat(64),
        rename: None,
        strip_dirs: None,
        unpack: true,
        unpack_dir: None,
    }
}

fn git_source() -> UpstreamSpec {
    UpstreamSpec::Git {
        url: "https://example.com/source.git".to_owned(),
        git_ref: "main".to_owned(),
        clone_dir: None,
    }
}

fn package() -> PackageSpec {
    PackageSpec {
        meta: MetaSpec {
            pname: "example".to_owned(),
            version: "1.0.0".to_owned(),
            release: 1,
            homepage: "https://example.com".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        },
        builder: BuilderSpec::default(),
        hooks: HooksSpec::default(),
        native_build_inputs: vec![DependencySpec::Binary("cmake".to_owned())],
        build_inputs: vec![dependency("zlib")],
        check_inputs: Vec::new(),
        outputs: vec![OutputSpec {
            name: "out".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            runtime_exclude: Vec::new(),
            paths: Vec::new(),
            conflicts: Vec::new(),
        }],
        options: OptionsSpec::default(),
        profiles: Vec::new(),
        sources: Vec::new(),
        architectures: Vec::new(),
        tuning: Vec::new(),
        emul32: false,
        mold: false,
    }
}

fn assert_limit(
    error: PackageConversionError,
    expected_field: &str,
    expected_actual: usize,
    expected_limit: usize,
    expected_unit: &'static str,
) {
    let PackageConversionError::LimitExceeded {
        field,
        actual,
        limit,
        unit,
    } = &error
    else {
        panic!("expected a package resource limit, found: {error}");
    };
    assert_eq!(field, expected_field);
    assert_eq!(*actual, expected_actual);
    assert_eq!(*limit, expected_limit);
    assert_eq!(*unit, expected_unit);
    assert_eq!(error.field(), expected_field);
}
