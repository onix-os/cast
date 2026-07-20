const AUTOTOOLS_REGENERATION_MARKER: &str =
    "cast autotools fixture: autoreconf build=%s host=%s";
const AUTOTOOLS_CANONICAL_CONFIGURE_AC: &str = r#"AC_INIT([cast-autotools-fixture], [1.0.0], [fixtures@invalid.example])
AC_CONFIG_SRCDIR([hello.c])
AC_CONFIG_HEADERS([config.h])
AM_INIT_AUTOMAKE([foreign])
AC_PROG_CC

AS_IF([test -z "$build_alias"], [AC_MSG_ERROR([--build is required])])
AS_IF([test -z "$host_alias"], [AC_MSG_ERROR([--host is required])])
AC_DEFINE_UNQUOTED(
    [CAST_AUTOTOOLS_BUILD_ALIAS],
    ["$build_alias"],
    [Build platform supplied by the frozen repository policy.]
)
AC_DEFINE_UNQUOTED(
    [CAST_AUTOTOOLS_HOST_ALIAS],
    ["$host_alias"],
    [Host platform supplied by the frozen repository policy.]
)

AC_CONFIG_FILES([Makefile])
AC_OUTPUT
"#;
const AUTOTOOLS_CANONICAL_MAKEFILE_AM: &str = r#"AUTOMAKE_OPTIONS = foreign

bin_PROGRAMS = cast-autotools-fixture
cast_autotools_fixture_SOURCES = hello.c

TESTS = cast-autotools-fixture
"#;
const AUTOTOOLS_PRINTF_ARGUMENT_BLOCK: &str = r#"    return printf(
               "cast autotools fixture: autoreconf build=%s host=%s\n",
               CAST_AUTOTOOLS_BUILD_ALIAS,
               CAST_AUTOTOOLS_HOST_ALIAS
           ) < 0;"#;
const AUTOTOOLS_CANONICAL_C_SOURCE: &str = r#"
#include "config.h"

#include <stdio.h>

int main(void)
{
    return printf(
               "cast autotools fixture: autoreconf build=%s host=%s\n",
               CAST_AUTOTOOLS_BUILD_ALIAS,
               CAST_AUTOTOOLS_HOST_ALIAS
           ) < 0;
}
"#;

fn validate_autotools_regeneration_dependency_contract(package: &PackageSpec) -> Result<(), String> {
    if package.native_build_inputs != [DependencySpec::Binary("autoreconf".to_owned())] {
        return Err("autotools must declare exactly native_build_inputs = [b.dep.binary \"autoreconf\"]".to_owned());
    }
    if !package.build_inputs.is_empty() || !package.check_inputs.is_empty() {
        return Err("the native autoreconf tool must not leak into target or check inputs".to_owned());
    }
    if package.builder.required_tools
        != [
            DependencySpec::Binary("autoconf".to_owned()),
            DependencySpec::Binary("automake".to_owned()),
            DependencySpec::Binary("awk".to_owned()),
            DependencySpec::Binary("grep".to_owned()),
            DependencySpec::Binary("install".to_owned()),
            DependencySpec::Binary("sed".to_owned()),
        ]
    {
        return Err("the structural Autotools builder tool contract drifted".to_owned());
    }
    let [StepSpec::Run { program, args }] = package.hooks.pre_setup.as_slice() else {
        return Err("autotools must have exactly one typed pre-setup regeneration step".to_owned());
    };
    if program.path != "/usr/bin/autoreconf"
        || program.requirement != DependencySpec::Binary("autoreconf".to_owned())
        || args.as_slice() != ["-fi"]
    {
        return Err("autotools pre-setup must execute the exact binary(autoreconf) -fi binding".to_owned());
    }
    if [
        &package.hooks.post_setup,
        &package.hooks.pre_build,
        &package.hooks.post_build,
        &package.hooks.pre_check,
        &package.hooks.post_check,
        &package.hooks.pre_install,
        &package.hooks.post_install,
        &package.hooks.pre_workload,
        &package.hooks.post_workload,
    ]
    .into_iter()
    .any(|hooks| !hooks.is_empty())
    {
        return Err("autotools regeneration must remain the only authored hook".to_owned());
    }
    if !matches!(
        package.builder.phases.setup.steps.as_slice(),
        [StepSpec::AutotoolsConfigure { flags }] if flags.is_empty()
    ) || !matches!(package.builder.phases.build.steps.as_slice(), [StepSpec::AutotoolsBuild])
        || !matches!(package.builder.phases.check.steps.as_slice(), [StepSpec::AutotoolsTest])
        || !matches!(package.builder.phases.install.steps.as_slice(), [StepSpec::AutotoolsInstall])
    {
        return Err("autotools regeneration must feed the complete structural configure/build/check/install graph".to_owned());
    }
    Ok(())
}

