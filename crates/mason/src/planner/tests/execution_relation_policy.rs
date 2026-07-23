const RELATION_POLICY_INSTALL_SCRIPT: &str = r#"
printf '%s\n' \
    'schema = 1' \
    'native-build = "sysbinary(ldconfig)"' \
    'build = "pkgconfig32(zlib)"' \
    'runtime-interpreter = "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))"' \
    'runtime-library = "soname(libz.so.1(x86_64))"' \
    > relation-policy.conf
install -Dm644 relation-policy.conf \
    "${CAST_INSTALL_ROOT}${CAST_DATADIR}/cast/relation-policy.conf"
"#;

const RELATION_POLICY_CONTENT: &[u8] = b"schema = 1\nnative-build = \"sysbinary(ldconfig)\"\nbuild = \"pkgconfig32(zlib)\"\nruntime-interpreter = \"interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))\"\nruntime-library = \"soname(libz.so.1(x86_64))\"\n";

fn assert_relation_policy_fixture_contract(declaration: &PackageSpec) {
    assert!(declaration.sources.is_empty(), "relation-policy: authored package must remain source-less");
    assert!(!declaration.options.networking, "relation-policy: execution networking must remain disabled");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(bash)", "binary(install)"]
    );
    assert_eq!(dependency_names(&declaration.native_build_inputs), ["sysbinary(ldconfig)"]);
    assert_eq!(dependency_names(&declaration.build_inputs), ["pkgconfig32(zlib)"]);
    assert!(declaration.check_inputs.is_empty());

    for (phase, steps) in [
        ("setup", &declaration.builder.phases.setup.steps),
        ("build", &declaration.builder.phases.build.steps),
        ("check", &declaration.builder.phases.check.steps),
        ("workload", &declaration.builder.phases.workload.steps),
    ] {
        assert!(steps.is_empty(), "relation-policy: {phase} must remain empty");
    }
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("relation-policy: expected one explicit install shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );
    assert_eq!(script, RELATION_POLICY_INSTALL_SCRIPT);

    let [output] = declaration.outputs.as_slice() else {
        panic!("relation-policy: declaration must have exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.summary.as_deref(), Some("Typed dependency-role policy fixture"));
    assert_eq!(
        output.description.as_deref(),
        Some("Pinned build and runtime capabilities with exact provider identities.")
    );
    assert_eq!(
        dependency_names(&output.runtime_inputs),
        [
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))",
            "soname(libz.so.1(x86_64))",
        ]
    );
    assert!(output.conflicts.is_empty());
}

#[test]
fn relation_policy_source_less_declaration_is_exact_and_role_validated() {
    let recipe = execution_fixture_package_directory("relation-policy").join("stone.glu");
    let loaded = crate::Recipe::load_authored(&recipe)
        .unwrap_or_else(|error| panic!("relation-policy: evaluate authored fixture: {error:#}"));
    assert_relation_policy_fixture_contract(&loaded.declaration);
    assert!(
        !recipe.with_file_name(SOURCE_LOCK_FILE_NAME).exists(),
        "relation-policy: source-less declaration must not gain a source lock"
    );
}
