fn dependency_role_values() -> Vec<(DependencyKind, DependencySpec)> {
    vec![
        (
            DependencyKind::Package,
            DependencySpec::Package(PackageRef {
                name: "role-package".to_owned(),
            }),
        ),
        (
            DependencyKind::Output,
            DependencySpec::Output(OutputRef {
                package: PackageRef {
                    name: "role-output".to_owned(),
                },
                output: "devel".to_owned(),
            }),
        ),
        (
            DependencyKind::Binary,
            DependencySpec::Binary("role-binary".to_owned()),
        ),
        (
            DependencyKind::SystemBinary,
            DependencySpec::SystemBinary("role-system-binary".to_owned()),
        ),
        (
            DependencyKind::PkgConfig,
            DependencySpec::PkgConfig("role-pkgconfig".to_owned()),
        ),
        (
            DependencyKind::PkgConfig32,
            DependencySpec::PkgConfig32("role-pkgconfig32".to_owned()),
        ),
        (
            DependencyKind::Soname,
            DependencySpec::Soname("librole.so.1".to_owned()),
        ),
        (
            DependencyKind::CMake,
            DependencySpec::CMake("RoleConfig".to_owned()),
        ),
        (
            DependencyKind::Python,
            DependencySpec::Python("role_python".to_owned()),
        ),
        (
            DependencyKind::Interpreter,
            DependencySpec::Interpreter("/usr/lib/ld-role.so.1(x86_64)".to_owned()),
        ),
    ]
}

fn assert_dependency_role_error(
    error: PackageConversionError,
    expected_field: &str,
    expected_role: DependencyRole,
    expected_kind: DependencyKind,
) {
    let PackageConversionError::UnsupportedDependencyRole {
        field,
        role,
        kind,
    } = &error
    else {
        panic!("expected a dependency role error, found: {error}");
    };
    assert_eq!(field, expected_field);
    assert_eq!(*role, expected_role);
    assert_eq!(*kind, expected_kind);
    assert_eq!(error.field(), expected_field);
    assert!(error.to_string().contains(&format!("`{expected_role}` role")));
    assert!(error.to_string().contains(&format!("kind `{expected_kind}`")));
}

#[test]
fn ordinary_dependency_roles_accept_their_complete_allowed_kind_sets() {
    let dependencies = dependency_role_values();
    let all = || {
        dependencies
            .iter()
            .map(|(_, dependency)| dependency.clone())
            .collect::<Vec<_>>()
    };
    let mut spec = package();
    spec.builder.required_tools = dependencies[..4]
        .iter()
        .map(|(_, dependency)| dependency.clone())
        .collect();
    spec.native_build_inputs = all();
    spec.build_inputs = all();
    spec.check_inputs = all();
    spec.outputs[0].runtime_inputs = dependencies
        .iter()
        .filter(|(kind, _)| {
            !matches!(
                kind,
                DependencyKind::CMake | DependencyKind::PkgConfig | DependencyKind::PkgConfig32
            )
        })
        .map(|(_, dependency)| dependency.clone())
        .collect();
    spec.outputs[0].conflicts = all();

    spec.validate().unwrap();
}

#[test]
fn ordinary_builder_tools_reject_every_non_executable_capability_kind() {
    for (kind, dependency) in dependency_role_values().into_iter().skip(4) {
        let mut spec = package();
        spec.builder.required_tools = vec![dependency];

        assert_dependency_role_error(
            spec.validate().unwrap_err(),
            "builder.required_tools[0]",
            DependencyRole::BuilderTool,
            kind,
        );
    }
}

#[test]
fn ordinary_runtime_dependencies_reject_development_metadata_kinds() {
    for (kind, dependency) in dependency_role_values().into_iter().filter(|(kind, _)| {
        matches!(
            kind,
            DependencyKind::CMake | DependencyKind::PkgConfig | DependencyKind::PkgConfig32
        )
    }) {
        let mut spec = package();
        spec.outputs[0].runtime_inputs = vec![dependency];

        assert_dependency_role_error(
            spec.validate().unwrap_err(),
            "outputs[0].runtime_inputs[0]",
            DependencyRole::Runtime,
            kind,
        );
    }
}

#[test]
fn selected_profile_roles_accept_executable_tools_and_broad_build_capabilities() {
    let dependencies = dependency_role_values();
    let all = || {
        dependencies
            .iter()
            .map(|(_, dependency)| dependency.clone())
            .collect::<Vec<_>>()
    };
    let required_tools = dependencies[..4]
        .iter()
        .map(|(_, dependency)| dependency.clone())
        .collect::<Vec<_>>();
    let mut spec = package();
    spec.profiles.push(ProfileSpec {
        name: "emul32/x86_64".to_owned(),
        builder: BuilderSpec {
            required_tools: required_tools.clone(),
            ..BuilderSpec::default()
        },
        hooks: HooksSpec::default(),
        native_build_inputs: all(),
        build_inputs: all(),
        check_inputs: all(),
    });

    spec.validate().unwrap();
    assert_eq!(
        spec.builder_for_profile(Some("emul32/x86_64")).required_tools(),
        required_tools
    );
    assert_eq!(
        spec.native_build_inputs_for_profile(Some("emul32/x86_64")),
        all()
    );
    assert_eq!(spec.build_inputs_for_profile(Some("emul32/x86_64")), all());
    assert_eq!(spec.check_inputs_for_profile(Some("emul32/x86_64")), all());
}

#[test]
fn selected_profile_builder_tools_reject_every_non_executable_capability_kind() {
    for (kind, dependency) in dependency_role_values().into_iter().skip(4) {
        let mut spec = package();
        spec.profiles.push(ProfileSpec {
            name: "emul32/x86_64".to_owned(),
            builder: BuilderSpec {
                required_tools: vec![dependency],
                ..BuilderSpec::default()
            },
            hooks: HooksSpec::default(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        });

        assert_dependency_role_error(
            spec.validate().unwrap_err(),
            "profiles[0].builder.required_tools[0]",
            DependencyRole::BuilderTool,
            kind,
        );
    }
}
