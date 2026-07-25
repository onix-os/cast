    use super::*;
    use super::super::{AnalyzerKind, ArrayPatch, ValuePatch};

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("build-policy.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn a_literal_text_spec_decodes() {
        let text: TextSpec = decode::<LuaTextSpec>(r#"return { kind = "literal", value = "cc" }"#).into();
        assert_eq!(text, TextSpec::Literal("cc".to_owned()));
    }

    #[test]
    fn a_context_text_spec_decodes_the_unit_context_value() {
        let text: TextSpec =
            decode::<LuaTextSpec>(r#"return { kind = "context", value = "package_name" }"#).into();
        assert_eq!(text, TextSpec::Context(ContextValue::PackageName));
    }

    #[test]
    fn a_nested_concat_text_spec_decodes_recursively() {
        let source = r#"
return {
    kind = "concat",
    values = {
        { kind = "literal", value = "lib" },
        { kind = "context", value = "lib_suffix" },
    },
}
"#;
        let text: TextSpec = decode::<LuaTextSpec>(source).into();
        assert_eq!(
            text,
            TextSpec::Concat(vec![
                TextSpec::Literal("lib".to_owned()),
                TextSpec::Context(ContextValue::LibSuffix),
            ])
        );
    }

    #[test]
    fn build_tool_variants_decode() {
        let package: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "package", value = "cmake" }"#).into();
        assert_eq!(package, BuildToolSpec::Package("cmake".to_owned()));

        let system: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "system_binary", value = "/bin/sh" }"#)
                .into();
        assert_eq!(system, BuildToolSpec::SystemBinary("/bin/sh".to_owned()));
    }

    #[test]
    fn a_value_patch_keeps_or_sets_the_converted_payload() {
        let keep = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ValuePatch::Keep);

        let set = value_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "set", value = { kind = "binary", value = "meson" } }"#,
        ));
        assert_eq!(set, ValuePatch::Set(BuildToolSpec::Binary("meson".to_owned())));
    }

    #[test]
    fn an_array_patch_maps_every_operation_and_element() {
        let keep = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(r#"return { kind = "keep" }"#));
        assert_eq!(keep, ArrayPatch::Keep);

        let append = array_patch::<LuaBuildToolSpec, BuildToolSpec>(decode(
            r#"return { kind = "append", values = { { kind = "package", value = "ninja" } } }"#,
        ));
        assert_eq!(
            append,
            ArrayPatch::Append(vec![BuildToolSpec::Package("ninja".to_owned())])
        );
    }

    #[test]
    fn compiler_flags_decode_with_empty_and_populated_lists() {
        let source = r#"
return {
    c = { { kind = "literal", value = "-Wall" } },
    cxx = {},
    f = {},
    d = {},
    rust = {},
    vala = {},
    go = {},
    ld = { { kind = "context", value = "ld_flags" } },
}
"#;
        let flags: CompilerFlagsSpec = decode::<LuaCompilerFlagsSpec>(source).into();
        assert_eq!(flags.c, vec![TextSpec::Literal("-Wall".to_owned())]);
        assert!(flags.cxx.is_empty());
        assert_eq!(flags.ld, vec![TextSpec::Context(ContextValue::LdFlags)]);
    }

    #[test]
    fn a_build_command_decodes_program_requirement_and_args() {
        let source = r#"
return {
    program = {
        path = "/usr/bin/cc",
        requirement = { kind = "package", value = "llvm" },
    },
    args = { "-fPIC", "-O2" },
}
"#;
        let command: BuildCommandSpec = decode::<LuaBuildCommandSpec>(source).into();
        assert_eq!(command.program.path, "/usr/bin/cc");
        assert_eq!(command.program.requirement, BuildToolSpec::Package("llvm".to_owned()));
        assert_eq!(command.args, vec!["-fPIC".to_owned(), "-O2".to_owned()]);
    }

    #[test]
    fn pure_target_types_decode_directly_on_the_domain_type() {
        let platform: PlatformPolicySpec = decode(
            r#"return { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" }"#,
        );
        assert_eq!(platform.architecture, "x86_64");

        let native: TargetEmulationSpec = decode(r#"return { kind = "native" }"#);
        assert_eq!(native, TargetEmulationSpec::Native);

        let emul32: TargetEmulationSpec =
            decode(r#"return { kind = "emul32", host_architecture = "x86_64" }"#);
        assert_eq!(
            emul32,
            TargetEmulationSpec::Emul32 { host_architecture: "x86_64".to_owned() }
        );

        let condition: EnvironmentCondition = decode(r#"return "compiler_cache_enabled""#);
        assert_eq!(condition, EnvironmentCondition::CompilerCacheEnabled);
    }

    #[test]
    fn an_environment_binding_decodes_value_and_condition() {
        let source = r#"
return {
    name = "CFLAGS",
    value = { kind = "context", value = "c_flags" },
    condition = "always",
}
"#;
        let binding: EnvironmentBindingSpec = decode::<LuaEnvironmentBindingSpec>(source).into();
        assert_eq!(binding.name, "CFLAGS");
        assert_eq!(binding.value, TextSpec::Context(ContextValue::CFlags));
        assert_eq!(binding.condition, EnvironmentCondition::Always);
    }

    #[test]
    fn the_sandbox_policy_decodes_directly_as_pure_data() {
        let source = r#"
return {
    hostname = "builder",
    credentials = "isolated_root",
    filesystems = { tmp = "empty", sys = "none", dev = "minimal" },
    guest_root = "/mason",
    artifacts_dir = "/mason/artifacts",
    build_dir = "/mason/build",
    source_dir = "/mason/source",
    recipe_dir = "/mason/recipe",
    package_dir = "/mason/package",
    install_dir = "/mason/install",
}
"#;
        let sandbox: SandboxPolicySpec = decode(source);
        assert_eq!(sandbox.hostname, "builder");
        assert_eq!(sandbox.filesystems.dev, SandboxDevPolicySpec::Minimal);
    }

    #[test]
    fn toolchain_input_tools_decode_through_the_wrapper() {
        let source = r#"
return {
    llvm = { { kind = "package", value = "clang" } },
    gnu = {},
}
"#;
        let inputs: ToolchainInputPolicySpec = decode::<LuaToolchainInputPolicySpec>(source).into();
        assert_eq!(inputs.llvm, vec![BuildToolSpec::Package("clang".to_owned())]);
        assert!(inputs.gnu.is_empty());
    }

    #[test]
    fn a_pgo_finish_decodes_optional_and_list_fields() {
        let with_copy = r#"
return {
    output = { kind = "literal", value = "merged.profdata" },
    inputs = { { kind = "literal", value = "a.profraw" }, { kind = "literal", value = "b.profraw" } },
    copy_to = { kind = "some", value = { kind = "literal", value = "final.profdata" } },
    remove_output_first = true,
}
"#;
        let finish: PgoFinishSpec = decode::<LuaPgoFinishSpec>(with_copy).into();
        assert_eq!(finish.output, TextSpec::Literal("merged.profdata".to_owned()));
        assert_eq!(finish.inputs.len(), 2);
        assert_eq!(finish.copy_to, Some(TextSpec::Literal("final.profdata".to_owned())));
        assert!(finish.remove_output_first);

        let without_copy = with_copy.replace(
            r#"copy_to = { kind = "some", value = { kind = "literal", value = "final.profdata" } },"#,
            r#"copy_to = { kind = "none" },"#,
        );
        let bare: PgoFinishSpec = decode::<LuaPgoFinishSpec>(&without_copy).into();
        assert_eq!(bare.copy_to, None);
    }

    #[test]
    fn analyzer_tools_decode_through_the_wrapper() {
        let source = r#"
return {
    pkg_config = { kind = "binary", value = "pkg-config" },
    python = { kind = "package", value = "python" },
    llvm = {
        objcopy = { kind = "binary", value = "llvm-objcopy" },
        strip = { kind = "binary", value = "llvm-strip" },
    },
    gnu = {
        objcopy = { kind = "binary", value = "objcopy" },
        strip = { kind = "binary", value = "strip" },
    },
}
"#;
        let tools: AnalyzerToolsPolicySpec = decode::<LuaAnalyzerToolsPolicySpec>(source).into();
        assert_eq!(tools.python, BuildToolSpec::Package("python".to_owned()));
        assert_eq!(tools.llvm.objcopy, BuildToolSpec::Binary("llvm-objcopy".to_owned()));
    }

    #[test]
    fn a_tuning_group_decodes_pure_choices_and_a_tagged_default() {
        let source = r#"
return {
    base = { enabled = { "lto" }, disabled = {} },
    default = { kind = "some", value = "balanced" },
    choices = {
        { name = "balanced", value = { enabled = { "o2" }, disabled = { "o3" } } },
    },
}
"#;
        let group: TuningGroupSpec = decode::<LuaTuningGroupSpec>(source).into();
        assert_eq!(group.base.enabled, vec!["lto".to_owned()]);
        assert_eq!(group.default, Some("balanced".to_owned()));
        assert_eq!(group.choices.len(), 1);
        assert_eq!(group.choices[0].name, "balanced");
    }

    // A complete policy is a large authored surface; rather than hand-write
    // ~250 lines of Lua, these Rust helpers assemble a minimal-but-complete
    // source (the profile forbids Lua-side helper functions, so the repetition
    // is generated here instead).
    fn lit(value: &str) -> String {
        format!(r#"{{ kind = "literal", value = "{value}" }}"#)
    }
    fn program() -> String {
        r#"{ path = "/bin/tool", requirement = { kind = "package", value = "t" } }"#.to_owned()
    }
    fn command() -> String {
        format!("{{ program = {}, args = {{}} }}", program())
    }
    fn builder_command() -> String {
        format!(
            "{{ program = {}, args = {{}}, environment = {{}}, working_dir = {} }}",
            program(),
            lit("/work")
        )
    }
    fn flags() -> String {
        "{ c = {}, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} }".to_owned()
    }
    fn toolchain_flags() -> String {
        format!("{{ common = {f}, gnu = {f}, llvm = {f} }}", f = flags())
    }
    fn compiler_tools() -> String {
        let roles = [
            "cc", "cxx", "objc", "objcxx", "cpp", "objcpp", "objcxxcpp", "ar", "ld", "objcopy",
            "nm", "ranlib", "strip",
        ];
        let body = roles.iter().map(|r| format!("{r} = {}", command())).collect::<Vec<_>>().join(", ");
        format!("{{ {body} }}")
    }
    fn tool() -> String {
        r#"{ kind = "package", value = "t" }"#.to_owned()
    }
    fn standard_builder() -> String {
        format!(
            "{{ environment = {{}}, setup = {c}, build = {c}, install = {c}, check = {c} }}",
            c = builder_command()
        )
    }
    fn stage() -> String {
        format!("{{ flags = {}, finish = {{ kind = \"none\" }} }}", toolchain_flags())
    }

    fn complete_policy_source() -> String {
        let layout_fields = [
            "prefix", "bindir", "sbindir", "includedir", "libdir", "libexecdir", "datadir",
            "vendordir", "docdir", "infodir", "localedir", "mandir", "sysconfdir", "localstatedir",
            "sharedstatedir", "runstatedir", "sysusersdir", "tmpfilesdir", "udevrulesdir",
            "bash_completions_dir", "fish_completions_dir", "elvish_completions_dir",
            "zsh_completions_dir",
        ];
        let layout = layout_fields
            .iter()
            .map(|f| format!("{f} = {}", lit(&format!("/{f}"))))
            .collect::<Vec<_>>()
            .join(", ");
        let toolchains = format!("{{ llvm = {t}, gnu = {t} }}", t = compiler_tools());
        let toolchain_inputs = "{ llvm = {}, gnu = {} }";
        let sandbox = r#"{
            hostname = "builder",
            credentials = "isolated_root",
            filesystems = { tmp = "empty", sys = "none", dev = "minimal" },
            guest_root = "/mason", artifacts_dir = "/a", build_dir = "/b", source_dir = "/s",
            recipe_dir = "/r", package_dir = "/p", install_dir = "/i"
        }"#;
        let build_root = format!(
            "{{ base = {{}}, toolchains = {ti}, emul32 = {{ base = {{}}, toolchains = {ti} }}, \
             analyzer_tools = {{ pkg_config = {t}, python = {t}, \
             llvm = {{ objcopy = {t}, strip = {t} }}, gnu = {{ objcopy = {t}, strip = {t} }} }}, \
             compiler_cache = {{ ccache = {p}, sccache = {p}, ccache_dir = \"/c\", sccache_dir = \"/c\", \
             go_cache_dir = \"/c\", go_mod_cache_dir = \"/c\", cargo_cache_dir = \"/c\", zig_cache_dir = \"/c\" }}, \
             mold = {{ linker = {cmd}, flags = {f} }} }}",
            ti = toolchain_inputs,
            t = tool(),
            p = program(),
            cmd = command(),
            f = flags(),
        );
        let sources = format!(
            "{{ git = {{ create_directory = {c}, copy = {c} }} }}",
            c = builder_command()
        );
        let builders = format!(
            "{{ cmake = {b}, meson = {b}, cargo = {b}, autotools = {b} }}",
            b = standard_builder()
        );
        let pgo = format!(
            "{{ shell_interpreter = {p}, merge_program = {p}, merge_args = {{}}, copy_program = {p}, \
             remove_program = {p}, sample = {tf}, stage_one = {s}, stage_two = {s}, use_profile = {s} }}",
            p = program(),
            tf = toolchain_flags(),
            s = stage(),
        );
        format!(
            "return {{\n\
             build_subdir = \"build\",\n\
             layout = {{ {layout} }},\n\
             toolchains = {toolchains},\n\
             targets = {{}},\n\
             retired_targets = {{}},\n\
             sandbox = {sandbox},\n\
             build_root = {build_root},\n\
             sources = {sources},\n\
             tuning = {{ flags = {{}}, groups = {{}}, default_groups = {{}} }},\n\
             environment = {{}},\n\
             builders = {builders},\n\
             analyzers = {{}},\n\
             pgo = {pgo},\n\
             }}"
        )
    }

    #[test]
    fn a_complete_build_policy_decodes_across_every_top_level_field() {
        let source = complete_policy_source();
        let policy = LuaBuildPolicyEvaluator::default()
            .evaluate(&Source::new("build-policy.lua", &source))
            .expect("complete policy decodes");

        assert_eq!(policy.build_subdir, "build");
        assert_eq!(policy.layout.prefix, TextSpec::Literal("/prefix".to_owned()));
        assert_eq!(policy.toolchains.llvm.cc.program.path, "/bin/tool");
        assert_eq!(policy.sandbox.hostname, "builder");
        assert_eq!(policy.build_root.compiler_cache.ccache_dir, "/c");
        assert_eq!(policy.builders.cmake.setup.program.path, "/bin/tool");
        assert_eq!(policy.pgo.shell_interpreter.path, "/bin/tool");
        assert!(policy.targets.is_empty());
    }

    #[test]
    fn an_all_keep_build_policy_patch_decodes_to_the_default_overlay() {
        let source = r#"
return {
    build_subdir = { kind = "keep" },
    layout = { kind = "keep" },
    toolchains = { kind = "keep" },
    targets = { kind = "keep" },
    retired_targets = { kind = "keep" },
    sandbox = { kind = "keep" },
    build_root = { kind = "keep" },
    sources = { kind = "keep" },
    tuning = { kind = "keep" },
    environment = { kind = "keep" },
    builders = { kind = "keep" },
    analyzers = { kind = "keep" },
    pgo = { kind = "keep" },
}
"#;
        let patch = LuaBuildPolicyEvaluator::default()
            .evaluate_patch(&Source::new("build-policy.lua", source))
            .expect("all-keep patch decodes");
        assert_eq!(patch, BuildPolicyPatchSpec::default());
    }

    #[test]
    fn a_build_policy_patch_sets_a_scalar_and_appends_an_analyzer() {
        let source = r#"
return {
    build_subdir = { kind = "set", value = "build" },
    layout = { kind = "keep" },
    toolchains = { kind = "keep" },
    targets = { kind = "keep" },
    retired_targets = { kind = "keep" },
    sandbox = { kind = "keep" },
    build_root = { kind = "keep" },
    sources = { kind = "keep" },
    tuning = { kind = "keep" },
    environment = { kind = "keep" },
    builders = { kind = "keep" },
    analyzers = { kind = "append", values = { "elf" } },
    pgo = { kind = "keep" },
}
"#;
        let patch = LuaBuildPolicyEvaluator::default()
            .evaluate_patch(&Source::new("build-policy.lua", source))
            .expect("patch decodes");
        assert_eq!(patch.build_subdir, ValuePatch::Set("build".to_owned()));
        assert_eq!(patch.analyzers, ArrayPatch::Append(vec![AnalyzerKind::Elf]));
    }

    #[test]
    fn an_analyzer_kind_decodes_from_its_snake_case_name() {
        let kind: AnalyzerKind = decode(r#"return "pkg_config""#);
        assert_eq!(kind, AnalyzerKind::PkgConfig);
    }

    fn literal_layout_field(name: &str) -> String {
        format!(r#"{name} = {{ kind = "literal", value = "/{name}" }}"#)
    }

    #[test]
    fn an_install_layout_decodes_every_locator() {
        let fields = [
            "prefix", "bindir", "sbindir", "includedir", "libdir", "libexecdir", "datadir",
            "vendordir", "docdir", "infodir", "localedir", "mandir", "sysconfdir", "localstatedir",
            "sharedstatedir", "runstatedir", "sysusersdir", "tmpfilesdir", "udevrulesdir",
            "bash_completions_dir", "fish_completions_dir", "elvish_completions_dir",
            "zsh_completions_dir",
        ];
        let body = fields.iter().map(|name| literal_layout_field(name)).collect::<Vec<_>>().join(",\n    ");
        let source = format!("return {{\n    {body},\n}}");

        let layout: InstallLayoutSpec = decode::<LuaInstallLayoutSpec>(&source).into();
        assert_eq!(layout.prefix, TextSpec::Literal("/prefix".to_owned()));
        assert_eq!(layout.zsh_completions_dir, TextSpec::Literal("/zsh_completions_dir".to_owned()));
    }

    /// A minimal-but-complete policy re-decodes to itself, exercising the
    /// structural spine of every top-level field through the emitter.
    #[test]
    fn a_complete_build_policy_round_trips_through_the_emitter() {
        let evaluator = LuaBuildPolicyEvaluator::default();
        let source = complete_policy_source();
        let policy = evaluator
            .evaluate(&Source::new("build-policy.lua", &source))
            .expect("complete policy decodes");

        let emitted = encode_lua_policy(&policy);
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let redecoded = evaluator
            .evaluate(&Source::new("build-policy.lua", &emitted))
            .expect("emitted policy re-decodes");
        assert_eq!(policy, redecoded);
    }

    /// A richer authored source exercises the encoder paths the minimal policy
    /// leaves empty: context/concat text, non-native emulation-free targets with
    /// per-target flags and bindings, retired targets, populated tuning groups
    /// with a default choice, a PGO finish with an optional copy, and every
    /// analyzer name (including the multi-capital `c_make`).
    fn rich_policy_source() -> String {
        let target = r#"{
            name = "native", target_triple = "x86_64-linux-gnu",
            build_triple = "x86_64-linux-gnu", host_triple = "x86_64-linux-gnu",
            lib_suffix = "64", artifact_architecture = "x86_64",
            emulation = { kind = "native" },
            build_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
            host_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
            target_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
            architecture_flags = {
                common = { c = { { kind = "context", value = "c_flags" } }, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} },
                gnu = { c = {}, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} },
                llvm = { c = {}, cxx = {}, f = {}, d = {}, rust = {}, vala = {}, go = {}, ld = {} }
            },
            environment = { { name = "CFLAGS", value = { kind = "context", value = "c_flags" }, condition = "compiler_cache_enabled" } }
        }"#;
        let tuning = r#"{
            flags = {},
            groups = { { name = "opt", value = {
                base = { enabled = { "o2" }, disabled = {} },
                default = { kind = "some", value = "balanced" },
                choices = { { name = "balanced", value = { enabled = { "o2" }, disabled = { "o3" } } } }
            } } },
            default_groups = { "opt" }
        }"#;
        let environment =
            r#"{ { name = "PATH", value = { kind = "literal", value = "/usr/bin" }, condition = "always" } }"#;
        let libdir_concat = r#"libdir = { kind = "concat", values = { { kind = "literal", value = "/usr/lib" }, { kind = "context", value = "lib_suffix" } } }"#;
        let finish = r#"finish = { kind = "some", value = { output = { kind = "literal", value = "merged.profdata" }, inputs = { { kind = "literal", value = "a.profraw" } }, copy_to = { kind = "some", value = { kind = "literal", value = "final.profdata" } }, remove_output_first = true } }"#;

        complete_policy_source()
            // Anchor on the leading newline so this does not also match the
            // `targets = {}` tail inside `retired_targets = {}`.
            .replace("\ntargets = {},\n", &format!("\ntargets = {{ {target} }},\n"))
            .replace(
                "retired_targets = {},\n",
                "retired_targets = { { name = \"old\", reason = \"removed\" } },\n",
            )
            .replace("tuning = { flags = {}, groups = {}, default_groups = {} },", &format!("tuning = {tuning},"))
            .replace("environment = {},\n", &format!("environment = {environment},\n"))
            .replace(
                "analyzers = {},\n",
                "analyzers = { \"elf\", \"binary\", \"c_make\", \"pkg_config\" },\n",
            )
            .replace(r#"libdir = { kind = "literal", value = "/libdir" }"#, libdir_concat)
            .replacen(r#"finish = { kind = "none" }"#, finish, 1)
    }

    #[test]
    fn a_rich_build_policy_round_trips_through_the_emitter() {
        let evaluator = LuaBuildPolicyEvaluator::default();
        let source = rich_policy_source();
        let policy = evaluator
            .evaluate(&Source::new("build-policy.lua", &source))
            .expect("rich policy decodes");

        // The richer source must actually reach the paths the minimal one skips.
        assert_eq!(policy.targets.len(), 1);
        assert_eq!(policy.retired_targets.len(), 1);
        assert_eq!(policy.analyzers, vec![
            AnalyzerKind::Elf,
            AnalyzerKind::Binary,
            AnalyzerKind::CMake,
            AnalyzerKind::PkgConfig,
        ]);
        assert!(matches!(policy.layout.libdir, TextSpec::Concat(_)));
        assert!(policy.pgo.stage_one.finish.is_some());

        let emitted = encode_lua_policy(&policy);
        let redecoded = evaluator
            .evaluate(&Source::new("build-policy.lua", &emitted))
            .expect("emitted rich policy re-decodes");
        assert_eq!(policy, redecoded);
    }
