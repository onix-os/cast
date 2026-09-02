    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("package.lua", source))
            .expect("lua value decodes")
            .value
    }

    fn empty_phases() -> String {
        "{ setup = { steps = {} }, build = { steps = {} }, install = { steps = {} }, \
         check = { steps = {} }, workload = { steps = {} } }"
            .to_owned()
    }
    fn empty_hooks() -> String {
        "{ pre_setup = {}, post_setup = {}, pre_build = {}, post_build = {}, pre_check = {}, \
         post_check = {}, pre_install = {}, post_install = {}, pre_workload = {}, post_workload = {} }"
            .to_owned()
    }
    fn builder() -> String {
        format!(
            "{{ required_tools = {{}}, environment = {{ \"cmake\" }}, phases = {}, \
             supported_hooks = {{ setup = true, build = true, check = true, install = true, workload = true }} }}",
            empty_phases()
        )
    }
    fn options() -> String {
        "{ toolchain = \"llvm\", cspgo = false, samplepgo = false, debug = false, strip = true, \
         networking = false, compressman = true, lastrip = false }"
            .to_owned()
    }

    fn complete_recipe_source() -> String {
        let output = r#"{ name = "out", include_in_manifest = true, summary = { kind = "none" },
            description = { kind = "none" }, provides_exclude = {}, runtime_inputs = {},
            runtime_exclude = {}, paths = {}, conflicts = {} }"#;
        format!(
            "return {{\n\
             meta = {{ pname = \"hello\", version = \"1.0\", release = 1, homepage = \"https://x\", license = {{ \"MIT\" }} }},\n\
             builder = {b},\n\
             hooks = {h},\n\
             native_build_inputs = {{}},\n\
             build_inputs = {{ {{ kind = \"binary\", value = \"cc\" }} }},\n\
             check_inputs = {{}},\n\
             outputs = {{ {output} }},\n\
             options = {o},\n\
             profiles = {{}},\n\
             sources = {{ {{ kind = \"git\", url = \"https://x/g.git\", git_ref = \"main\", clone_dir = {{ kind = \"none\" }} }} }},\n\
             architectures = {{ \"x86_64\" }},\n\
             tuning = {{ {{ key = \"lto\", value = {{ kind = \"enable\" }} }} }},\n\
             emul32 = false,\n\
             mold = true,\n\
             }}",
            b = builder(),
            h = empty_hooks(),
            o = options(),
        )
    }

    #[test]
    fn equivalent_replacements_compare_equal_and_divergent_ones_do_not() {
        let source = complete_recipe_source();
        let original = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &source))
            .expect("recipe decodes");
        // A byte-identical replacement normalizes to the same spec.
        let replacement = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &source))
            .expect("recipe decodes");
        assert!(recipe_is_equivalent_replacement(&original, &replacement));

        // A replacement that changes a field is not an equivalent migration.
        let mut divergent = replacement.clone();
        divergent.mold = !divergent.mold;
        assert!(!recipe_is_equivalent_replacement(&original, &divergent));
    }

    #[test]
    fn recipe_explicit_inputs_bind_into_the_identity() {
        use declarative_config::{DeclarationInputEvaluator, EvaluationDeadline};

        let source = complete_recipe_source();
        let evaluator = LuaPackageEvaluator::default();
        let deadline = || EvaluationDeadline::start(evaluator.limits().timeout);
        let src = Source::new("stone.lua", &source);

        // The source lock is bound as explicit inputs, so recipes evaluated with
        // different locks commit to distinct identities even for equal values.
        let none = evaluator.evaluate_with_inputs_within(&src, &[], deadline()).unwrap();
        let locked = evaluator.evaluate_with_inputs_within(&src, b"lock-bytes", deadline()).unwrap();
        assert_eq!(none.value, locked.value);
        assert_ne!(
            none.identity.explicit_inputs_sha256,
            locked.identity.explicit_inputs_sha256
        );
    }

    #[test]
    fn the_migration_gate_authorizes_equivalents_and_rejects_mismatches() {
        let source = complete_recipe_source();
        let authored = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &source))
            .expect("recipe decodes");
        let replacement = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &source))
            .expect("recipe decodes");
        assert_eq!(
            authorize_recipe_migration(&authored, &replacement),
            RecipeMigrationDecision::Authorized
        );

        let mut divergent = replacement.clone();
        divergent.meta.pname = "different".to_owned();
        assert_eq!(
            authorize_recipe_migration(&authored, &divergent),
            RecipeMigrationDecision::Rejected
        );
    }

    #[test]
    fn an_emitted_recipe_re_decodes_to_the_same_package() {
        let original = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &complete_recipe_source()))
            .expect("recipe decodes");
        let emitted = encode_lua_recipe(&original);
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let round_tripped = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &emitted))
            .expect("emitted recipe re-decodes");
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn a_complete_package_recipe_decodes_across_every_field() {
        let source = complete_recipe_source();
        let package = LuaPackageEvaluator::default()
            .evaluate(&Source::new("package.lua", &source))
            .expect("complete recipe decodes");

        assert_eq!(package.meta.pname, "hello");
        assert_eq!(package.build_inputs, vec![DependencySpec::Binary("cc".to_owned())]);
        assert_eq!(package.outputs.len(), 1);
        assert_eq!(package.options.toolchain, crate::ToolchainSpec::Llvm);
        assert!(matches!(package.sources[0], UpstreamSpec::Git { .. }));
        assert_eq!(package.architectures, vec!["x86_64".to_owned()]);
        assert_eq!(package.tuning[0].key, "lto");
        assert!(package.mold);
    }

    #[test]
    fn meta_decodes_directly_as_pure_data() {
        let meta: MetaSpec = decode(
            r#"return { pname = "hello", version = "1.0", release = 1, homepage = "https://x", license = { "MIT" } }"#,
        );
        assert_eq!(meta.pname, "hello");
        assert_eq!(meta.release, 1);
        assert_eq!(meta.license, vec!["MIT".to_owned()]);
    }

    #[test]
    fn step_variants_decode_including_programs_and_builder_steps() {
        let run: StepSpec = decode::<LuaStepSpec>(
            r#"return { kind = "run", program = { path = "/bin/cc", requirement = { kind = "binary", value = "cc" } }, args = { "-c" } }"#,
        )
        .into();
        assert!(matches!(run, StepSpec::Run { program, args } if program.path == "/bin/cc" && args == ["-c"]));

        let cmake: StepSpec = decode::<LuaStepSpec>(r#"return { kind = "cmake_build" }"#).into();
        assert_eq!(cmake, StepSpec::CMakeBuild);

        let cargo: StepSpec =
            decode::<LuaStepSpec>(r#"return { kind = "cargo_install", binaries = { "hello" } }"#).into();
        assert_eq!(cargo, StepSpec::CargoInstall { binaries: vec!["hello".to_owned()] });
    }

    #[test]
    fn phases_decode_with_empty_and_populated_step_lists() {
        let source = r#"
return {
    setup = { steps = {} },
    build = { steps = { { kind = "cmake_build" } } },
    install = { steps = {} },
    check = { steps = {} },
    workload = { steps = {} },
}
"#;
        let phases: PhasesSpec = decode::<LuaPhasesSpec>(source).into();
        assert!(phases.setup.steps.is_empty());
        assert_eq!(phases.build.steps, vec![StepSpec::CMakeBuild]);
    }

    #[test]
    fn an_output_decodes_options_paths_and_dependencies() {
        let source = r#"
return {
    name = "out",
    include_in_manifest = true,
    summary = { kind = "some", value = "main output" },
    description = { kind = "none" },
    provides_exclude = {},
    runtime_inputs = { { kind = "soname", value = "libc.so.6" } },
    runtime_exclude = {},
    paths = { { kind = "exe", path = "/usr/bin/hello" } },
    conflicts = {},
}
"#;
        let output: OutputSpec = decode::<LuaOutputSpec>(source).into();
        assert_eq!(output.name, "out");
        assert_eq!(output.summary, Some("main output".to_owned()));
        assert_eq!(output.description, None);
        assert_eq!(output.runtime_inputs, vec![DependencySpec::Soname("libc.so.6".to_owned())]);
        assert_eq!(output.paths, vec![crate::PathSpec::Exe { path: "/usr/bin/hello".to_owned() }]);
    }

    #[test]
    fn upstream_archive_and_git_decode_with_optional_fields() {
        let archive: UpstreamSpec = decode::<LuaUpstreamSpec>(
            r#"return { kind = "archive", url = "https://x/a.tar", hash = "abc", rename = { kind = "some", value = "a" }, strip_dirs = { kind = "none" }, unpack = true, unpack_dir = { kind = "none" } }"#,
        )
        .into();
        assert!(matches!(archive, UpstreamSpec::Archive { rename: Some(ref r), unpack: true, .. } if r == "a"));

        let git: UpstreamSpec = decode::<LuaUpstreamSpec>(
            r#"return { kind = "git", url = "https://x/g.git", git_ref = "main", clone_dir = { kind = "none" } }"#,
        )
        .into();
        assert!(matches!(git, UpstreamSpec::Git { clone_dir: None, .. }));
    }

    #[test]
    fn dependency_variants_decode_including_references() {
        let binary: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "binary", value = "cc" }"#).into();
        assert_eq!(binary, DependencySpec::Binary("cc".to_owned()));

        let cmake: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "cmake", value = "Foo" }"#).into();
        assert_eq!(cmake, DependencySpec::CMake("Foo".to_owned()));

        let package: DependencySpec =
            decode::<LuaDependencySpec>(r#"return { kind = "package", value = { name = "glibc" } }"#).into();
        assert_eq!(package, DependencySpec::Package(PackageRef { name: "glibc".to_owned() }));

        let output: DependencySpec = decode::<LuaDependencySpec>(
            r#"return { kind = "output", value = { package = { name = "llvm" }, output = "dev" } }"#,
        )
        .into();
        assert_eq!(
            output,
            DependencySpec::Output(OutputRef {
                package: PackageRef { name: "llvm".to_owned() },
                output: "dev".to_owned(),
            })
        );
    }

    /// A hand-authored, minimal Lua recipe — no generated marker, no expanded
    /// builder, no output/option/hook tables — decodes through the shared
    /// [`lower`] into a complete [`PackageSpec`]: the default split-output set,
    /// the lowered cmake builder, and the default options. This proves Lua can
    /// author a full package on its own, with all authoring logic in shared Rust
    /// and none in a config language.
    #[test]
    fn a_minimal_authored_lua_recipe_lowers_through_shared_rust() {
        let source = r#"
return {
    meta = {
        pname = "hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/hello",
        license = { "MIT" },
    },
    builder = { kind = "cmake", flags = { "-DBUILD_TESTS=ON" } },
    build_inputs = { { kind = "package", value = { name = "zlib" } } },
}
"#;
        let package = LuaPackageEvaluator::default()
            .evaluate_authored(&Source::new("stone.lua", source))
            .expect("minimal authored recipe lowers");

        // Identity and the one dependency the recipe declared survive.
        assert_eq!(package.meta.pname, "hello");
        assert_eq!(
            package.build_inputs,
            vec![DependencySpec::Package(PackageRef { name: "zlib".to_owned() })]
        );

        // The builder request lowered to typed cmake steps, with checks on by
        // default (the recipe omitted `run_tests`).
        assert_eq!(
            package.builder.phases.setup.steps,
            vec![StepSpec::CMakeConfigure { flags: vec!["-DBUILD_TESTS=ON".to_owned()] }]
        );
        assert_eq!(package.builder.phases.check.steps, vec![StepSpec::CMakeTest]);

        // Every omitted optional took the shared package-ABI default.
        assert_eq!(package.outputs.len(), 9);
        assert_eq!(package.outputs[0].name, "out");
        assert_eq!(package.options.toolchain, ToolchainSpec::Llvm);
        assert_eq!(package.hooks, HooksSpec::default());

        // The engine path is exactly the shared lowering of the equivalent
        // language-agnostic authored package — the Lua table is pure syntax.
        let equivalent = lower(AuthoredPackage {
            meta: package.meta.clone(),
            builder: BuilderRequest::Cmake {
                flags: vec!["-DBUILD_TESTS=ON".to_owned()],
                run_tests: true,
            },
            sources: Vec::new(),
            native_build_inputs: Vec::new(),
            build_inputs: vec![DependencySpec::Package(PackageRef { name: "zlib".to_owned() })],
            check_inputs: Vec::new(),
            outputs: None,
            options: None,
            profiles: Vec::new(),
            architectures: Vec::new(),
            tuning: Vec::new(),
            emul32: false,
            mold: false,
            hooks: HooksSpec::default(),
        })
        .expect("the equivalent authored package is valid");
        assert_eq!(package, equivalent);
    }

    /// The Lua half of the rich-authoring proof: a `custom` builder (the data
    /// escape hatch), a dependency, and an explicit output override decode
    /// through the shared `lower`. This confirms the Lua `Custom` builder-request
    /// path and that authored outputs replace the default set — symmetric to the
    /// Gluon `a_rich_authored_gluon_recipe...` proof.
    #[test]
    fn an_authored_lua_recipe_with_a_custom_builder_lowers_through_shared_rust() {
        let source = r#"
return {
    meta = { pname = "hello", version = "1.0.0", release = 1, homepage = "https://x", license = { "MIT" } },
    builder = { kind = "custom", spec = {
        required_tools = { { kind = "binary", value = "zig" } },
        environment = {},
        phases = {
            setup = { steps = {} },
            build = { steps = { { kind = "run",
                program = { path = "/usr/bin/zig", requirement = { kind = "binary", value = "zig" } },
                args = { "build" } } } },
            install = { steps = {} },
            check = { steps = {} },
            workload = { steps = {} },
        },
        supported_hooks = { setup = true, build = true, check = true, install = true, workload = true },
    } },
    build_inputs = { { kind = "package", value = { name = "zlib" } } },
    outputs = { { name = "out", include_in_manifest = true, summary = { kind = "none" },
        description = { kind = "none" }, provides_exclude = {}, runtime_inputs = {},
        runtime_exclude = {}, paths = {}, conflicts = {} } },
}
"#;
        let package = LuaPackageEvaluator::default()
            .evaluate_authored(&Source::new("stone.lua", source))
            .expect("custom authored recipe lowers");

        // The custom builder passed through untouched: no environment, its tools.
        assert!(package.builder.environment.is_empty());
        assert_eq!(
            package.builder.required_tools(),
            [DependencySpec::Binary("zig".to_owned())]
        );
        // The authored dependency survives; the explicit output replaces defaults.
        assert_eq!(
            package.build_inputs,
            vec![DependencySpec::Package(PackageRef { name: "zlib".to_owned() })]
        );
        assert_eq!(package.outputs.len(), 1);
        assert_eq!(package.outputs[0].name, "out");
    }
