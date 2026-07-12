// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::{DiagnosticCategory, Evaluator, Source};
use stone_recipe::{
    PathKind, RECIPE_ABI_VERSION, RecipeEvaluationError, Tuning, evaluate_gluon, evaluate_gluon_with_inputs,
    tuning::Toolchain, upstream::Props,
};

const SOURCE: &str = r#"{
    name = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}"#;

fn authored(body: &str) -> Source {
    Source::new("stone.glu", format!("let boulder = import! boulder.recipe.v1\n{body}"))
}

#[test]
fn direct_record_literal_evaluates_with_defaults() {
    let source = authored(&format!(
        r#"
let source = {SOURCE}
{{
    source,
    build = boulder.defaults.build,
    package = boulder.defaults.package,
    options = boulder.defaults.options,
    profiles = [],
    sub_packages = [],
    upstreams = [],
    architectures = [],
    tuning = [],
    emul32 = boulder.boolean.false,
    mold = boulder.boolean.false,
}}
"#
    ));

    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(evaluated.recipe.source.name, "example");
    assert_eq!(evaluated.recipe.source.version, "1.2.3");
    assert!(matches!(evaluated.recipe.options.toolchain, Toolchain::Llvm));
    assert!(evaluated.recipe.options.debug);
    assert!(evaluated.recipe.options.strip);
    assert!(evaluated.recipe.options.lastrip);
    assert!(evaluated.recipe.profiles.is_empty());
    assert!(evaluated.recipe.sub_packages.is_empty());
}

#[test]
fn constructors_cover_all_fields_and_explicit_variants() {
    let source = authored(&format!(
        r#"
let base = boulder.recipe (boulder.source {SOURCE})
{{
    build = boulder.build {{
        setup = boulder.optional.set "setup",
        build = boulder.optional.set "build",
        install = boulder.optional.set "install",
        check = boulder.optional.set "check",
        workload = boulder.optional.set "workload",
        environment = boulder.optional.set "environment",
        build_deps = ["build-dependency"],
        check_deps = ["check-dependency"],
    }},
    package = boulder.package {{
        summary = boulder.optional.set "summary",
        description = boulder.optional.set "description",
        provides_exclude = ["provided(*)"],
        run_deps = ["runtime-dependency"],
        run_deps_exclude = ["excluded-runtime(*)"],
        paths = [
            boulder.path.any "/usr/share/example",
            boulder.path.exe "/usr/bin/example",
            boulder.path.symlink "/usr/bin/example-link",
            boulder.path.special "/usr/lib/example.special",
        ],
        conflicts = ["other-package"],
    }},
    options = boulder.options {{
        toolchain = boulder.toolchain.gnu,
        cspgo = boulder.boolean.true,
        samplepgo = boulder.boolean.true,
        debug = boulder.boolean.false,
        strip = boulder.boolean.false,
        networking = boulder.boolean.true,
        compressman = boulder.boolean.true,
        lastrip = boulder.boolean.false,
    }},
    profiles = [
        boulder.named "x86_64" {{
            build = boulder.optional.set "profile build",
            .. boulder.defaults.build
        }},
        boulder.named "aarch64" boulder.defaults.build,
    ],
    sub_packages = [
        boulder.named "example-devel" {{
            summary = boulder.optional.set "development files",
            .. boulder.defaults.package
        }},
        boulder.named "example-docs" boulder.defaults.package,
    ],
    upstreams = [
        boulder.upstream.archive_with {{
            url = "https://example.com/source.tar.xz",
            hash = "archive-hash",
            rename = boulder.optional.set "renamed.tar.xz",
            strip_dirs = boulder.optional.set 2,
            unpack = boulder.boolean.false,
            unpack_dir = boulder.optional.set "archive",
        }},
        boulder.upstream.git_with {{
            url = "https://example.com/source.git",
            git_ref = "v1.2.3",
            clone_dir = boulder.optional.set "git",
        }},
        boulder.upstream.archive "https://example.com/minimal.tar.xz" "minimal-hash",
        boulder.upstream.git "https://example.com/minimal.git" "main",
    ],
    architectures = ["x86_64", "aarch64"],
    tuning = [
        boulder.named "harden" boulder.tuning.enable,
        boulder.named "lto" boulder.tuning.disable,
        boulder.named "optimize" (boulder.tuning.config "speed"),
    ],
    emul32 = boulder.boolean.true,
    mold = boulder.boolean.true,
    .. base
}}
"#
    ));

    let recipe = evaluate_gluon(&source).unwrap().recipe;

    assert_eq!(recipe.build.setup.as_deref(), Some("setup"));
    assert_eq!(recipe.build.check_deps, ["check-dependency"]);
    assert_eq!(recipe.package.paths[0].kind, PathKind::Any);
    assert_eq!(recipe.package.paths[1].kind, PathKind::Exe);
    assert_eq!(recipe.package.paths[2].kind, PathKind::Symlink);
    assert_eq!(recipe.package.paths[3].kind, PathKind::Special);
    assert!(matches!(recipe.options.toolchain, Toolchain::Gnu));
    assert_eq!(recipe.profiles.len(), 2);
    assert_eq!(recipe.sub_packages.len(), 2);
    assert!(matches!(
        recipe.upstreams[0].props,
        Props::Plain {
            strip_dirs: Some(2),
            unpack: false,
            ..
        }
    ));
    assert!(matches!(recipe.upstreams[1].props, Props::Git { .. }));
    assert!(matches!(
        recipe.upstreams[2].props,
        Props::Plain {
            unpack: true,
            rename: None,
            ..
        }
    ));
    assert!(matches!(recipe.upstreams[3].props, Props::Git { clone_dir: None, .. }));
    assert_eq!(recipe.architectures, ["x86_64", "aarch64"]);
    assert!(matches!(recipe.tuning[0].value, Tuning::Enable));
    assert!(matches!(recipe.tuning[1].value, Tuning::Disable));
    assert!(matches!(recipe.tuning[2].value, Tuning::Config(ref value) if value == "speed"));
    assert!(recipe.emul32);
    assert!(recipe.mold);
}