fn validate_autotools_regeneration_sources(
    configure_ac: &str,
    makefile_am: &str,
    source: &str,
) -> Result<(), String> {
    if configure_ac != AUTOTOOLS_CANONICAL_CONFIGURE_AC {
        return Err("Autoconf input must equal the exact active canonical source".to_owned());
    }
    if makefile_am != AUTOTOOLS_CANONICAL_MAKEFILE_AM {
        return Err("Automake input must equal the exact active canonical source".to_owned());
    }
    if source != AUTOTOOLS_CANONICAL_C_SOURCE
        || source.matches(AUTOTOOLS_PRINTF_ARGUMENT_BLOCK).count() != 1
        || source.matches(AUTOTOOLS_REGENERATION_MARKER).count() != 1
    {
        return Err("Autotools C input must equal the exact generated-header and printf contract".to_owned());
    }
    Ok(())
}

fn assert_autotools_regeneration_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let configure_ac = fs::read_to_string(source_tree.join("configure.ac")).unwrap();
    let makefile_am = fs::read_to_string(source_tree.join("Makefile.am")).unwrap();
    let source = fs::read_to_string(source_tree.join("hello.c")).unwrap();
    validate_autotools_regeneration_dependency_contract(package).unwrap();
    validate_autotools_regeneration_sources(&configure_ac, &makefile_am, &source).unwrap();

    let mut missing_native_input = package.clone();
    missing_native_input.native_build_inputs.clear();
    assert!(
        validate_autotools_regeneration_dependency_contract(&missing_native_input).is_err(),
        "removing the native autoreconf input must fail closed"
    );

    let mut wrong_origin = package.clone();
    wrong_origin.build_inputs = wrong_origin.native_build_inputs.clone();
    wrong_origin.native_build_inputs.clear();
    assert!(
        validate_autotools_regeneration_dependency_contract(&wrong_origin).is_err(),
        "moving autoreconf to the target input role must fail closed"
    );

    for child_tool in ["awk", "grep", "sed"] {
        let mut missing_child_tool = package.clone();
        missing_child_tool
            .builder
            .required_tools
            .retain(|dependency| dependency != &DependencySpec::Binary(child_tool.to_owned()));
        assert!(
            validate_autotools_regeneration_dependency_contract(&missing_child_tool).is_err(),
            "removing the standard Autotools child tool {child_tool} must fail closed"
        );
    }

    let mut missing_hook = package.clone();
    missing_hook.hooks.pre_setup.clear();
    assert!(
        validate_autotools_regeneration_dependency_contract(&missing_hook).is_err(),
        "removing the autoreconf hook must fail closed"
    );

    let mut wrong_program = package.clone();
    let [StepSpec::Run { program, .. }] = wrong_program.hooks.pre_setup.as_mut_slice() else {
        unreachable!("validated fixture has one typed regeneration step")
    };
    program.path = "/usr/bin/autoconf".to_owned();
    program.requirement = DependencySpec::Binary("autoconf".to_owned());
    assert!(
        validate_autotools_regeneration_dependency_contract(&wrong_program).is_err(),
        "substituting the typed autoreconf program binding must fail closed"
    );

    let reject_sources = |label: &str, changed_configure: &str, changed_makefile: &str, changed_source: &str| {
        assert!(
            validate_autotools_regeneration_sources(changed_configure, changed_makefile, changed_source).is_err(),
            "{label} must fail the exact active source contract"
        );
    };

    let dnl_header = configure_ac.replacen(
        "AC_CONFIG_HEADERS([config.h])",
        "dnl AC_CONFIG_HEADERS([config.h])",
        1,
    );
    reject_sources("a dnl-disabled generated header", &dnl_header, &makefile_am, &source);
    let dnl_compiler = configure_ac.replacen("AC_PROG_CC", "dnl AC_PROG_CC", 1);
    reject_sources("a dnl-disabled compiler probe", &dnl_compiler, &makefile_am, &source);
    let commented_output = configure_ac.replacen("AC_OUTPUT", "# AC_OUTPUT", 1);
    reject_sources(
        "a shell-commented Autoconf output directive",
        &commented_output,
        &makefile_am,
        &source,
    );
    let bypassed_alias_check = configure_ac.replacen(
        r#"AS_IF([test -z "$build_alias"], [AC_MSG_ERROR([--build is required])])"#,
        "dnl accept a missing build alias",
        1,
    );
    reject_sources(
        "a bypassed build-alias check",
        &bypassed_alias_check,
        &makefile_am,
        &source,
    );
    let duplicate_configure_rule = format!("{configure_ac}AC_OUTPUT\n");
    reject_sources(
        "an extraneous duplicate Autoconf rule",
        &duplicate_configure_rule,
        &makefile_am,
        &source,
    );

    let commented_tests = makefile_am.replacen(
        "TESTS = cast-autotools-fixture",
        "# TESTS = cast-autotools-fixture",
        1,
    );
    reject_sources("a commented test registration", &configure_ac, &commented_tests, &source);
    let replacement_tests = makefile_am.replacen(
        "TESTS = cast-autotools-fixture",
        "TESTS = /usr/bin/true",
        1,
    );
    reject_sources("a replacement test program", &configure_ac, &replacement_tests, &source);
    let replacement_install = makefile_am.replacen(
        "bin_PROGRAMS = cast-autotools-fixture",
        "noinst_PROGRAMS = cast-autotools-fixture",
        1,
    );
    reject_sources("a non-installed replacement target", &configure_ac, &replacement_install, &source);
    let custom_check_rule = format!("{makefile_am}\ncheck-local:\n\t@true\n");
    reject_sources("an extraneous custom check rule", &configure_ac, &custom_check_rule, &source);
    let duplicate_test_rule = format!("{makefile_am}TESTS = cast-autotools-fixture\n");
    reject_sources("a duplicate test rule", &configure_ac, &duplicate_test_rule, &source);

    let manual_header_bypass = source.replacen(
        "#include \"config.h\"",
        "#define CAST_AUTOTOOLS_BUILD_ALIAS \"manual\"\n#define CAST_AUTOTOOLS_HOST_ALIAS \"manual\"",
        1,
    );
    reject_sources("a manual generated-header bypass", &configure_ac, &makefile_am, &manual_header_bypass);
    let swapped_aliases = source.replacen(
        "               CAST_AUTOTOOLS_BUILD_ALIAS,\n               CAST_AUTOTOOLS_HOST_ALIAS",
        "               CAST_AUTOTOOLS_HOST_ALIAS,\n               CAST_AUTOTOOLS_BUILD_ALIAS",
        1,
    );
    reject_sources("swapped printf build/host arguments", &configure_ac, &makefile_am, &swapped_aliases);
}

fn assert_autotools_regeneration_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    let names = |directory: &Path| {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(entry.file_type().unwrap().is_file());
                entry.file_name().into_string().unwrap()
            })
            .collect::<BTreeSet<_>>()
    };
    let expected = BTreeSet::from([
        "Makefile.am".to_owned(),
        "configure.ac".to_owned(),
        "hello.c".to_owned(),
    ]);
    assert_eq!(names(source_tree), expected, "autotools: tracked source input set drifted");
    assert_eq!(names(published), expected, "autotools: locked archive input set drifted");
    for name in expected {
        assert_eq!(
            fs::read(published.join(&name)).unwrap(),
            fs::read(source_tree.join(&name)).unwrap(),
            "autotools: locked archive contains stale `{name}` bytes"
        );
    }
    for generated in ["configure", "Makefile.in", "config.h.in", "aclocal.m4", "autom4te.cache"] {
        assert!(!source_tree.join(generated).exists(), "autotools: tracked source contains generated `{generated}`");
        assert!(!published.join(generated).exists(), "autotools: locked archive contains generated `{generated}`");
    }
}
