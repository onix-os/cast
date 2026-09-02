#[path = "package_v3/adapter.rs"]
mod adapter;

use adapter::{
    PackageDeclarationError, evaluate_default_package, evaluate_package_with_inputs,
};
use declarative_config::{DeclarationEvaluationError, DeclarationInputEvaluator, Source};
use declarative_config::DiagnosticCategory;
use stone_recipe::package::{
    BuiltProgramSpec, DependencyKind, DependencyRole, DependencySpec,
    LuaPackageEvaluator, PackageConversionError, ProgramSpec, StepSpec, SupportedHooksSpec,
};

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn authored(body: &str) -> Source {
    Source::new("stone.lua", format!("return {body}"))
}

/// The meta block every authored fixture needs, so bodies name only the field
/// under test.
fn meta(pname: &str) -> String {
    format!(
        r#"meta = {{ pname = "{pname}", version = "1.0.0", release = 1,
            homepage = "https://example.com", license = {{ "MPL-2.0" }} }}"#
    )
}

/// One output record with every option absent.
fn output(name: &str, include_in_manifest: bool) -> String {
    format!(
        r#"{{ name = "{name}", include_in_manifest = {include_in_manifest},
           summary = {{ kind = "none" }}, description = {{ kind = "none" }},
           provides_exclude = {{}}, runtime_inputs = {{}}, runtime_exclude = {{}},
           paths = {{}}, conflicts = {{}} }}"#
    )
}

