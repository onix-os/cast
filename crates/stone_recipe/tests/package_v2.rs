// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

use gluon_config::{DiagnosticCategory, Evaluator, Source, SourceRoot};
use stone_recipe::package::{
    BuilderEnvironmentSpec, DependencySpec, PACKAGE_ABI_VERSION, PackageConversionError, PackageEvaluationError,
    StepSpec, SupportedHooksSpec, evaluate_gluon, evaluate_gluon_with, evaluate_gluon_with_inputs,
};

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn authored(body: &str) -> Source {
    Source::new("stone.glu", format!("let b = import! boulder.package.v2\n{body}"))
}

#[test]
fn imported_factory_arguments_and_typed_patch_produce_a_direct_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");
    let source_root = SourceRoot::new(&root).unwrap();
    let source = source_root
        .load(Path::new("package_v2_stone.glu"), 1024 * 1024)
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
        ["binary(cmake)", "binary(ninja)", "binary(ctest)"]
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
                script: r#"ln -s factory-hello "${BOULDER_INSTALL_ROOT}${BOULDER_BINDIR}/hello""#.to_owned()
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
    assert_eq!(PACKAGE_ABI_VERSION, 2);
    assert!(
        evaluated
            .fingerprint
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "boulder.package.v2")
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