#[test]
fn composition_helpers_replace_control_file_semantics() {
    let source = authored(&format!(
        r#"
let base = boulder.recipe (boulder.source {SOURCE})

let append_build_patch = boulder.build_patch {{
    setup = boulder.optional.set "append setup",
    build_deps = boulder.optional.set ["append-build"],
    .. boulder.defaults.build_patch
}}
let base_build = {{
    setup = boulder.optional.set "base setup",
    build_deps = ["base-build"],
    check_deps = ["base-check"],
    .. boulder.defaults.build
}}
let build = boulder.compose.build.append append_build_patch base_build

let override_build_patch = boulder.build_patch {{
    check_deps = boulder.optional.set ["only-check"],
    .. boulder.defaults.build_patch
}}
let build = boulder.compose.build.override override_build_patch build

let prepend_package_patch = boulder.package_patch {{
    conflicts = boulder.optional.set ["prepend-conflict"],
    .. boulder.defaults.package_patch
}}
let base_package = {{
    run_deps = ["base-run"],
    conflicts = ["base-conflict"],
    .. boulder.defaults.package
}}
let package = boulder.compose.package.prepend prepend_package_patch base_package

let append_package_patch = boulder.package_patch {{
    run_deps = boulder.optional.set ["append-run"],
    .. boulder.defaults.package_patch
}}
let package = boulder.compose.package.append append_package_patch package

let prepend_profile_patch = boulder.build_patch {{
    environment = boulder.optional.set "profile prepend",
    .. boulder.defaults.build_patch
}}
let base_profile = {{
    environment = boulder.optional.set "profile base",
    .. boulder.defaults.build
}}
let profile = boulder.compose.build.prepend prepend_profile_patch base_profile

let override_subpackage_patch = boulder.package_patch {{
    run_deps = boulder.optional.set ["only-subpackage-run"],
    .. boulder.defaults.package_patch
}}
let base_subpackage = {{
    run_deps = ["old-subpackage-run"],
    .. boulder.defaults.package
}}
let sub_package = boulder.compose.package.override override_subpackage_patch base_subpackage

{{
    build,
    package,
    profiles = [boulder.named "emul32" profile],
    sub_packages = [boulder.named "example-devel" sub_package],
    .. base
}}
"#
    ));

    let recipe = evaluate_gluon(&source).unwrap().recipe;

    assert_eq!(recipe.build.setup.as_deref(), Some("base setup\nappend setup"));
    assert_eq!(recipe.build.build_deps, ["base-build", "append-build"]);
    assert_eq!(recipe.build.check_deps, ["only-check"]);
    assert_eq!(recipe.package.run_deps, ["base-run", "append-run"]);
    assert_eq!(recipe.package.conflicts, ["prepend-conflict", "base-conflict"]);
    assert_eq!(recipe.profiles[0].key, "emul32");
    assert_eq!(
        recipe.profiles[0].value.environment.as_deref(),
        Some("profile prepend\nprofile base")
    );
    assert_eq!(recipe.sub_packages[0].key, "example-devel");
    assert_eq!(recipe.sub_packages[0].value.run_deps, ["only-subpackage-run"]);
}

