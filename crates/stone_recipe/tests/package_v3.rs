#[path = "package_v3/adapter.rs"]
mod adapter;

use adapter::{
    PackageDeclarationError, evaluate_default_package, evaluate_package_with_inputs,
};
use declarative_config::{DeclarationEvaluationError, DeclarationInputEvaluator, Source};
use declarative_config::DiagnosticCategory;
use stone_recipe::package::{
    BuiltProgramSpec, DependencyKind, DependencyRole, DependencySpec,
    GluonPackageEvaluator, PackageConversionError, ProgramSpec, StepSpec, SupportedHooksSpec,
};

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn authored(body: &str) -> Source {
    Source::new("stone.glu", format!("let a = import! cast.authored.v1\n{body}"))
}

fn binary_program(name: &str) -> ProgramSpec {
    ProgramSpec {
        path: format!("/usr/bin/{name}"),
        requirement: DependencySpec::Binary(name.to_owned()),
    }
}

fn assert_dependency_role_conversion_error(
    error: PackageDeclarationError,
    expected_field: &str,
    expected_role: DependencyRole,
    expected_kind: DependencyKind,
) {
    let diagnostic = error.to_string();
    let DeclarationEvaluationError::Conversion(PackageConversionError::UnsupportedDependencyRole {
        field,
        role,
        kind,
    }) = error
    else {
        panic!("expected a dependency role conversion error, found: {diagnostic}");
    };
    assert_eq!(field, expected_field);
    assert_eq!(role, expected_role);
    assert_eq!(kind, expected_kind);
    assert!(diagnostic.contains(&format!("`{expected_role}` role")));
    assert!(diagnostic.contains(&format!("kind `{expected_kind}`")));
}

#[test]
fn retired_package_and_builder_abis_are_not_compatibility_aliases() {
    for module in [
        "boulder.package.v3",
        "boulder.builders.cmake.v2",
        "boulder.builders.meson.v2",
        "boulder.builders.cargo.v2",
        "boulder.builders.autotools.v2",
        "cast.package.v2",
        "cast.builders.cmake.v1",
        "cast.builders.meson.v1",
        "cast.builders.cargo.v1",
        "cast.builders.autotools.v1",
        "boulder.package.v2",
    ] {
        let error = evaluate_default_package(&Source::new("stone.glu", format!("import! {module}"))).unwrap_err();
        assert!(matches!(
            error,
            DeclarationEvaluationError::Evaluation(ref diagnostic)
                if diagnostic.category == DiagnosticCategory::Import
                    && diagnostic.message.contains(module)
        ));
    }
}

#[test]
fn frozen_package_abi_has_no_cargo_fetch_escape_hatch() {
    let error = evaluate_default_package(&authored("a.step.cargo_fetch")).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref diagnostic)
            if diagnostic.category == DiagnosticCategory::Type
                && diagnostic.message.contains("cargo_fetch")
    ));
}

