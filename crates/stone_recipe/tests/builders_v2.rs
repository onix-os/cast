use declarative_config::Source;
use stone_recipe::package::{
    AuthoredPackage, BuilderRequest, DependencySpec, LuaPackageEvaluator, HooksSpec, MetaSpec,
    ProgramSpec, StepSpec, lower,
};

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

/// A hand-authored, minimal Gluon recipe — no `import! cast.package.v3`, no
/// expanded builder, no output/option/hook tables — decodes through the shared
/// [`lower`] into a complete [`PackageSpec`]. Gluon records are structurally
/// typed, so the recipe names every field and selects Rust defaults with
/// `unset`; the builder is named by kind via the `cast.authored.v1` prelude.
/// This is the Gluon half of the independence proof: authoring runs entirely in
/// shared Rust, with no `package.glu` logic involved.
#[test]
fn a_minimal_authored_gluon_recipe_lowers_through_shared_rust() {
    let source = Source::new(
        "stone.glu",
        r#"let a = import! cast.authored.v1
{
    meta = {
        pname = "hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/hello",
        license = ["MIT"],
    },
    builder = a.builder.cmake { flags = ["-DBUILD_TESTS=ON"], run_tests = a.true },
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

    let package = LuaPackageEvaluator::default()
        .evaluate_authored(&source)
        .expect("minimal authored recipe lowers");

    // The builder request lowered to typed cmake steps, checks on by default.
    assert_eq!(
        package.builder.phases.setup.steps,
        vec![StepSpec::CMakeConfigure { flags: vec!["-DBUILD_TESTS=ON".to_owned()] }]
    );
    assert_eq!(package.builder.phases.check.steps, vec![StepSpec::CMakeTest]);
    // Every `unset` optional took the shared package-ABI default.
    assert_eq!(package.outputs.len(), 9);
    assert_eq!(package.outputs[0].name, "out");
    assert_eq!(package.hooks, HooksSpec::default());

    // The engine path is exactly the shared lowering of the equivalent
    // language-agnostic authored package — the Gluon record is pure syntax.
    let equivalent = lower(AuthoredPackage {
        meta: MetaSpec {
            pname: "hello".to_owned(),
            version: "1.0.0".to_owned(),
            release: 1,
            homepage: "https://example.invalid/hello".to_owned(),
            license: vec!["MIT".to_owned()],
        },
        builder: BuilderRequest::Cmake {
            flags: vec!["-DBUILD_TESTS=ON".to_owned()],
            run_tests: true,
        },
        sources: Vec::new(),
        native_build_inputs: Vec::new(),
        build_inputs: Vec::new(),
        check_inputs: Vec::new(),
        outputs: None,
        options: None,
        profiles: Vec::new(),
        architectures: Vec::new(),
        tuning: Vec::new(),
        emul32: false,
        mold: false,
        hooks: HooksSpec::default(),
    });
    assert_eq!(package, equivalent);
}

/// A richer authored Gluon recipe — a custom shell builder, a dependency, a
/// source, a build hook, and an explicit output override — decodes through the
/// shared `lower`. This proves the `cast.authored.v1` prelude expresses the
/// corpus's real feature surface (not just the trivial default case) with no
/// `package.glu` involvement: the `Custom` builder passes through untouched, the
/// authored outputs replace the default set, and hooks stay distinct from the
/// builder phases.
#[test]
fn a_rich_authored_gluon_recipe_lowers_custom_builder_deps_and_hooks() {
    let source = Source::new(
        "stone.glu",
        r#"let a = import! cast.authored.v1
let scripts = a.scripts {
    setup = a.phase [],
    build = a.phase [a.step.shell "zig build"],
    install = a.phase [a.step.shell "zig build install"],
    check = a.phase [],
    workload = a.phase [],
}
{
    meta = {
        pname = "hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/hello",
        license = ["MIT"],
    },
    builder = a.builder.shell scripts [a.dep.binary "zig"],
    sources = [a.source.archive "https://example.invalid/hello-1.0.0.tar.gz" "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"],
    native_build_inputs = [],
    build_inputs = [a.dep.package "zlib"],
    check_inputs = [],
    outputs = a.outputs.explicit [a.output "out"],
    options = a.unset,
    profiles = [],
    architectures = ["x86_64"],
    tuning = [],
    emul32 = a.false,
    mold = a.true,
    hooks = a.some.hooks (a.hooks {
        pre_build = [a.step.run (a.program.binary "prepare") []],
        .. a.empty.hooks
    }),
}
"#,
    );

    let package = LuaPackageEvaluator::default()
        .evaluate_authored(&source)
        .expect("rich authored recipe lowers");

    // Custom builder passed through: no environment, its exact tools + steps.
    assert!(package.builder.environment.is_empty());
    assert_eq!(
        dependency_names(package.builder.required_tools()),
        ["binary(zig)"]
    );
    assert_eq!(
        package.builder.phases.build.steps,
        [shell("zig build", Vec::new())]
    );
    // The authored dependency and source survive.
    assert_eq!(
        package.build_inputs,
        [DependencySpec::Package(stone_recipe::package::PackageRef { name: "zlib".to_owned() })]
    );
    assert_eq!(package.sources.len(), 1);
    // The explicit output override replaces the 9-output default set.
    assert_eq!(package.outputs.len(), 1);
    assert_eq!(package.outputs[0].name, "out");
    // Hooks stay distinct from builder phases.
    assert_eq!(
        package.hooks.pre_build,
        [StepSpec::Run {
            program: binary_program("prepare"),
            args: Vec::new(),
        }]
    );
    assert_eq!(package.architectures, ["x86_64"]);
    assert!(package.mold);
}
