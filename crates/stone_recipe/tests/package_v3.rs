use std::path::Path;

#[path = "package_v3/adapter.rs"]
mod adapter;

use adapter::{
    PackageDeclarationError, evaluate_default_package, evaluate_package,
    evaluate_package_with_inputs, rooted_package_evaluator,
};
use declarative_config::{DeclarationEvaluationError, DeclarationInputEvaluator, Source, SourceRoot};
use gluon_config::DiagnosticCategory;
use stone_recipe::package::{
    BuilderEnvironmentSpec, BuiltProgramSpec, DependencyKind, DependencyRole, DependencySpec, PACKAGE_ABI_VERSION,
    GluonPackageEvaluator, PackageConversionError, ProgramSpec, StepSpec, SupportedHooksSpec,
};

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn authored(body: &str) -> Source {
    Source::new("stone.glu", format!("let b = import! cast.package.v3\n{body}"))
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
    let error = evaluate_default_package(&authored("b.step.cargo_fetch")).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref diagnostic)
            if diagnostic.category == DiagnosticCategory::Type
                && diagnostic.message.contains("cargo_fetch")
    ));
}

#[test]
fn imported_factory_arguments_and_typed_patch_produce_a_direct_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");
    let source_root = SourceRoot::new(&root).unwrap();
    let source = source_root
        .load(Path::new("package_v3_stone.glu"), 1024 * 1024)
        .unwrap();
    let evaluator = rooted_package_evaluator(source_root);

    let evaluated = evaluate_package(&evaluator, &source).unwrap();

    assert_eq!(evaluated.value.meta.pname, "factory-hello");
    assert_eq!(evaluated.value.outputs.len(), 10);
    assert_eq!(evaluated.value.outputs[9].name, "dev");
    assert!(matches!(
        evaluated.value.build_inputs[0],
        DependencySpec::Package(ref package) if package.name == "zlib"
    ));
    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        ["binary(sh)", "binary(ninja)"]
    );
    assert_eq!(evaluated.value.builder.environment, [BuilderEnvironmentSpec::CMake]);
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    assert_eq!(
        dependency_names(&evaluated.value.build_inputs),
        ["zlib", "pkgconfig(libressl)"]
    );
    let phases = evaluated.value.phases();
    assert_eq!(
        phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_DOCUMENTATION=OFF".to_owned()]
        }]
    );
    assert_eq!(phases.build.steps, [StepSpec::CMakeBuild]);
    assert_eq!(phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(
        phases.install.steps,
        [
            StepSpec::CMakeInstall,
            StepSpec::Shell {
                interpreter: binary_program("bash"),
                declared_programs: vec![binary_program("ln")],
                script: r#"ln -s factory-hello "${CAST_INSTALL_ROOT}${CAST_BINDIR}/hello""#.to_owned()
            }
        ]
    );
    assert_eq!(
        evaluated
            .value
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "docs",
            "devel",
            "dbginfo",
            "libs",
            "32bit",
            "32bit-devel",
            "32bit-dbginfo",
            "demos",
            "dev",
        ]
    );
    assert_eq!(
        evaluated
            .value
            .outputs
            .iter()
            .map(|output| (output.name.as_str(), output.include_in_manifest))
            .collect::<Vec<_>>(),
        [
            ("out", true),
            ("docs", true),
            ("devel", true),
            ("dbginfo", false),
            ("libs", true),
            ("32bit", true),
            ("32bit-devel", true),
            ("32bit-dbginfo", false),
            ("demos", true),
            ("dev", true),
        ]
    );
    assert_eq!(
        dependency_names(&evaluated.value.outputs[0].runtime_inputs),
        ["soname(libtls.so.28)"]
    );
    assert_eq!(
        dependency_names(&evaluated.value.outputs[9].runtime_inputs),
        ["factory-hello"]
    );
    assert_eq!(evaluated.value.architectures, ["x86_64"]);
    assert_eq!(PACKAGE_ABI_VERSION, 3);
    assert!(
        evaluated
            .identity
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "cast.package.v3")
    );
}