#[test]
fn manifest_membership_is_explicit_not_inferred_from_package_name() {
    let source = authored(
        r#"
let root = a.output "out"
{
    outputs = a.outputs.explicit [root],
    .. {
        meta = {
            pname = "symbols-dbginfo", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
"#,
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    assert_eq!(evaluated.value.outputs.len(), 1);
    assert!(evaluated.value.outputs[0].include_in_manifest);
}

#[test]
fn external_built_and_shell_steps_preserve_distinct_program_authority() {
    let source = authored(
        r#"
let tool = a.package_ref "odd-tool"
let scripts = a.scripts {
    build = a.phase [
        a.step.run (a.program.package tool "/opt/odd/bin/tool") ["--frozen"],
        a.step.run_built (a.program.built "build/generated-tool") ["--self-test"],
        a.step.shell_with {
            interpreter = a.program.binary "dash",
            declared_programs = [a.program.package tool "/opt/odd/bin/helper"],
            script = "helper --check",
        },
        a.step.shell "echo builtin",
    ],
    .. a.empty.scripts
}
{
    builder = a.builder.shell scripts [],
    outputs = a.outputs.explicit [a.output "out"],
    .. {
        meta = {
            pname = "example", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
"#,
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    assert_eq!(
        evaluated.value.builder.phases.build.steps,
        [
            StepSpec::Run {
                program: ProgramSpec {
                    path: "/opt/odd/bin/tool".to_owned(),
                    requirement: DependencySpec::Package(stone_recipe::package::PackageRef {
                        name: "odd-tool".to_owned(),
                    }),
                },
                args: vec!["--frozen".to_owned()],
            },
            StepSpec::RunBuilt {
                program: BuiltProgramSpec {
                    path: "build/generated-tool".to_owned(),
                },
                args: vec!["--self-test".to_owned()],
            },
            StepSpec::Shell {
                interpreter: binary_program("dash"),
                declared_programs: vec![ProgramSpec {
                    path: "/opt/odd/bin/helper".to_owned(),
                    requirement: DependencySpec::Package(stone_recipe::package::PackageRef {
                        name: "odd-tool".to_owned(),
                    }),
                }],
                script: "helper --check".to_owned(),
            },
            StepSpec::Shell {
                interpreter: binary_program("bash"),
                declared_programs: Vec::new(),
                script: "echo builtin".to_owned(),
            },
        ]
    );
}

#[test]
fn built_program_paths_are_normalized_before_planning() {
    for invalid in [
        "",
        "/build/tool",
        "build/../tool",
        "./build/tool",
        "build//tool",
        r"build\tool",
    ] {
        let source = authored(&format!(
            r#"
let scripts = a.scripts {{
    check = a.phase [a.step.run_built (a.program.built {invalid:?}) []],
    .. a.empty.scripts
}}
{{
    builder = a.builder.shell scripts [],
    outputs = a.outputs.explicit [a.output "out"],
    .. {{
        meta = {{
            pname = "example", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        }},
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }}
}}
"#
        ));
        let error = evaluate_default_package(&source).unwrap_err();
        assert!(matches!(
            error,
            DeclarationEvaluationError::Conversion(PackageConversionError::InvalidText { ref field, .. })
                if field == "builder.phases.check.steps[0].program.path"
        ));
    }
}

#[test]
fn invalid_program_bindings_are_rejected_before_planning() {
    for (program, expected_field) in [
        (
            r#"{ path = "tool", requirement = a.dep.binary "tool" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
        (
            r#"{ path = "/usr/bin/other", requirement = a.dep.binary "tool" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
        (
            r#"{ path = "/usr/bin/pkg-config", requirement = a.dep.pkgconfig "example" }"#,
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            r#"{ path = "/usr/bin/nested/tool", requirement = a.dep.binary "nested/tool" }"#,
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            r#"{ path = "/usr/bin/tool", requirement = a.dep.package "tool-package" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
    ] {
        let source = authored(&format!(
            r#"
let scripts = a.scripts {{
    build = a.phase [a.step.run {program} []],
    .. a.empty.scripts
}}
{{
    builder = a.builder.shell scripts [],
    outputs = a.outputs.explicit [a.output "out"],
    .. {{
        meta = {{
            pname = "example", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        }},
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }}
}}
"#
        ));

        let error = evaluate_default_package(&source).unwrap_err();
        eprintln!("PROBE: {error}");
        assert!(matches!(error, DeclarationEvaluationError::Conversion(_)));
        assert_eq!(
            match &error {
                DeclarationEvaluationError::Conversion(error) => error.field(),
                DeclarationEvaluationError::Evaluation(_) => unreachable!(),
            },
            expected_field
        );
    }
}

#[test]
fn factory_missing_argument_is_a_gluon_type_error() {
    let source = authored(
        r#"
let make = \deps -> {
    native_build_inputs = [deps.cmake],
    .. {
        meta = {
            pname = "example", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
make { wrong = a.dep.binary "cmake" }
"#,
    );

    let error = evaluate_default_package(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref error)
            if error.category == DiagnosticCategory::Type
    ));
}

#[test]
fn evaluator_accepts_typed_kinds_in_ordinary_dependency_roles() {
    let source = authored(
        r#"
let root = {
    runtime_inputs = [
        a.dep.package "runtime-package",
        a.dep.output (a.package_ref "runtime-suite") "runtime",
        a.dep.binary "runtime-binary",
        a.dep.system_binary "runtime-system-binary",
        a.dep.soname "libruntime.so.1",
        a.dep.python "runtime_python",
        a.dep.interpreter "/usr/lib/ld-runtime.so.1(x86_64)",
    ],
    conflicts = [a.dep.pkgconfig32 "conflicting-devel"],
    .. a.output "out"
}
{
    builder = a.builder.shell a.empty.scripts [
        a.dep.package "tool-package",
        a.dep.output (a.package_ref "tool-suite") "tools",
        a.dep.binary "tool-binary",
        a.dep.system_binary "tool-system-binary",
    ],
    native_build_inputs = [a.dep.cmake "NativeConfig"],
    build_inputs = [a.dep.pkgconfig "target-devel", a.dep.pkgconfig32 "target-devel"],
    check_inputs = [
        a.dep.soname "libcheck.so.1",
        a.dep.interpreter "/usr/lib/ld-check.so.1(x86_64)",
    ],
    outputs = a.outputs.explicit [root],
    .. {
        meta = {
            pname = "ordinary-roles", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
"#,
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        [
            "tool-package",
            "tool-suite-tools",
            "binary(tool-binary)",
            "sysbinary(tool-system-binary)",
        ]
    );
    assert_eq!(
        dependency_names(&evaluated.value.outputs[0].runtime_inputs),
        [
            "runtime-package",
            "runtime-suite-runtime",
            "binary(runtime-binary)",
            "sysbinary(runtime-system-binary)",
            "soname(libruntime.so.1)",
            "python(runtime_python)",
            "interpreter(/usr/lib/ld-runtime.so.1(x86_64))",
        ]
    );
    assert_eq!(
        evaluated.value.outputs[0].conflicts[0]
            .provider()
            .unwrap()
            .to_name(),
        "pkgconfig32(conflicting-devel)"
    );
}

#[test]
fn evaluator_rejects_typed_kind_mismatches_in_ordinary_dependency_roles() {
    for (field, declaration, role, kind) in [
        (
            "builder.required_tools[0]",
            "builder = a.builder.shell a.empty.scripts [a.dep.soname \"libtool.so.1\"],",
            DependencyRole::BuilderTool,
            DependencyKind::Soname,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = a.outputs.explicit [{ runtime_inputs = [a.dep.cmake \"RuntimeConfig\"], .. a.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::CMake,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = a.outputs.explicit [{ runtime_inputs = [a.dep.pkgconfig \"runtime-devel\"], .. a.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::PkgConfig,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = a.outputs.explicit [{ runtime_inputs = [a.dep.pkgconfig32 \"runtime-devel\"], .. a.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::PkgConfig32,
        ),
    ] {
        let source = authored(&format!(
            r#"
let base = {{
    meta = {{
        pname = "ordinary-role-error", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }},
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}}
{{
    {declaration}
    .. base
}}
"#
        ));

        assert_dependency_role_conversion_error(evaluate_default_package(&source).unwrap_err(), field, role, kind);
    }
}

#[test]
fn evaluator_accepts_typed_kinds_in_a_selected_profile() {
    let source = authored(
        r#"
let selected = a.profile {
    name = "emul32/x86_64",
    builder = {
        required_tools = [
            a.dep.package "profile-tool-package",
            a.dep.output (a.package_ref "profile-tool-suite") "tools",
            a.dep.binary "profile-tool-binary",
            a.dep.system_binary "profile-tool-system-binary",
        ],
        environment = [],
        phases = a.empty.scripts,
        supported_hooks = a.hook_support.all,
    },
    hooks = a.empty.hooks,
    native_build_inputs = [a.dep.cmake "ProfileNativeConfig"],
    build_inputs = [a.dep.pkgconfig "profile-devel", a.dep.pkgconfig32 "profile-devel"],
    check_inputs = [
        a.dep.soname "libprofile-check.so.1",
        a.dep.interpreter "/usr/lib/ld-profile-check.so.1(x86_64)",
    ],
}
{
    outputs = a.outputs.explicit [a.output "out"],
    profiles = [selected],
    .. {
        meta = {
            pname = "profile-roles", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
"#,
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    let selected = evaluated.value.profile("emul32/x86_64").unwrap();
    assert_eq!(
        dependency_names(selected.builder.required_tools()),
        [
            "profile-tool-package",
            "profile-tool-suite-tools",
            "binary(profile-tool-binary)",
            "sysbinary(profile-tool-system-binary)",
        ]
    );
    assert_eq!(
        dependency_names(&selected.build_inputs),
        ["pkgconfig(profile-devel)", "pkgconfig32(profile-devel)"]
    );
}

#[test]
fn evaluator_rejects_typed_kind_mismatches_in_a_selected_profile() {
    for (dependency, kind) in [
        ("a.dep.pkgconfig32 \"profile-devel\"", DependencyKind::PkgConfig32),
        (
            "a.dep.interpreter \"/usr/lib/ld-profile.so.1(x86_64)\"",
            DependencyKind::Interpreter,
        ),
    ] {
        let source = authored(&format!(
            r#"
let selected = a.profile {{
    name = "emul32/x86_64",
    builder = {{
        required_tools = [{dependency}],
        environment = [],
        phases = a.empty.scripts,
        supported_hooks = a.hook_support.all,
    }},
    hooks = a.empty.hooks,
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
}}
{{
    outputs = a.outputs.explicit [a.output "out"],
    profiles = [selected],
    .. {{
        meta = {{
            pname = "profile-role-error", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        }},
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }}
}}
"#
        ));

        assert_dependency_role_conversion_error(
            evaluate_default_package(&source).unwrap_err(),
            "profiles[0].builder.required_tools[0]",
            DependencyRole::BuilderTool,
            kind,
        );
    }
}

#[test]
fn missing_local_output_reference_has_an_indexed_field() {
    let source = authored(
        r#"
let root = {
    runtime_inputs = [a.dep.output (a.package_ref "example") "missing"],
    .. a.output "out"
}
{
    outputs = a.outputs.explicit [root],
    .. {
        meta = {
            pname = "example", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = ["MPL-2.0"],
        },
        builder = a.builder.custom a.empty.builder,
        sources = [],
        native_build_inputs = [],
        build_inputs = [],
        check_inputs = [],
        outputs = a.outputs.default,
        options = a.unset,
        profiles = [],
        architectures = [],
        tuning = [],
        emul32 = a.false,
        mold = a.false,
        hooks = a.unset,
    }
}
"#,
    );

    let error = evaluate_default_package(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(
            PackageConversionError::MissingOutputReference { ref field, .. }
        ) if field == "outputs[0].runtime_inputs[0]"
    ));
}

#[test]
fn evaluator_validates_the_concrete_package() {
    let source = authored(
        r#"
{
    meta = {
        pname = "example", version = "v1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
"#,
    );

    let error = evaluate_default_package(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(ref error)
            if error.field() == "meta.version"
    ));
}

#[test]
fn evaluator_rejects_malformed_source_fields_before_planning() {
    for (source, expected_field, expected_message) in [
        (
            r#"a.source.archive "https://example.com/source.tar.xz" "short""#,
            "sources[0].hash",
            "64 lowercase ASCII hexadecimal",
        ),
        (
            r#"a.source.archive_with {
                url = "https://example.com/source.tar.xz",
                hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                rename = a.optional.set "../escape",
                strip_dirs = a.optional.unset,
                unpack = a.true,
                unpack_dir = a.optional.unset,
            }"#,
            "sources[0].rename",
            "normalized filename component",
        ),
        (
            r#"a.source.archive_with {
                url = "https://example.com/source.tar.xz",
                hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                rename = a.optional.unset,
                strip_dirs = a.optional.unset,
                unpack = a.true,
                unpack_dir = a.optional.set "../../escape",
            }"#,
            "sources[0].unpack_dir",
            "normalized, non-empty relative path",
        ),
        (
            r#"a.source.git "https://example.com/source.git" """#,
            "sources[0].git_ref",
            "must be non-empty",
        ),
    ] {
        let source = authored(&format!(
            r#"
let base = {{
    meta = {{
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }},
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}}
{{
    sources = [{source}],
    .. base
}}
"#
        ));

        let error = evaluate_default_package(&source).unwrap_err();
        let DeclarationEvaluationError::Conversion(conversion) = &error else {
            panic!("malformed source reached the wrong diagnostic layer: {error}")
        };
        assert_eq!(conversion.field(), expected_field);
        assert!(
            error.to_string().contains(expected_message),
            "diagnostic did not explain {expected_field}: {error}"
        );
    }
}

#[test]
fn evaluator_rejects_package_metadata_that_can_escape_artifact_paths() {
    for (pname, version, field) in [
        ("../../escape", "1.0.0", "meta.pname"),
        ("example", "1/../../escape", "meta.version"),
    ] {
        let source = authored(&format!(
            r#"
{{
    meta = {{
        pname = {pname:?}, version = {version:?}, release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }},
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}}
"#
        ));

        let error = evaluate_default_package(&source).unwrap_err();
        assert!(matches!(
            error,
            DeclarationEvaluationError::Conversion(ref error) if error.field() == field
        ));
    }
}

#[test]
fn evaluator_rejects_unsafe_or_duplicate_profile_keys() {
    let unsafe_profile = authored(
        r#"
let profile_named = \name -> a.profile {
    name,
    builder = a.empty.builder,
    hooks = a.empty.hooks,
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
}
let base = {
    meta = {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
{
    profiles = [profile_named "emul32/../x86_64"],
    .. base
}
"#,
    );

    let error = evaluate_default_package(&unsafe_profile).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(PackageConversionError::InvalidProfileName {
            index: 0,
            ref name,
        }) if name == "emul32/../x86_64"
    ));

    let duplicate_profiles = authored(
        r#"
let profile_named = \name -> a.profile {
    name,
    builder = a.empty.builder,
    hooks = a.empty.hooks,
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
}
let base = {
    meta = {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
{
    profiles = [profile_named "native", profile_named "emul32/x86_64", profile_named "native"],
    .. base
}
"#,
    );

    let error = evaluate_default_package(&duplicate_profiles).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(PackageConversionError::DuplicateProfileName {
            first_index: 0,
            duplicate_index: 2,
            ref name,
        }) if name == "native"
    ));
}

#[test]
fn evaluator_rejects_networked_frozen_packages_with_locked_source_guidance() {
    let source = authored(
        r#"
let base = {
    meta = {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
{
    options = a.some.options (a.options {
        toolchain = a.toolchain.llvm,
        cspgo = a.false,
        samplepgo = a.false,
        debug = a.true,
        strip = a.true,
        networking = a.true,
        compressman = a.false,
        lastrip = a.true,
    }),
    .. base
}
"#,
    );

    let error = evaluate_default_package(&source).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(PackageConversionError::FrozenBuildNetworkingUnsupported)
    ));
    assert!(error.to_string().contains("locked sources"));
}

#[test]
fn evaluator_keeps_special_constructor_reserved_but_rejects_concrete_package_use() {
    let source = authored(
        r#"
let base = {
    meta = {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
{
    outputs = a.outputs.explicit [a.output_with {
        paths = [a.path.special "/usr/lib/example/events.fifo"],
        .. a.output "out"
    }],
    .. base
}
"#,
    );

    let error = evaluate_default_package(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(
            PackageConversionError::UnsupportedSpecialPathRule { ref field }
        ) if field == "outputs[0].paths[0]"
    ));
    assert!(error.to_string().contains("reserved by package-v3"));
}

#[test]
fn package_fingerprint_is_deterministic_and_binds_explicit_inputs() {
    let source = authored(
        r#"
let abi_version: Int = a.abi_version
{
    meta = {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [],
    build_inputs = [],
    check_inputs = [],
    outputs = a.outputs.default,
    options = a.unset,
    profiles = [],
    architectures = [],
    tuning = [],
    emul32 = a.false,
    mold = a.false,
    hooks = a.unset,
}
"#,
    );
    let evaluator = GluonPackageEvaluator::default();

    let first = evaluate_package_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let repeated = evaluate_package_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let changed = evaluate_package_with_inputs(&evaluator, &source, b"lock-v2").unwrap();
    let typed = <GluonPackageEvaluator as DeclarationInputEvaluator<PackageSpec>>::evaluate_with_inputs(
        &GluonPackageEvaluator::default(),
        &source,
        b"lock-v1",
    )
    .unwrap();

    assert_eq!(first.value, repeated.value);
    assert_eq!(first.identity, repeated.identity);
    assert_eq!(typed.value, first.value);
    assert_eq!(typed.identity, first.identity);
    assert_ne!(first.identity.sha256, changed.identity.sha256);
}

include!("package_v3/normalized_value.rs");