#[test]
fn invalid_url_and_release_are_conversion_errors() {
    let invalid_url = authored(&format!(
        r#"
let base = boulder.recipe (boulder.source {SOURCE})
{{ upstreams = [boulder.upstream.git "not a URL" "main"], .. base }}
"#
    ));
    let error = evaluate_gluon(&invalid_url).unwrap_err();
    assert!(matches!(
        error,
        RecipeEvaluationError::Conversion(ref error) if error.field() == "upstreams[0].url"
    ));

    let invalid_release = authored(
        r#"
boulder.recipe (boulder.source {
    name = "example",
    version = "1.2.3",
    release = 0,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
})
"#,
    );
    let error = evaluate_gluon(&invalid_release).unwrap_err();
    assert!(matches!(
        error,
        RecipeEvaluationError::Conversion(ref error) if error.field() == "source.release"
    ));
}

#[test]
fn negative_release_reports_its_field_path() {
    let source = authored(
        r#"
boulder.recipe (boulder.source {
    name = "example",
    version = "1.2.3",
    release = -1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
})
"#,
    );

    let error = evaluate_gluon(&source).unwrap_err();

    assert!(matches!(
        error,
        RecipeEvaluationError::Conversion(ref error) if error.field() == "source.release"
    ));
}

#[test]
fn archive_strip_dirs_checks_both_integer_bounds() {
    for value in [-1, 256] {
        let source = authored(&format!(
            r#"
let base = boulder.recipe (boulder.source {SOURCE})
{{
    upstreams = [boulder.upstream.archive_with {{
        url = "https://example.com/source.tar.xz",
        hash = "archive-hash",
        rename = boulder.optional.unset,
        strip_dirs = boulder.optional.set {value},
        unpack = boulder.boolean.true,
        unpack_dir = boulder.optional.unset,
    }}],
    .. base
}}
"#
        ));

        let error = evaluate_gluon(&source).unwrap_err();

        assert!(matches!(
            error,
            RecipeEvaluationError::Conversion(ref error) if error.field() == "upstreams[0].strip_dirs"
        ));
    }
}

#[test]
fn invalid_types_unknown_fields_and_path_variants_are_type_errors() {
    for body in [
        r#"
boulder.recipe (boulder.source {
    name = "example", version = "1.2.3", release = 1,
    homepage = 42, license = ["MPL-2.0"],
})
"#,
        r#"
boulder.recipe (boulder.source {
    name = "example", version = "1.2.3", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
    home_page = "misspelled",
})
"#,
        r#"
let base = boulder.recipe (boulder.source {
    name = "example", version = "1.2.3", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
{ package = { paths = [Directory { path = "/tmp" }], .. boulder.defaults.package }, .. base }
"#,
    ] {
        let error = evaluate_gluon(&authored(body)).unwrap_err();
        assert!(matches!(
            error,
            RecipeEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type
        ));
    }
}

#[test]
fn evaluation_fingerprint_is_deterministic_and_binds_explicit_inputs() {
    let source = authored(&format!(
        "let abi_version: Int = boulder.abi_version\nboulder.recipe (boulder.source {SOURCE})"
    ));
    let evaluator = Evaluator::default();

    let first = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let repeated = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let changed = evaluate_gluon_with_inputs(&evaluator, &source, b"lock-v2").unwrap();

    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
    assert_eq!(RECIPE_ABI_VERSION, 1);
    assert_eq!(first.fingerprint.configuration_abi_version, RECIPE_ABI_VERSION);
    assert_eq!(
        first
            .fingerprint
            .imported_modules
            .iter()
            .map(|module| module.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["boulder.recipe.v1", "std.array.prim", "std.string.prim", "std.types"]
    );
}
