// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::Source;
use stone_recipe::package::evaluate_gluon;

fn package(builder_import: &str, body: &str) -> Source {
    Source::new(
        "stone.glu",
        format!(
            r#"let b = import! boulder.package.v2
let builder = import! {builder_import}
let base = b.mk_package (b.meta {{
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
}})
{{
    builder = {body},
    .. base
}}
"#
        ),
    )
}

#[test]
fn meson_builder_lowers_tools_flags_and_disabled_checks() {
    let source = package(
        "boulder.builders.meson.v1",
        r#"builder.builder {
            flags = ["-Ddocumentation=false"],
            run_tests = b.boolean.false,
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(
        evaluated.recipe.build.build_deps,
        ["binary(cmake)", "binary(meson)", "binary(ninja)", "binary(pkgconf)",]
    );
    assert_eq!(
        evaluated.recipe.build.setup.as_deref(),
        Some("%meson -Ddocumentation=false")
    );
    assert_eq!(evaluated.recipe.build.build.as_deref(), Some("%meson_build"));
    assert_eq!(evaluated.recipe.build.install.as_deref(), Some("%meson_install"));
    assert!(evaluated.recipe.build.check.is_none());
    assert!(
        evaluated
            .fingerprint
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "boulder.builders.meson.v1")
    );
}

#[test]
fn cargo_builder_lowers_features_binaries_environment_and_checks() {
    let source = package(
        "boulder.builders.cargo.v1",
        r#"builder.builder {
            features = ["cli", "tls"],
            binaries = ["example", "examplectl"],
            run_tests = b.boolean.true,
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(evaluated.recipe.build.build_deps, ["binary(cargo)"]);
    assert_eq!(
        evaluated.recipe.build.build.as_deref(),
        Some("%cargo_build --features cli,tls")
    );
    assert_eq!(
        evaluated.recipe.build.install.as_deref(),
        Some("%cargo_install example examplectl")
    );
    assert_eq!(
        evaluated.recipe.build.check.as_deref(),
        Some("%cargo_test --features cli,tls")
    );
    assert_eq!(
        evaluated.recipe.build.environment.as_deref(),
        Some("%cargo_set_environment")
    );
}

#[test]
fn autotools_builder_lowers_to_existing_phase_contract() {
    let source = package(
        "boulder.builders.autotools.v1",
        r#"builder.builder {
            flags = ["--disable-static"],
            .. builder.defaults
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(
        evaluated.recipe.build.build_deps,
        ["binary(autoconf)", "binary(automake)", "binary(make)"]
    );
    assert_eq!(
        evaluated.recipe.build.setup.as_deref(),
        Some("%configure --disable-static")
    );
    assert_eq!(evaluated.recipe.build.build.as_deref(), Some("%make"));
    assert_eq!(evaluated.recipe.build.check.as_deref(), Some("%make check"));
    assert_eq!(evaluated.recipe.build.install.as_deref(), Some("%make_install"));
}

#[test]
fn custom_shell_builder_requires_structural_tools_and_composes_hooks() {
    let source = Source::new(
        "stone.glu",
        r#"let b = import! boulder.package.v2
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
let scripts = b.scripts {
    build = b.optional.set "zig build",
    install = b.optional.set "zig build install --prefix %(installroot)/usr",
    .. b.defaults.scripts
}
{
    builder = b.builder.shell scripts [b.dep.binary "zig"],
    hooks = b.hooks {
        pre_build = ["prepare-generated-files"],
        post_build = ["verify-generated-files"],
        environment = ["ZIG_GLOBAL_CACHE_DIR=%(buildroot)/zig-cache; export ZIG_GLOBAL_CACHE_DIR"],
        .. b.defaults.hooks
    },
    .. base
}
"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(evaluated.recipe.build.build_deps, ["binary(zig)"]);
    assert_eq!(
        evaluated.recipe.build.build.as_deref(),
        Some("prepare-generated-files\nzig build\nverify-generated-files")
    );
    assert_eq!(
        evaluated.recipe.build.environment.as_deref(),
        Some("ZIG_GLOBAL_CACHE_DIR=%(buildroot)/zig-cache; export ZIG_GLOBAL_CACHE_DIR")
    );
}
