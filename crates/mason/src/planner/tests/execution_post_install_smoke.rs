const POST_INSTALL_SMOKE_SCRIPT: &str =
    r#""${CAST_INSTALL_ROOT}${CAST_BINDIR}/staged-probe" --self-test"#;
const POST_INSTALL_SMOKE_SUCCESS: &str = "staged-probe: staged install self-test passed";
const POST_INSTALL_SMOKE_PATH_REJECTION: &str = "staged-probe: staged executable path mismatch";

fn validate_post_install_smoke_source_contract(cmake_lists: &str, source: &str) -> Result<(), String> {
    for fragment in [
        "add_executable(staged-probe staged-probe.c)",
        "install(TARGETS staged-probe RUNTIME DESTINATION \"${CMAKE_INSTALL_BINDIR}\")",
        "install(DIRECTORY DESTINATION \"${CMAKE_INSTALL_DATADIR}/cast\")",
        "add_test(NAME staged-probe-build-tree COMMAND staged-probe)",
        "PASS_REGULAR_EXPRESSION \"staged-probe: build-tree check passed\"",
    ] {
        if cmake_lists.matches(fragment).count() != 1 {
            return Err(format!("CMake contract must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        "const char *install_root = getenv(\"CAST_INSTALL_ROOT\");",
        "const char *bindir = getenv(\"CAST_BINDIR\");",
        "const char *datadir = getenv(\"CAST_DATADIR\");",
        "\"%s%s/staged-probe\"",
        "\"%s%s/cast/post-install-smoke-test.proof\"",
        "strcmp(invoked_path, expected_path) != 0",
        "strcmp(argv[1], \"--self-test\") == 0",
        "if (write_proof(proof_path) != 0)",
        "fchmod(fileno(proof), 0644)",
        "fwrite(proof_bytes, 1, sizeof(proof_bytes) - 1, proof)",
        POST_INSTALL_SMOKE_SUCCESS,
        POST_INSTALL_SMOKE_PATH_REJECTION,
    ] {
        if source.matches(fragment).count() != 1 {
            return Err(format!("staged probe must contain exactly one `{fragment}`"));
        }
    }
    Ok(())
}

fn assert_post_install_smoke_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let install = &package.builder.phases.install;
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.hooks.post_install.as_slice()
    else {
        panic!("post-install-smoke-test: expected exactly one structural post-install shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert!(declared_programs.is_empty());
    assert_eq!(script, POST_INSTALL_SMOKE_SCRIPT);
    assert!(
        package.hooks.pre_install.is_empty(),
        "post-install-smoke-test: staged check must remain a post-install hook"
    );
    let [StepSpec::CMakeInstall] = install.steps.as_slice() else {
        panic!("post-install-smoke-test: structural CMake install step drifted");
    };

    let cmake_lists = fs::read_to_string(source_tree.join("CMakeLists.txt")).unwrap();
    let source = fs::read_to_string(source_tree.join("staged-probe.c")).unwrap();
    validate_post_install_smoke_source_contract(&cmake_lists, &source).unwrap();

    let missing_staged_path = source.replacen("strcmp(invoked_path, expected_path) != 0", "0", 1);
    assert!(
        validate_post_install_smoke_source_contract(&cmake_lists, &missing_staged_path).is_err(),
        "removing the staged executable identity check must fail closed"
    );
    let missing_proof_write = source.replacen("if (write_proof(proof_path) != 0)", "if (0)", 1);
    assert!(
        validate_post_install_smoke_source_contract(&cmake_lists, &missing_proof_write).is_err(),
        "removing the staged proof write must fail closed"
    );
    let build_tree_substitute = cmake_lists.replacen(
        "install(TARGETS staged-probe RUNTIME DESTINATION \"${CMAKE_INSTALL_BINDIR}\")",
        "",
        1,
    );
    assert!(
        validate_post_install_smoke_source_contract(&build_tree_substitute, &source).is_err(),
        "removing the staged executable install must fail closed"
    );
    let missing_proof_directory = cmake_lists.replacen(
        "install(DIRECTORY DESTINATION \"${CMAKE_INSTALL_DATADIR}/cast\")",
        "",
        1,
    );
    assert!(
        validate_post_install_smoke_source_contract(&missing_proof_directory, &source).is_err(),
        "removing the install-created proof directory must fail closed"
    );
}

fn assert_post_install_smoke_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    for name in ["CMakeLists.txt", "staged-probe.c"] {
        assert_eq!(
            fs::read(published.join(name)).unwrap(),
            fs::read(source_tree.join(name)).unwrap(),
            "post-install-smoke-test: locked archive contains stale `{name}` bytes"
        );
    }
}