#[test]
fn manifest_membership_is_explicit_not_inferred_from_package_name() {
    let source = authored(
        r#"
let root = b.output "out"
{
    outputs = [root],
    .. b.mk_package (b.meta {
        pname = "symbols-dbginfo", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
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
let tool = b.package_ref "odd-tool"
let scripts = b.scripts {
    build = b.phase [
        b.step.run (b.program.package tool "/opt/odd/bin/tool") ["--frozen"],
        b.step.run_built (b.program.built "build/generated-tool") ["--self-test"],
        b.step.shell_with {
            interpreter = b.program.binary "dash",
            declared_programs = [b.program.package tool "/opt/odd/bin/helper"],
            script = "helper --check",
        },
        b.step.shell "echo builtin",
    ],
    .. b.defaults.scripts
}
{
    builder = b.builder.custom scripts [],
    outputs = [b.output "out"],
    .. b.mk_package (b.meta {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
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
let scripts = b.scripts {{
    check = b.phase [b.step.run_built (b.program.built {invalid:?}) []],
    .. b.defaults.scripts
}}
{{
    builder = b.builder.custom scripts [],
    outputs = [b.output "out"],
    .. b.mk_package (b.meta {{
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }})
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
            r#"{ path = "tool", requirement = b.dep.binary "tool" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
        (
            r#"{ path = "/usr/bin/other", requirement = b.dep.binary "tool" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
        (
            r#"{ path = "/usr/bin/pkg-config", requirement = b.dep.pkgconfig "example" }"#,
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            r#"{ path = "/usr/bin/nested/tool", requirement = b.dep.binary "nested/tool" }"#,
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            r#"{ path = "/usr/bin/tool", requirement = b.dep.package "tool-package" }"#,
            "builder.phases.build.steps[0].program.path",
        ),
    ] {
        let source = authored(&format!(
            r#"
let scripts = b.scripts {{
    build = b.phase [b.step.run {program} []],
    .. b.defaults.scripts
}}
{{
    builder = b.builder.custom scripts [],
    outputs = [b.output "out"],
    .. b.mk_package (b.meta {{
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }})
}}
"#
        ));

        let error = evaluate_default_package(&source).unwrap_err();
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
fn patch_replace_can_explicitly_clear_an_array() {
    let source = authored(
        r#"
let base = {
    architectures = ["x86_64"],
    .. b.mk_package (b.meta {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
}
let patch = b.package_patch {
    architectures = b.patch.array.replace [],
    .. b.defaults.package_patch
}
b.override_attrs patch base
"#,
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    assert!(evaluated.value.architectures.is_empty());
}

#[test]
fn factory_missing_argument_is_a_gluon_type_error() {
    let source = authored(
        r#"
let make = \deps -> {
    native_build_inputs = [deps.cmake],
    .. b.mk_package (b.meta {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
}
make { wrong = b.dep.binary "cmake" }
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
        b.dep.package "runtime-package",
        b.dep.output (b.package_ref "runtime-suite") "runtime",
        b.dep.binary "runtime-binary",
        b.dep.system_binary "runtime-system-binary",
        b.dep.soname "libruntime.so.1",
        b.dep.python "runtime_python",
        b.dep.interpreter "/usr/lib/ld-runtime.so.1(x86_64)",
    ],
    conflicts = [b.dep.pkgconfig32 "conflicting-devel"],
    .. b.output "out"
}
{
    builder = b.builder.custom b.defaults.scripts [
        b.dep.package "tool-package",
        b.dep.output (b.package_ref "tool-suite") "tools",
        b.dep.binary "tool-binary",
        b.dep.system_binary "tool-system-binary",
    ],
    native_build_inputs = [b.dep.cmake "NativeConfig"],
    build_inputs = [b.dep.pkgconfig "target-devel", b.dep.pkgconfig32 "target-devel"],
    check_inputs = [
        b.dep.soname "libcheck.so.1",
        b.dep.interpreter "/usr/lib/ld-check.so.1(x86_64)",
    ],
    outputs = [root],
    .. b.mk_package (b.meta {
        pname = "ordinary-roles", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
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
            "builder = b.builder.custom b.defaults.scripts [b.dep.soname \"libtool.so.1\"],",
            DependencyRole::BuilderTool,
            DependencyKind::Soname,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = [{ runtime_inputs = [b.dep.cmake \"RuntimeConfig\"], .. b.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::CMake,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = [{ runtime_inputs = [b.dep.pkgconfig \"runtime-devel\"], .. b.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::PkgConfig,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            "outputs = [{ runtime_inputs = [b.dep.pkgconfig32 \"runtime-devel\"], .. b.output \"out\" }],",
            DependencyRole::Runtime,
            DependencyKind::PkgConfig32,
        ),
    ] {
        let source = authored(&format!(
            r#"
let base = b.mk_package (b.meta {{
    pname = "ordinary-role-error", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
}})
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
let selected = b.profile_with {
    builder = b.builder.custom b.defaults.scripts [
        b.dep.package "profile-tool-package",
        b.dep.output (b.package_ref "profile-tool-suite") "tools",
        b.dep.binary "profile-tool-binary",
        b.dep.system_binary "profile-tool-system-binary",
    ],
    native_build_inputs = [b.dep.cmake "ProfileNativeConfig"],
    build_inputs = [b.dep.pkgconfig "profile-devel", b.dep.pkgconfig32 "profile-devel"],
    check_inputs = [
        b.dep.soname "libprofile-check.so.1",
        b.dep.interpreter "/usr/lib/ld-profile-check.so.1(x86_64)",
    ],
    .. b.profile "emul32/x86_64"
}
{
    outputs = [b.output "out"],
    profiles = [selected],
    .. b.mk_package (b.meta {
        pname = "profile-roles", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
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
        ("b.dep.pkgconfig32 \"profile-devel\"", DependencyKind::PkgConfig32),
        (
            "b.dep.interpreter \"/usr/lib/ld-profile.so.1(x86_64)\"",
            DependencyKind::Interpreter,
        ),
    ] {
        let source = authored(&format!(
            r#"
let selected = b.profile_with {{
    builder = b.builder.custom b.defaults.scripts [{dependency}],
    .. b.profile "emul32/x86_64"
}}
{{
    outputs = [b.output "out"],
    profiles = [selected],
    .. b.mk_package (b.meta {{
        pname = "profile-role-error", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    }})
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
    runtime_inputs = [b.dep.output (b.package_ref "example") "missing"],
    .. b.output "out"
}
{
    outputs = [root],
    .. b.mk_package (b.meta {
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.com", license = ["MPL-2.0"],
    })
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
b.mk_package (b.meta {
    pname = "example", version = "v1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
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
            r#"b.source.archive "https://example.com/source.tar.xz" "short""#,
            "sources[0].hash",
            "64 lowercase ASCII hexadecimal",
        ),
        (
            r#"b.source.archive_with {
                url = "https://example.com/source.tar.xz",
                hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                rename = b.optional.set "../escape",
                strip_dirs = b.optional.unset,
                unpack = b.boolean.true,
                unpack_dir = b.optional.unset,
            }"#,
            "sources[0].rename",
            "normalized filename component",
        ),
        (
            r#"b.source.archive_with {
                url = "https://example.com/source.tar.xz",
                hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                rename = b.optional.unset,
                strip_dirs = b.optional.unset,
                unpack = b.boolean.true,
                unpack_dir = b.optional.set "../../escape",
            }"#,
            "sources[0].unpack_dir",
            "normalized, non-empty relative path",
        ),
        (
            r#"b.source.git "https://example.com/source.git" """#,
            "sources[0].git_ref",
            "must be non-empty",
        ),
    ] {
        let source = authored(&format!(
            r#"
let base = b.mk_package (b.meta {{
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
}})
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
b.mk_package (b.meta {{
    pname = {pname:?}, version = {version:?}, release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
}})
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
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
{
    profiles = [b.profile "emul32/../x86_64"],
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
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
{
    profiles = [b.profile "native", b.profile "emul32/x86_64", b.profile "native"],
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
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
{
    options = {
        networking = b.boolean.true,
        .. b.defaults.options
    },
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
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
{
    outputs = [b.output_with {
        paths = [b.path.special "/usr/lib/example/events.fifo"],
        .. b.output "out"
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
let abi_version: Int = b.abi_version
b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
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