/// A custom builder whose named phase carries `steps` and which requires
/// `tools`; every other phase is empty.
fn custom_builder_with_tools(tools: &str, phase: &str, steps: &str) -> String {
    let phases = ["setup", "build", "install", "check", "workload"]
        .into_iter()
        .map(|name| {
            let body = if name == phase { steps } else { "" };
            format!("{name} = {{ steps = {{ {body} }} }}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{ kind = \"custom\", spec = {{ required_tools = {{ {tools} }}, environment = {{}}, \
         phases = {{ {phases} }}, \
         supported_hooks = {{ setup = false, build = false, check = false, install = false, workload = false }} }} }}"
    )
}

/// A custom builder whose named phase carries `steps`; every other phase empty.
fn custom_builder(phase: &str, steps: &str) -> String {
    let phases = ["setup", "build", "install", "check", "workload"]
        .into_iter()
        .map(|name| {
            let body = if name == phase { steps } else { "" };
            format!("{name} = {{ steps = {{ {body} }} }}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{ kind = \"custom\", spec = {{ required_tools = {{}}, environment = {{}}, phases = {{ {phases} }}, \
         supported_hooks = {{ setup = false, build = false, check = false, install = false, workload = false }} }} }}"
    )
}

/// A complete minimal authored recipe: the given `pname`, an empty custom
/// builder, and whatever extra fields the caller appends.
fn authored_package(pname: &str, extra: &str) -> Source {
    authored(&format!(
        "{{ {}, builder = {}{} }}",
        meta(pname),
        custom_builder("", ""),
        extra,
    ))
}

/// A recipe whose named phase carries one step, for step-level rejection tests.
fn package_with_step(pname: &str, phase: &str, step: &str) -> Source {
    authored(&format!(
        "{{ {}, builder = {}, outputs = {{ {} }} }}",
        meta(pname),
        custom_builder(phase, step),
        output("out", true),
    ))
}

/// A typed dependency literal. `package` and `output` carry a reference table;
/// every other kind carries a bare string.
fn dep(kind: &str, value: &str) -> String {
    match kind {
        "package" => format!(r#"{{ kind = "package", value = {{ name = "{value}" }} }}"#),
        _ => format!(r#"{{ kind = "{kind}", value = "{value}" }}"#),
    }
}

/// An `output` dependency naming one output of another package.
fn output_dep(package: &str, output: &str) -> String {
    format!(
        r#"{{ kind = "output", value = {{ package = {{ name = "{package}" }}, output = "{output}" }} }}"#
    )
}

/// One output record, overriding the listed fields; the rest stay absent/empty.
fn output_with(name: &str, overrides: &[(&str, &str)]) -> String {
    let mut fields: Vec<(&str, String)> = vec![
        ("include_in_manifest", "true".to_owned()),
        ("summary", r#"{ kind = "none" }"#.to_owned()),
        ("description", r#"{ kind = "none" }"#.to_owned()),
        ("provides_exclude", "{}".to_owned()),
        ("runtime_inputs", "{}".to_owned()),
        ("runtime_exclude", "{}".to_owned()),
        ("paths", "{}".to_owned()),
        ("conflicts", "{}".to_owned()),
    ];
    for (key, value) in overrides {
        if let Some(slot) = fields.iter_mut().find(|(field, _)| field == key) {
            slot.1 = (*value).to_owned();
        }
    }
    let body = fields
        .into_iter()
        .map(|(field, value)| format!("{field} = {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(r#"{{ name = "{name}", {body} }}"#)
}

/// A complete, already-lowered package spec. The input-evaluator path decodes
/// the frozen domain directly, so it names every field.
fn complete_package(pname: &str) -> Source {
    authored(&format!(
        "{{ {}, builder = {}, hooks = {}, outputs = {{ {} }} }}",
        meta(pname),
        custom_builder("", ""),
        empty_hooks(),
        output("out", true),
    ))
}

/// An archive upstream with the given hash and optional rename/unpack_dir.
fn archive(hash: &str, rename: &str, unpack_dir: &str) -> String {
    format!(
        r#"{{ kind = "archive", url = "https://example.com/source.tar.xz", hash = "{hash}",
           rename = {rename}, strip_dirs = {{ kind = "none" }}, unpack = true,
           unpack_dir = {unpack_dir} }}"#
    )
}

/// Every hook slot empty.
fn empty_hooks() -> String {
    let slots = ["setup", "build", "check", "install", "workload"]
        .into_iter()
        .flat_map(|phase| [format!("pre_{phase} = {{}}"), format!("post_{phase} = {{}}")])
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{ {slots} }}")
}

/// A profile whose builder requires `tools` and which carries the given typed
/// dependency lists. Every phase is empty; hooks are supported but unused.
fn profile(name: &str, tools: &str, native: &str, build: &str, check: &str) -> String {
    let phases = ["setup", "build", "install", "check", "workload"]
        .into_iter()
        .map(|phase| format!("{phase} = {{ steps = {{}} }}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{ name = \"{name}\", builder = {{ required_tools = {{ {tools} }}, environment = {{}}, \
         phases = {{ {phases} }}, supported_hooks = {{ setup = true, build = true, check = true, \
         install = true, workload = true }} }}, hooks = {}, native_build_inputs = {{ {native} }}, \
         build_inputs = {{ {build} }}, check_inputs = {{ {check} }} }}",
        empty_hooks(),
    )
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
fn frozen_package_abi_has_no_cargo_fetch_escape_hatch() {
    let error = evaluate_default_package(&authored(&format!(
        "{{ {}, builder = {{ kind = \"custom\", spec = {{ required_tools = {{}}, environment = {{}}, \
         phases = {{ setup = {{ steps = {{}} }}, build = {{ steps = {{ {{ kind = \"cargo_fetch\" }} }} }}, \
         install = {{ steps = {{}} }}, check = {{ steps = {{}} }}, workload = {{ steps = {{}} }} }}, \
         supported_hooks = {{ setup = false, build = false, check = false, install = false, workload = false }} }} }} }}",
        meta("no-cargo-fetch"),
    )))
    .unwrap_err();

    assert!(
        error.to_string().contains("cargo_fetch"),
        "the frozen ABI must reject a cargo_fetch step by name: {error}"
    );
}

#[test]
fn manifest_membership_is_explicit_not_inferred_from_package_name() {
    let source = authored_package(
        "symbols-dbginfo",
        &format!(", outputs = {{ {} }}", output("out", true)),
    );

    let evaluated = evaluate_default_package(&source).unwrap();
    assert_eq!(evaluated.value.outputs.len(), 1);
    assert!(evaluated.value.outputs[0].include_in_manifest);
}

#[test]
fn external_built_and_shell_steps_preserve_distinct_program_authority() {
    let build_steps = r#"
        { kind = "run",
          program = { path = "/opt/odd/bin/tool", requirement = { kind = "package", value = { name = "odd-tool" } } },
          args = { "--frozen" } },
        { kind = "run_built",
          program = { path = "build/generated-tool" },
          args = { "--self-test" } },
        { kind = "shell",
          interpreter = { path = "/usr/bin/dash", requirement = { kind = "binary", value = "dash" } },
          declared_programs = {
              { path = "/opt/odd/bin/helper", requirement = { kind = "package", value = { name = "odd-tool" } } },
          },
          script = "helper --check" },
        { kind = "shell",
          interpreter = { path = "/usr/bin/bash", requirement = { kind = "binary", value = "bash" } },
          declared_programs = {},
          script = "echo builtin" }
    "#;
    let source = authored(&format!(
        "{{ {}, builder = {{ kind = \"custom\", spec = {{ required_tools = {{}}, environment = {{}}, \
         phases = {{ setup = {{ steps = {{}} }}, build = {{ steps = {{ {build_steps} }} }}, \
         install = {{ steps = {{}} }}, check = {{ steps = {{}} }}, workload = {{ steps = {{}} }} }}, \
         supported_hooks = {{ setup = false, build = false, check = false, install = false, workload = false }} }} }}, \
         outputs = {{ {} }} }}",
        meta("example"),
        output("out", true),
    ));

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
        let source = package_with_step(
            "example",
            "check",
            &format!(r#"{{ kind = "run_built", program = {{ path = {invalid:?} }}, args = {{}} }}"#),
        );
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
    for (path, requirement, expected_field) in [
        ("tool", dep("binary", "tool"), "builder.phases.build.steps[0].program.path"),
        (
            "/usr/bin/other",
            dep("binary", "tool"),
            "builder.phases.build.steps[0].program.path",
        ),
        (
            "/usr/bin/pkg-config",
            dep("pkg_config", "example"),
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            "/usr/bin/nested/tool",
            dep("binary", "nested/tool"),
            "builder.phases.build.steps[0].program.requirement",
        ),
        (
            "/usr/bin/tool",
            dep("package", "tool-package"),
            "builder.phases.build.steps[0].program.path",
        ),
    ] {
        let source = package_with_step(
            "example",
            "build",
            &format!(
                r#"{{ kind = "run", program = {{ path = "{path}", requirement = {requirement} }}, args = {{}} }}"#
            ),
        );

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

/// A recipe that omits a required field is a schema type error, not a silently
/// defaulted package. Only the optional fields named by the authored ABI may be
/// absent; `meta` is not one of them.
#[test]
fn a_missing_required_field_is_a_schema_type_error() {
    let source = authored(&format!("{{ builder = {} }}", custom_builder("", "")));

    let error = evaluate_default_package(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref error)
            if error.category == DiagnosticCategory::Type
    ));
}

#[test]
fn evaluator_accepts_typed_kinds_in_ordinary_dependency_roles() {
    let runtime_inputs = [
        dep("package", "runtime-package"),
        output_dep("runtime-suite", "runtime"),
        dep("binary", "runtime-binary"),
        dep("system_binary", "runtime-system-binary"),
        dep("soname", "libruntime.so.1"),
        dep("python", "runtime_python"),
        dep("interpreter", "/usr/lib/ld-runtime.so.1(x86_64)"),
    ]
    .join(", ");
    let root = output_with(
        "out",
        &[
            ("runtime_inputs", &format!("{{ {runtime_inputs} }}")),
            (
                "conflicts",
                &format!("{{ {} }}", dep("pkg_config32", "conflicting-devel")),
            ),
        ],
    );
    let tools = [
        dep("package", "tool-package"),
        output_dep("tool-suite", "tools"),
        dep("binary", "tool-binary"),
        dep("system_binary", "tool-system-binary"),
    ]
    .join(", ");
    let source = authored(&format!(
        "{{ {}, builder = {}, native_build_inputs = {{ {} }}, \
         build_inputs = {{ {}, {} }}, check_inputs = {{ {}, {} }}, outputs = {{ {root} }} }}",
        meta("ordinary-roles"),
        custom_builder_with_tools(&tools, "", ""),
        dep("cmake", "NativeConfig"),
        dep("pkg_config", "target-devel"),
        dep("pkg_config32", "target-devel"),
        dep("soname", "libcheck.so.1"),
        dep("interpreter", "/usr/lib/ld-check.so.1(x86_64)"),
    ));

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
    let runtime_mismatch = |dependency: String| {
        format!(
            "builder = {}, outputs = {{ {} }}",
            custom_builder("", ""),
            output_with("out", &[("runtime_inputs", &format!("{{ {dependency} }}"))]),
        )
    };
    for (field, declaration, role, kind) in [
        (
            "builder.required_tools[0]",
            format!(
                "builder = {}",
                custom_builder_with_tools(&dep("soname", "libtool.so.1"), "", "")
            ),
            DependencyRole::BuilderTool,
            DependencyKind::Soname,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            runtime_mismatch(dep("cmake", "RuntimeConfig")),
            DependencyRole::Runtime,
            DependencyKind::CMake,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            runtime_mismatch(dep("pkg_config", "runtime-devel")),
            DependencyRole::Runtime,
            DependencyKind::PkgConfig,
        ),
        (
            "outputs[0].runtime_inputs[0]",
            runtime_mismatch(dep("pkg_config32", "runtime-devel")),
            DependencyRole::Runtime,
            DependencyKind::PkgConfig32,
        ),
    ] {
        let source = authored(&format!("{{ {}, {declaration} }}", meta("ordinary-role-error")));

        assert_dependency_role_conversion_error(evaluate_default_package(&source).unwrap_err(), field, role, kind);
    }
}

#[test]
fn evaluator_accepts_typed_kinds_in_a_selected_profile() {
    let tools = [
        dep("package", "profile-tool-package"),
        output_dep("profile-tool-suite", "tools"),
        dep("binary", "profile-tool-binary"),
        dep("system_binary", "profile-tool-system-binary"),
    ]
    .join(", ");
    let selected = profile(
        "emul32/x86_64",
        &tools,
        &dep("cmake", "ProfileNativeConfig"),
        &format!(
            "{}, {}",
            dep("pkg_config", "profile-devel"),
            dep("pkg_config32", "profile-devel")
        ),
        &format!(
            "{}, {}",
            dep("soname", "libprofile-check.so.1"),
            dep("interpreter", "/usr/lib/ld-profile-check.so.1(x86_64)")
        ),
    );
    let source = authored(&format!(
        "{{ {}, builder = {}, outputs = {{ {} }}, profiles = {{ {selected} }} }}",
        meta("profile-roles"),
        custom_builder("", ""),
        output("out", true),
    ));

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
        (dep("pkg_config32", "profile-devel"), DependencyKind::PkgConfig32),
        (
            dep("interpreter", "/usr/lib/ld-profile.so.1(x86_64)"),
            DependencyKind::Interpreter,
        ),
    ] {
        let selected = profile("emul32/x86_64", &dependency, "", "", "");
        let source = authored(&format!(
            "{{ {}, builder = {}, outputs = {{ {} }}, profiles = {{ {selected} }} }}",
            meta("profile-role-error"),
            custom_builder("", ""),
            output("out", true),
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
    let root = output_with(
        "out",
        &[(
            "runtime_inputs",
            &format!("{{ {} }}", output_dep("example", "missing")),
        )],
    );
    let source = authored(&format!(
        "{{ {}, builder = {}, outputs = {{ {root} }} }}",
        meta("example"),
        custom_builder("", ""),
    ));

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
    let source = authored(&format!(
        r#"{{ meta = {{ pname = "example", version = "v1.0.0", release = 1,
            homepage = "https://example.com", license = {{ "MPL-2.0" }} }}, builder = {} }}"#,
        custom_builder("", ""),
    ));

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
            archive("short", "{ kind = \"none\" }", "{ kind = \"none\" }"),
            "sources[0].hash",
            "64 lowercase ASCII hexadecimal",
        ),
        (
            archive(&"a".repeat(64), r#"{ kind = "some", value = "../escape" }"#, "{ kind = \"none\" }"),
            "sources[0].rename",
            "normalized filename component",
        ),
        (
            archive(
                &"a".repeat(64),
                "{ kind = \"none\" }",
                r#"{ kind = "some", value = "../../escape" }"#,
            ),
            "sources[0].unpack_dir",
            "normalized, non-empty relative path",
        ),
        (
            r#"{ kind = "git", url = "https://example.com/source.git", git_ref = "",
               clone_dir = { kind = "none" } }"#
                .to_owned(),
            "sources[0].git_ref",
            "must be non-empty",
        ),
    ] {
        let source = authored(&format!(
            "{{ {}, builder = {}, sources = {{ {source} }} }}",
            meta("example"),
            custom_builder("", ""),
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
            r#"{{ meta = {{ pname = {pname:?}, version = {version:?}, release = 1,
               homepage = "https://example.com", license = {{ "MPL-2.0" }} }}, builder = {} }}"#,
            custom_builder("", ""),
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
    let with_profiles = |names: &[&str]| {
        let profiles = names
            .iter()
            .map(|name| profile(name, "", "", "", ""))
            .collect::<Vec<_>>()
            .join(", ");
        authored(&format!(
            "{{ {}, builder = {}, profiles = {{ {profiles} }} }}",
            meta("example"),
            custom_builder("", ""),
        ))
    };
    let unsafe_profile = with_profiles(&["emul32/../x86_64"]);

    let error = evaluate_default_package(&unsafe_profile).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(PackageConversionError::InvalidProfileName {
            index: 0,
            ref name,
        }) if name == "emul32/../x86_64"
    ));

    let duplicate_profiles = with_profiles(&["native", "emul32/x86_64", "native"]);

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
    let source = authored(&format!(
        r#"{{ {}, builder = {}, options = {{ toolchain = "llvm", cspgo = false,
           samplepgo = false, debug = true, strip = true, networking = true,
           compressman = false, lastrip = true }} }}"#,
        meta("example"),
        custom_builder("", ""),
    ));

    let error = evaluate_default_package(&source).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(PackageConversionError::FrozenBuildNetworkingUnsupported)
    ));
    assert!(error.to_string().contains("locked sources"));
}

#[test]
fn evaluator_keeps_special_constructor_reserved_but_rejects_concrete_package_use() {
    let root = output_with(
        "out",
        &[(
            "paths",
            r#"{ { kind = "special", path = "/usr/lib/example/events.fifo" } }"#,
        )],
    );
    let source = authored(&format!(
        "{{ {}, builder = {}, outputs = {{ {root} }} }}",
        meta("example"),
        custom_builder("", ""),
    ));

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
    let source = complete_package("example");
    let evaluator = LuaPackageEvaluator::default();

    let first = evaluate_package_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let repeated = evaluate_package_with_inputs(&evaluator, &source, b"lock-v1").unwrap();
    let changed = evaluate_package_with_inputs(&evaluator, &source, b"lock-v2").unwrap();
    let typed = <LuaPackageEvaluator as DeclarationInputEvaluator<PackageSpec>>::evaluate_with_inputs(
        &LuaPackageEvaluator::default(),
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
