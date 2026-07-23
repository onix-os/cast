const HEADER_ONLY_INSTALL_SCRIPT: &str = r#"install -Dm644 include/vector.h "${CAST_INSTALL_ROOT}${CAST_PREFIX}/include/cast-header-only/vector.h"
install -Dm644 cast-header-only.pc "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/pkgconfig/cast-header-only.pc"
install -Dm644 LICENSE "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-header-only-library-fixture/LICENSE""#;
const HEADER_ONLY_CHECK_SCRIPT: &str = r#"test ! -e include/cast-header-only/vector.h
test -f "${CAST_INSTALL_ROOT}${CAST_PREFIX}/include/cast-header-only/vector.h"
cc -std=c11 -Wall -Wextra -Werror -pedantic-errors -nostdinc -I"${CAST_INSTALL_ROOT}${CAST_PREFIX}/include" -fsyntax-only tests/consumer.c"#;

fn validate_header_only_source_contract(header: &str, consumer: &str, pkgconfig: &str) -> Result<(), String> {
    for fragment in [
        "#define CAST_HEADER_ONLY_VECTOR_MAGIC 0x48445231",
        "#define CAST_HEADER_ONLY_VECTOR_ADD(left, right) ((left) + (right))",
    ] {
        if header.matches(fragment).count() != 1 {
            return Err(format!("header-only source must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        "#include <cast-header-only/vector.h>",
        "CAST_HEADER_ONLY_VECTOR_MAGIC == 0x48445231",
        "CAST_HEADER_ONLY_VECTOR_ADD(20, 22) == 42",
    ] {
        if consumer.matches(fragment).count() != 1 {
            return Err(format!("header-only consumer must contain exactly one `{fragment}`"));
        }
    }
    for fragment in [
        "Name: cast-header-only",
        "Version: 1.0.0",
        "Cflags: -I${includedir}",
    ] {
        if pkgconfig.matches(fragment).count() != 1 {
            return Err(format!("header-only pkg-config file must contain exactly one `{fragment}`"));
        }
    }
    Ok(())
}

fn validate_header_only_scripts(install: &str, check: &str) -> Result<(), String> {
    if install != HEADER_ONLY_INSTALL_SCRIPT {
        return Err("header-only install script drifted".to_owned());
    }
    if check != HEADER_ONLY_CHECK_SCRIPT {
        return Err("header-only staged-consumer check drifted".to_owned());
    }
    for required in [
        "test ! -e include/cast-header-only/vector.h",
        "${CAST_INSTALL_ROOT}${CAST_PREFIX}/include/cast-header-only/vector.h",
        "-nostdinc",
        "-I\"${CAST_INSTALL_ROOT}${CAST_PREFIX}/include\"",
        "-fsyntax-only tests/consumer.c",
    ] {
        if check.matches(required).count() != 1 {
            return Err(format!("header-only staged check must contain exactly one `{required}`"));
        }
    }
    Ok(())
}

fn assert_header_only_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    assert_eq!(
        dependency_names(&package.builder.required_tools),
        ["binary(cc)", "binary(install)", "binary(dash)"],
        "header-only-library: provider closure drift"
    );
    for phase in [&package.builder.phases.setup, &package.builder.phases.build] {
        assert!(phase.steps.is_empty());
    }

    let [StepSpec::Shell {
        interpreter: install_interpreter,
        declared_programs: install_programs,
        script: install,
    }] = package.builder.phases.install.steps.as_slice()
    else {
        panic!("header-only-library: install must contain one structural shell step");
    };
    assert_eq!(install_interpreter.path, "/usr/bin/dash");
    assert_eq!(
        install_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );

    let [StepSpec::Shell {
        interpreter: check_interpreter,
        declared_programs: check_programs,
        script: check,
    }] = package.builder.phases.check.steps.as_slice()
    else {
        panic!("header-only-library: check must contain one structural shell step");
    };
    assert_eq!(check_interpreter.path, "/usr/bin/dash");
    assert_eq!(
        check_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/cc"]
    );
    validate_header_only_scripts(install, check).unwrap();

    let header = fs::read_to_string(source_tree.join("include/vector.h")).unwrap();
    let consumer = fs::read_to_string(source_tree.join("tests/consumer.c")).unwrap();
    let pkgconfig = fs::read_to_string(source_tree.join("cast-header-only.pc")).unwrap();
    validate_header_only_source_contract(&header, &consumer, &pkgconfig).unwrap();
    assert!(
        !source_tree.join("include/cast-header-only/vector.h").exists(),
        "header-only-library: the build tree must not contain the staged include path"
    );

    let without_staged_root = check.replacen("${CAST_INSTALL_ROOT}", "", 1);
    assert!(validate_header_only_scripts(install, &without_staged_root).is_err());
    let without_host_isolation = check.replacen("-nostdinc ", "", 1);
    assert!(validate_header_only_scripts(install, &without_host_isolation).is_err());
    let without_identity = header.replacen("#define CAST_HEADER_ONLY_VECTOR_MAGIC 0x48445231", "", 1);
    assert!(validate_header_only_source_contract(&without_identity, &consumer, &pkgconfig).is_err());
}

fn assert_header_only_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    for relative in [
        "LICENSE",
        "cast-header-only.pc",
        "include/vector.h",
        "tests/consumer.c",
    ] {
        assert_eq!(
            fs::read(published.join(relative)).unwrap(),
            fs::read(source_tree.join(relative)).unwrap(),
            "header-only-library: locked archive contains stale `{relative}` bytes"
        );
    }
    assert!(!published.join("include/cast-header-only/vector.h").exists());
}
