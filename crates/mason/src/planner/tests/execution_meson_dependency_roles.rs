const MESON_ZLIB_MARKER: &str = "cast meson fixture: pkgconfig zlib round-trip verified";
const MESON_FILE_MIME: &str = "application/x-pie-executable\\n";
const MESON_MAIN_EXECUTABLE_BLOCK: &str = r#"fixture = executable(
  'cast-meson-fixture',
  'hello.c',
  dependencies: zlib,
  install: true,
)"#;
const MESON_CHECKER_EXECUTABLE_BLOCK: &str = r#"check_file_input = executable(
  'cast-meson-check-file-input',
  'check-file-input.c',
  install: false,
)"#;
const MESON_CHECKER_TEST_BLOCK: &str = r#"test(
  'cast-meson-declared-file-check-input',
  check_file_input,
  args: [fixture],
)"#;

fn validate_meson_dependency_role_contract(package: &PackageSpec) -> Result<(), String> {
    if package.build_inputs != [DependencySpec::PkgConfig("zlib".to_owned())] {
        return Err("meson must declare exactly build_inputs = [b.dep.pkgconfig \"zlib\"]".to_owned());
    }
    if package.check_inputs != [DependencySpec::Binary("file".to_owned())] {
        return Err("meson must declare exactly check_inputs = [b.dep.binary \"file\"]".to_owned());
    }
    if !package.native_build_inputs.is_empty() {
        return Err("meson target and check inputs must not leak into native_build_inputs".to_owned());
    }
    Ok(())
}

