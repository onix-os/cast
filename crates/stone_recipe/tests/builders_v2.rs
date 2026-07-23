use declarative_config::{DeclarationEvaluationError, DeclarationEvaluator, Evaluation, Source};
use gluon_config::EvaluationIdentity;
use stone_recipe::package::{
    BuilderEnvironmentSpec, DependencySpec, GluonPackageEvaluator, PackageConversionError,
    PackageSpec, ProgramSpec, StepSpec, SupportedHooksSpec,
};

fn evaluate_package(
    source: &Source,
) -> Result<
    Evaluation<PackageSpec, EvaluationIdentity>,
    DeclarationEvaluationError<PackageConversionError>,
> {
    DeclarationEvaluator::<PackageSpec>::evaluate(&GluonPackageEvaluator::default(), source)
}

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn binary_program(name: &str) -> ProgramSpec {
    ProgramSpec {
        path: format!("/usr/bin/{name}"),
        requirement: DependencySpec::Binary(name.to_owned()),
    }
}

fn shell(script: &str, declared_programs: Vec<ProgramSpec>) -> StepSpec {
    StepSpec::Shell {
        interpreter: binary_program("bash"),
        declared_programs,
        script: script.to_owned(),
    }
}

fn package(builder_import: &str, body: &str) -> Source {
    Source::new(
        "stone.glu",
        format!(
            r#"let b = import! cast.package.v3
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
fn meson_builder_returns_tools_environment_phases_and_hooks() {
    let source = package(
        "cast.builders.meson.v2",
        r#"builder.builder {
            flags = ["-Ddocumentation=false"],
            run_tests = b.boolean.false,
        }"#,
    );
    let evaluated = evaluate_package(&source).unwrap();

    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        [
            "binary(cmake)",
            "binary(sh)",
            "binary(ninja)",
            "binary(pkgconf)",
        ]
    );
    assert_eq!(evaluated.value.builder.environment, [BuilderEnvironmentSpec::Meson]);
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    let phases = evaluated.value.phases();
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
            .identity
            .modules
            .iter()
            .any(|module| module.logical_name == "cast.builders.meson.v2")
    );
}

#[test]
fn cmake_builder_returns_tools_environment_phases_and_hooks() {
    let source = package(
        "cast.builders.cmake.v2",
        r#"builder.builder {
            flags = ["-DBUILD_SHARED_LIBS=ON"],
            run_tests = b.boolean.true,
        }"#,
    );
    let evaluated = evaluate_package(&source).unwrap();

    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        ["binary(sh)", "binary(ninja)"]
    );
    assert_eq!(evaluated.value.builder.environment, [BuilderEnvironmentSpec::CMake]);
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    let phases = evaluated.value.phases();
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
        "cast.builders.cargo.v2",
        r#"builder.builder {
            features = ["cli", "tls"],
            binaries = ["example", "examplectl"],
            run_tests = b.boolean.true,
        }"#,
    );
    let evaluated = evaluate_package(&source).unwrap();

    assert!(evaluated.value.builder.required_tools().is_empty());
    assert_eq!(evaluated.value.builder.environment, [BuilderEnvironmentSpec::Cargo]);
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    let phases = evaluated.value.phases();
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
}

#[test]
fn autotools_builder_declares_structural_phase_contract() {
    let source = package(
        "cast.builders.autotools.v2",
        r#"builder.builder {
            flags = ["--disable-static"],
            .. builder.defaults
        }"#,
    );
    let evaluated = evaluate_package(&source).unwrap();

    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        [
            "binary(autoconf)",
            "binary(automake)",
            "binary(awk)",
            "binary(grep)",
            "binary(install)",
            "binary(sed)",
        ]
    );
    assert_eq!(
        evaluated.value.builder.environment,
        [BuilderEnvironmentSpec::Autotools]
    );
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    let phases = evaluated.value.phases();
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
        r#"let b = import! cast.package.v3
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.com", license = ["MPL-2.0"],
})
let scripts = b.scripts {
    build = b.phase [
        b.step.shell_with {
            interpreter = b.program.binary "bash",
            declared_programs = [b.program.binary "zig"],
            script = "ZIG_GLOBAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-cache\" zig build",
        },
    ],
    install = b.phase [
        b.step.shell_with {
            interpreter = b.program.binary "bash",
            declared_programs = [b.program.binary "zig"],
            script = "ZIG_GLOBAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-cache\" zig build install --prefix \"${CAST_INSTALL_ROOT}${CAST_PREFIX}\"",
        },
    ],
    .. b.defaults.scripts
}
{
    builder = b.builder.shell scripts [b.dep.binary "zig"],
    hooks = b.hooks {
        pre_build = [b.step.run (b.program.binary "prepare-generated-files") []],
        post_build = [b.step.run (b.program.binary "verify-generated-files") []],
        .. b.defaults.hooks
    },
    .. base
}
"#,
    );
    let evaluated = evaluate_package(&source).unwrap();

    assert_eq!(
        dependency_names(evaluated.value.builder.required_tools()),
        ["binary(zig)"]
    );
    assert!(evaluated.value.builder.environment.is_empty());
    assert_eq!(evaluated.value.builder.supported_hooks, SupportedHooksSpec::all());
    let phases = evaluated.value.phases();
    assert_eq!(
        phases.build.steps,
        [
            StepSpec::Run {
                program: binary_program("prepare-generated-files"),
                args: Vec::new(),
            },
            shell(
                r#"ZIG_GLOBAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-cache" zig build"#,
                vec![binary_program("zig")],
            ),
            StepSpec::Run {
                program: binary_program("verify-generated-files"),
                args: Vec::new(),
            }
        ]
    );
    assert_eq!(
        phases.install.steps,
        [shell(
            r#"ZIG_GLOBAL_CACHE_DIR="${CAST_BUILD_ROOT}/zig-cache" zig build install --prefix "${CAST_INSTALL_ROOT}${CAST_PREFIX}""#,
            vec![binary_program("zig")],
        )]
    );
}
