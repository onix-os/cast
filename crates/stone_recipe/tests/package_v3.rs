use std::path::Path;

use gluon_config::{DiagnosticCategory, Evaluator, Source, SourceRoot};
use stone_recipe::package::{
    BuilderEnvironmentSpec, BuiltProgramSpec, DependencySpec, PACKAGE_ABI_VERSION, PackageConversionError,
    PackageEvaluationError, ProgramSpec, StepSpec, SupportedHooksSpec, evaluate_gluon, evaluate_gluon_with,
    evaluate_gluon_with_inputs,
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
        let error = evaluate_gluon(&Source::new("stone.glu", format!("import! {module}"))).unwrap_err();
        assert!(matches!(
            error,
            PackageEvaluationError::Evaluation(ref diagnostic)
                if diagnostic.category == DiagnosticCategory::Import
                    && diagnostic.message.contains(module)
        ));
    }
}

#[test]
fn frozen_package_abi_has_no_cargo_fetch_escape_hatch() {
    let error = evaluate_gluon(&authored("b.step.cargo_fetch")).unwrap_err();

    assert!(matches!(
        error,
        PackageEvaluationError::Evaluation(ref diagnostic)
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
    let evaluator = Evaluator::default().with_source_root(source_root);

    let evaluated = evaluate_gluon_with(&evaluator, &source).unwrap();

    assert_eq!(evaluated.package.meta.pname, "factory-hello");
    assert_eq!(evaluated.package.outputs.len(), 10);
    assert_eq!(evaluated.package.outputs[9].name, "dev");
    assert!(matches!(
        evaluated.package.build_inputs[0],
        DependencySpec::Package(ref package) if package.name == "zlib"
    ));
    assert_eq!(
        dependency_names(evaluated.package.builder.required_tools()),
        ["binary(ninja)"]
    );
    assert_eq!(evaluated.package.builder.environment, [BuilderEnvironmentSpec::CMake]);
    assert_eq!(evaluated.package.builder.supported_hooks, SupportedHooksSpec::all());
    assert_eq!(dependency_names(&evaluated.package.build_inputs), ["zlib"]);
    let phases = evaluated.package.phases();
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
            .package
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
            .package
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
        dependency_names(&evaluated.package.outputs[0].runtime_inputs),
        ["pkgconfig(libressl)"]
    );
    assert_eq!(
        dependency_names(&evaluated.package.outputs[9].runtime_inputs),
        ["factory-hello"]
    );
    assert_eq!(evaluated.package.architectures, ["x86_64"]);
    assert_eq!(PACKAGE_ABI_VERSION, 3);
    assert!(
        evaluated
            .fingerprint
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

    let evaluated = evaluate_gluon(&source).unwrap();
    assert_eq!(evaluated.package.outputs.len(), 1);
    assert!(evaluated.package.outputs[0].include_in_manifest);
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

    let evaluated = evaluate_gluon(&source).unwrap();
    assert_eq!(
        evaluated.package.builder.phases.build.steps,
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
        let error = evaluate_gluon(&source).unwrap_err();
        assert!(matches!(
            error,
            PackageEvaluationError::Conversion(PackageConversionError::InvalidText { ref field, .. })
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

        let error = evaluate_gluon(&source).unwrap_err();
        assert!(matches!(error, PackageEvaluationError::Conversion(_)));
        assert_eq!(
            match &error {
                PackageEvaluationError::Conversion(error) => error.field(),
                PackageEvaluationError::Evaluation(_) => unreachable!(),
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

    let evaluated = evaluate_gluon(&source).unwrap();
    assert!(evaluated.package.architectures.is_empty());
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

    let error = evaluate_gluon(&source).unwrap_err();
    assert!(matches!(
        error,
        PackageEvaluationError::Evaluation(ref error)
            if error.category == DiagnosticCategory::Type
    ));
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

    let error = evaluate_gluon(&source).unwrap_err();
    assert!(matches!(
        error,
        PackageEvaluationError::Conversion(
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

    let error = evaluate_gluon(&source).unwrap_err();
    assert!(matches!(
        error,
        PackageEvaluationError::Conversion(ref error)
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

        let error = evaluate_gluon(&source).unwrap_err();
        let PackageEvaluationError::Conversion(conversion) = &error else {
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

        let error = evaluate_gluon(&source).unwrap_err();
        assert!(matches!(
            error,
            PackageEvaluationError::Conversion(ref error) if error.field() == field
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

    let error = evaluate_gluon(&unsafe_profile).unwrap_err();
    assert!(matches!(
        error,
        PackageEvaluationError::Conversion(PackageConversionError::InvalidProfileName {
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

    let error = evaluate_gluon(&duplicate_profiles).unwrap_err();
    assert!(matches!(
        error,
        PackageEvaluationError::Conversion(PackageConversionError::DuplicateProfileName {
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

    let error = evaluate_gluon(&source).unwrap_err();

    assert!(matches!(
        error,
        PackageEvaluationError::Conversion(PackageConversionError::FrozenBuildNetworkingUnsupported)
    ));
    assert!(error.to_string().contains("locked sources"));
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
    let evaluator = Evaluator::default();

    let first = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let repeated = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let changed = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v2").unwrap();

    assert_eq!(first.package, repeated.package);
    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
}