fn validate_meson_dependency_role_sources(
    meson: &str,
    source: &str,
    checker: &str,
) -> Result<(), String> {
    for fragment in [
        "dependency('zlib', method: 'pkg-config', required: true)",
        MESON_MAIN_EXECUTABLE_BLOCK,
        MESON_CHECKER_EXECUTABLE_BLOCK,
        "test('cast-meson-zlib-round-trip', fixture)",
        MESON_CHECKER_TEST_BLOCK,
    ] {
        if meson.matches(fragment).count() != 1 {
            return Err(format!("Meson dependency-role contract must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        "#include <zlib.h>",
        "compress2(",
        "uncompress(",
        "memcmp(",
        "Z_BEST_COMPRESSION",
        MESON_ZLIB_MARKER,
    ] {
        if source.matches(fragment).count() != 1 {
            return Err(format!("Meson zlib source must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        MESON_FILE_MIME,
        "execlp(\"file\", \"file\", \"--brief\", \"--mime-type\", \"--\", argv[1], (char *)NULL);",
        "if (wait_for_success(child) != 0)",
        "strcmp(output, expected)",
    ] {
        if checker.matches(fragment).count() != 1 {
            return Err(format!("Meson check-only helper must contain exactly one `{fragment}`"));
        }
    }
    if source.contains("execlp(") || source.contains("application/x-pie-executable") {
        return Err("the installed program must not consume the check-only file capability".to_owned());
    }
    Ok(())
}

fn assert_meson_dependency_role_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let meson = fs::read_to_string(source_tree.join("meson.build")).unwrap();
    let source = fs::read_to_string(source_tree.join("hello.c")).unwrap();
    let checker = fs::read_to_string(source_tree.join("check-file-input.c")).unwrap();
    validate_meson_dependency_role_contract(package).unwrap();
    validate_meson_dependency_role_sources(&meson, &source, &checker).unwrap();

    let mut missing_build = package.clone();
    missing_build.build_inputs.clear();
    assert!(
        validate_meson_dependency_role_contract(&missing_build).is_err(),
        "removing the pkg-config zlib build input must fail closed"
    );

    let mut wrong_build_provider = package.clone();
    wrong_build_provider.build_inputs = vec![DependencySpec::PkgConfig("zlib-ng".to_owned())];
    assert!(
        validate_meson_dependency_role_contract(&wrong_build_provider).is_err(),
        "substituting a different pkg-config provider must fail closed"
    );

    let mut wrong_build_origin = package.clone();
    wrong_build_origin.native_build_inputs = wrong_build_origin.build_inputs.clone();
    wrong_build_origin.build_inputs.clear();
    assert!(
        validate_meson_dependency_role_contract(&wrong_build_origin).is_err(),
        "moving zlib to the native-build origin must fail closed"
    );

    let mut missing_check = package.clone();
    missing_check.check_inputs.clear();
    assert!(
        validate_meson_dependency_role_contract(&missing_check).is_err(),
        "removing the file check input must fail closed"
    );

    let mut wrong_check_provider = package.clone();
    wrong_check_provider.check_inputs = vec![DependencySpec::Binary("readelf".to_owned())];
    assert!(
        validate_meson_dependency_role_contract(&wrong_check_provider).is_err(),
        "substituting a different check provider must fail closed"
    );

    let mut wrong_check_origin = package.clone();
    wrong_check_origin.build_inputs.extend(wrong_check_origin.check_inputs.clone());
    wrong_check_origin.check_inputs.clear();
    assert!(
        validate_meson_dependency_role_contract(&wrong_check_origin).is_err(),
        "moving file into the target build inputs must fail closed"
    );

    let missing_link = meson.replacen("dependencies: zlib", "", 1);
    assert!(
        validate_meson_dependency_role_sources(&missing_link, &source, &checker).is_err(),
        "removing the Meson zlib link must fail closed"
    );
    for call in ["compress2(", "uncompress("] {
        let missing_call = source.replacen(call, "removed_zlib_call(", 1);
        assert!(
            validate_meson_dependency_role_sources(&meson, &missing_call, &checker).is_err(),
            "removing `{call}` must fail closed"
        );
    }

    let missing_check_execution = checker.replacen("execlp(\"file\"", "removed_file_exec(\"file\"", 1);
    assert!(
        validate_meson_dependency_role_sources(&meson, &source, &missing_check_execution).is_err(),
        "removing the causal file execution must fail closed"
    );
    let installed_checker = meson.replacen("install: false", "install: true", 1);
    assert!(
        validate_meson_dependency_role_sources(&installed_checker, &source, &checker).is_err(),
        "installing the check-only helper must fail closed"
    );

    let missing_checker_test = meson.replacen(MESON_CHECKER_TEST_BLOCK, "", 1);
    assert!(
        validate_meson_dependency_role_sources(&missing_checker_test, &source, &checker).is_err(),
        "removing the checker test registration must fail closed"
    );
    let replaced_checker_test = meson.replacen(
        MESON_CHECKER_TEST_BLOCK,
        r#"test(
  'cast-meson-declared-file-check-input',
  fixture,
  args: [fixture],
)"#,
        1,
    );
    assert!(
        validate_meson_dependency_role_sources(&replaced_checker_test, &source, &checker).is_err(),
        "replacing the checker executable in its test registration must fail closed"
    );

    let swapped_install_roles = meson
        .replacen(
            MESON_MAIN_EXECUTABLE_BLOCK,
            r#"fixture = executable(
  'cast-meson-fixture',
  'hello.c',
  dependencies: zlib,
  install: false,
)"#,
            1,
        )
        .replacen(
            MESON_CHECKER_EXECUTABLE_BLOCK,
            r#"check_file_input = executable(
  'cast-meson-check-file-input',
  'check-file-input.c',
  install: true,
)"#,
            1,
        );
    assert!(
        validate_meson_dependency_role_sources(&swapped_install_roles, &source, &checker).is_err(),
        "swapping the installed and check-only target roles must fail closed"
    );
}

fn assert_meson_dependency_role_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    let expected = ["check-file-input.c", "hello.c", "meson.build"];
    let mut actual = fs::read_dir(published)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(actual, expected, "meson: locked archive member set drifted");
    for name in expected {
        assert_eq!(
            fs::read(published.join(name)).unwrap(),
            fs::read(source_tree.join(name)).unwrap(),
            "meson: locked archive contains stale `{name}` bytes"
        );
    }
}
