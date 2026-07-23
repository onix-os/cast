const CMAKE_ZLIB_MARKER: &str = "cast cmake fixture: zlib round-trip verified";

fn validate_cmake_zlib_dependency_contract(package: &PackageSpec) -> Result<(), String> {
    if package.build_inputs != [DependencySpec::CMake("zlib".to_owned())] {
        return Err("cmake must declare exactly build_inputs = [b.dep.cmake \"zlib\"]".to_owned());
    }
    if !package.native_build_inputs.is_empty() || !package.check_inputs.is_empty() {
        return Err("zlib must remain a build input, not a native or check input".to_owned());
    }
    Ok(())
}

fn validate_cmake_zlib_source_contract(cmake_lists: &str, source: &str) -> Result<(), String> {
    for fragment in [
        "find_package(ZLIB REQUIRED)",
        "target_link_libraries(cast-cmake-fixture PRIVATE ZLIB::ZLIB)",
        "add_test(NAME cast-cmake-fixture-runs COMMAND cast-cmake-fixture)",
        "PASS_REGULAR_EXPRESSION \"cast cmake fixture: zlib round-trip verified\"",
    ] {
        if cmake_lists.matches(fragment).count() != 1 {
            return Err(format!("CMake contract must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        "#include <zlib.h>",
        "compress2(",
        "uncompress(",
        "memcmp(",
        "Z_BEST_COMPRESSION",
        CMAKE_ZLIB_MARKER,
    ] {
        if source.matches(fragment).count() != 1 {
            return Err(format!("zlib round-trip source must contain exactly one `{fragment}`"));
        }
    }
    Ok(())
}

fn assert_cmake_zlib_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let cmake_lists = fs::read_to_string(source_tree.join("CMakeLists.txt")).unwrap();
    let source = fs::read_to_string(source_tree.join("hello.c")).unwrap();
    validate_cmake_zlib_dependency_contract(package).unwrap();
    validate_cmake_zlib_source_contract(&cmake_lists, &source).unwrap();

    let mut missing = package.clone();
    missing.build_inputs.clear();
    assert!(
        validate_cmake_zlib_dependency_contract(&missing).is_err(),
        "removing the zlib dependency must fail closed"
    );

    let mut wrong_provider = package.clone();
    wrong_provider.build_inputs = vec![DependencySpec::CMake("zlib-ng".to_owned())];
    assert!(
        validate_cmake_zlib_dependency_contract(&wrong_provider).is_err(),
        "substituting a different CMake provider must fail closed"
    );

    let mut wrong_origin = package.clone();
    wrong_origin.native_build_inputs = wrong_origin.build_inputs.clone();
    wrong_origin.build_inputs.clear();
    assert!(
        validate_cmake_zlib_dependency_contract(&wrong_origin).is_err(),
        "moving zlib to the native-build origin must fail closed"
    );

    let missing_link = cmake_lists.replacen(
        "target_link_libraries(cast-cmake-fixture PRIVATE ZLIB::ZLIB)",
        "",
        1,
    );
    assert!(
        validate_cmake_zlib_source_contract(&missing_link, &source).is_err(),
        "removing the ZLIB target link must fail closed"
    );

    for call in ["compress2(", "uncompress("] {
        let missing_call = source.replacen(call, "removed_zlib_call(", 1);
        assert!(
            validate_cmake_zlib_source_contract(&cmake_lists, &missing_call).is_err(),
            "removing `{call}` must fail closed"
        );
    }
}

fn assert_cmake_zlib_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    for name in ["CMakeLists.txt", "hello.c"] {
        assert_eq!(
            fs::read(published.join(name)).unwrap(),
            fs::read(source_tree.join(name)).unwrap(),
            "cmake: locked archive contains stale `{name}` bytes"
        );
    }
}
