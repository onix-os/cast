// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

use gluon_config::{DiagnosticCategory, Evaluator, Source, SourceRoot};
use stone_recipe::package::{
    DependencySpec, PACKAGE_ABI_VERSION, PackageConversionError, PackageEvaluationError, evaluate_gluon,
    evaluate_gluon_with, evaluate_gluon_with_inputs,
};

fn authored(body: &str) -> Source {
    Source::new("stone.glu", format!("let b = import! boulder.package.v2\n{body}"))
}

#[test]
fn imported_factory_arguments_and_typed_patch_lower_to_recipe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");
    let source_root = SourceRoot::new(&root).unwrap();
    let source = source_root
        .load(Path::new("package_v2_stone.glu"), 1024 * 1024)
        .unwrap();
    let evaluator = Evaluator::default().with_source_root(source_root);

    let evaluated = evaluate_gluon_with(&evaluator, &source).unwrap();

    assert_eq!(evaluated.package.meta.pname, "factory-hello");
    assert_eq!(evaluated.package.outputs.len(), 2);
    assert_eq!(evaluated.package.outputs[1].name, "dev");
    assert!(matches!(
        evaluated.package.build_inputs[0],
        DependencySpec::Package(ref package) if package.name == "zlib"
    ));
    assert_eq!(
        evaluated.recipe.build.build_deps,
        ["binary(cmake)", "binary(ninja)", "binary(ctest)", "zlib"]
    );
    assert_eq!(
        evaluated.recipe.build.setup.as_deref(),
        Some("%cmake -DBUILD_DOCUMENTATION=OFF")
    );
    assert_eq!(evaluated.recipe.build.build.as_deref(), Some("%cmake_build"));
    assert_eq!(evaluated.recipe.build.check.as_deref(), Some("%cmake_test"));
    assert_eq!(
        evaluated.recipe.build.install.as_deref(),
        Some("%cmake_install\nln -s factory-hello %(installroot)/usr/bin/hello")
    );
    assert_eq!(evaluated.recipe.package.run_deps, ["pkgconfig(libressl)"]);
    assert_eq!(evaluated.recipe.sub_packages[0].key, "factory-hello-dev");
    assert_eq!(evaluated.recipe.sub_packages[0].value.run_deps, ["factory-hello"]);
    assert_eq!(evaluated.recipe.architectures, ["x86_64"]);
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
