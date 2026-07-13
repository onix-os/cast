// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::Source;
use stone_recipe::package::{DependencySpec, StepSpec, evaluate_gluon};

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

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
fn meson_builder_keeps_policy_tools_out_of_package_defaults() {
    let source = package(
        "boulder.builders.meson.v1",
        r#"builder.builder {
            flags = ["-Ddocumentation=false"],
            run_tests = b.boolean.false,
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert!(evaluated.package.builder.authored_required_tools().is_empty());
    let phases = evaluated.package.phases();
    assert_eq!(
        phases.setup.steps,
        [StepSpec::MesonSetup {
            flags: vec!["-Ddocumentation=false".to_owned()]
        }]
    );
    assert_eq!(phases.build.steps, [StepSpec::MesonBuild]);
    assert_eq!(phases.install.steps, [StepSpec::MesonInstall]);
    assert!(phases.check.is_empty());
    assert!(
        evaluated
            .fingerprint
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "boulder.builders.meson.v1")
    );
}

#[test]
fn cmake_builder_declares_structural_steps() {
    let source = package(
        "boulder.builders.cmake.v1",
        r#"builder.builder {
            flags = ["-DBUILD_SHARED_LIBS=ON"],
            run_tests = b.boolean.true,
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert!(evaluated.package.builder.authored_required_tools().is_empty());
    let phases = evaluated.package.phases();
    assert_eq!(
        phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_SHARED_LIBS=ON".to_owned()]
        }]
    );
    assert_eq!(phases.build.steps, [StepSpec::CMakeBuild]);
    assert_eq!(phases.install.steps, [StepSpec::CMakeInstall]);
    assert_eq!(phases.check.steps, [StepSpec::CMakeTest]);
}

#[test]
fn cargo_builder_declares_features_binaries_environment_and_checks() {
    let source = package(
        "boulder.builders.cargo.v1",
        r#"builder.builder {
            features = ["cli", "tls"],
            binaries = ["example", "examplectl"],
            run_tests = b.boolean.true,
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert!(evaluated.package.builder.authored_required_tools().is_empty());
    let phases = evaluated.package.phases();
    assert_eq!(
        phases.build.steps,
        [StepSpec::CargoBuild {
            features: vec!["cli".to_owned(), "tls".to_owned()]
        }]
    );
    assert_eq!(
        phases.install.steps,
        [StepSpec::CargoInstall {
            binaries: vec!["example".to_owned(), "examplectl".to_owned()]
        }]
    );
    assert!(matches!(phases.check.steps.as_slice(), [StepSpec::CargoTest { .. }]));
    assert_eq!(phases.environment.steps, [StepSpec::CargoEnvironment]);
}

#[test]
fn autotools_builder_declares_structural_phase_contract() {
    let source = package(
        "boulder.builders.autotools.v1",
        r#"builder.builder {
            flags = ["--disable-static"],
            .. builder.defaults
        }"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert!(evaluated.package.builder.authored_required_tools().is_empty());
    let phases = evaluated.package.phases();
    assert_eq!(
        phases.setup.steps,
        [StepSpec::AutotoolsConfigure {
            flags: vec!["--disable-static".to_owned()]
        }]
    );
    assert_eq!(phases.build.steps, [StepSpec::AutotoolsBuild]);
    assert_eq!(phases.check.steps, [StepSpec::AutotoolsTest]);
    assert_eq!(phases.install.steps, [StepSpec::AutotoolsInstall]);
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
    build = b.phase [
        b.step.shell "ZIG_GLOBAL_CACHE_DIR=\"${BOULDER_BUILD_ROOT}/zig-cache\" zig build",
    ],
    install = b.phase [
        b.step.shell "ZIG_GLOBAL_CACHE_DIR=\"${BOULDER_BUILD_ROOT}/zig-cache\" zig build install --prefix \"${BOULDER_INSTALL_ROOT}${BOULDER_PREFIX}\"",
    ],
    .. b.defaults.scripts
}
{
    builder = b.builder.shell scripts [b.dep.binary "zig"],
    hooks = b.hooks {
        pre_build = [b.step.shell "prepare-generated-files"],
        post_build = [b.step.shell "verify-generated-files"],
        .. b.defaults.hooks
    },
    .. base
}
"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(
        dependency_names(evaluated.package.builder.authored_required_tools()),
        ["binary(zig)"]
    );
    let phases = evaluated.package.phases();
    assert_eq!(
        phases.build.steps,
        [
            StepSpec::Shell {
                script: "prepare-generated-files".to_owned()
            },
            StepSpec::Shell {
                script: r#"ZIG_GLOBAL_CACHE_DIR="${BOULDER_BUILD_ROOT}/zig-cache" zig build"#.to_owned()
            },
            StepSpec::Shell {
                script: "verify-generated-files".to_owned()
            }
        ]
    );
    assert_eq!(
        phases.install.steps,
        [StepSpec::Shell {
            script: r#"ZIG_GLOBAL_CACHE_DIR="${BOULDER_BUILD_ROOT}/zig-cache" zig build install --prefix "${BOULDER_INSTALL_ROOT}${BOULDER_PREFIX}""#
                .to_owned()
        }]
    );
    assert!(phases.environment.steps.is_empty());
}
