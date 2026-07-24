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
