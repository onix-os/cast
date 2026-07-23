use super::*;
use crate::BuildPolicy;
use stone_recipe::build_policy::BuildToolSpec;

fn fixture_context(target_name: &str, compiler_cache_enabled: bool, mold_enabled: bool) -> BuildContext {
    let policy = BuildPolicy::repository_for_tests();
    let target = policy.target(target_name).unwrap();
    BuildContext::resolve(
        &policy.spec,
        target,
        TypedContextInputs {
            package_name: "example".to_owned(),
            package_version: "1.2.3".to_owned(),
            package_release: 4,
            source_dir: "/mason/sourcedir".to_owned(),
            install_root: "/mason/install".to_owned(),
            build_root: format!("/mason/build/{target_name}"),
            work_dir: format!("/mason/build/{target_name}/source"),
            pgo_dir: format!("/mason/build/{target_name}-pgo"),
            jobs: 8,
            source_date_epoch: 1_700_000_000,
            pgo_stage: PgoContextStage::Two,
            toolchain: ToolchainSpec::Llvm,
            compiler_cache_enabled,
            mold_enabled,
            flags: CompilerFlagsSpec {
                c: vec![
                    TextSpec::Literal("-O2".to_owned()),
                    TextSpec::Concat(vec![
                        TextSpec::Literal("-flto=".to_owned()),
                        TextSpec::Context(ContextValue::Jobs),
                    ]),
                ],
                rust: vec![TextSpec::Concat(vec![
                    TextSpec::Literal("-Cprofile-use=".to_owned()),
                    TextSpec::Context(ContextValue::PgoDir),
                ])],
                ..CompilerFlagsSpec::default()
            },
        },
    )
    .unwrap()
}

#[test]
fn typed_context_resolves_policy_layout_tools_flags_and_cache_conditions() {
    let context = fixture_context("x86_64", false, true);

    assert_eq!(context.layout.libdir, "/usr/lib");
    assert_eq!(context.layout.libexecdir, "/usr/lib/example");
    assert_eq!(context.tools.cc, "/usr/bin/clang");
    assert_eq!(context.tools.objcpp, "/usr/bin/clang -E -");
    assert_eq!(context.tools.ld, "/usr/bin/ld.mold");
    assert_eq!(context.flags.c, "-O2 -flto=8 -fuse-ld=mold");
    assert_eq!(
        context.flags.rust,
        "-Cprofile-use=/mason/build/x86_64-pgo -Clink-arg=-fuse-ld=mold"
    );
    assert_eq!(context.environment["PATH"], "/usr/bin:/bin");
    assert_eq!(context.environment["CFLAGS"], context.flags.c);
    assert_eq!(context.environment["PGO_STAGE"], "TWO");
    assert_eq!(context.environment["GOAMD64"], "v2");
    assert_eq!(
        context.environment["PKG_CONFIG_PATH"],
        "/usr/lib/pkgconfig:/usr/share/pkgconfig"
    );
    assert!(!context.environment.contains_key("CCACHE_DIR"));

    let cached = fixture_context("x86_64", true, false);
    assert_eq!(cached.environment["PATH"], "/usr/bin:/bin");
    assert_eq!(cached.environment["CCACHE_DIR"], "/mason/ccache");
    assert_eq!(cached.environment["RUSTC_WRAPPER"], "/usr/bin/sccache");
    assert_eq!(cached.tools.cc, "/usr/bin/ccache /usr/bin/clang");
    assert_eq!(cached.tools.objcpp, "/usr/bin/ccache /usr/bin/clang -E -");
    assert_eq!(cached.tools.ld, "/usr/bin/ld.lld");
    assert!(!cached.flags.c.contains("mold"));
}

#[test]
fn compiler_flag_tokens_preserve_policy_order_and_multiplicity() {
    let policy = BuildPolicy::repository_for_tests();
    let target = policy.target("x86_64").unwrap();
    let mut inputs = fixture_context("x86_64", false, false).inputs;
    inputs.flags.rust = vec![
        TextSpec::Literal("-C".to_owned()),
        TextSpec::Literal("opt-level=3".to_owned()),
        TextSpec::Literal("-C".to_owned()),
        TextSpec::Literal("codegen-units=1".to_owned()),
    ];

    let context = BuildContext::resolve(&policy.spec, target, inputs).unwrap();
    assert_eq!(context.flags.rust, "-C opt-level=3 -C codegen-units=1");
    assert_eq!(context.environment["RUSTFLAGS"], context.flags.rust);
}

#[test]
fn repository_default_tuning_reaches_rustflags_without_token_loss() {
    let policy = BuildPolicy::repository_for_tests();
    let target = policy.target("x86_64").unwrap();
    let tuning = crate::build::tuning::resolve(&policy.spec.tuning, target, ToolchainSpec::Llvm, &[]).unwrap();
    let mut inputs = fixture_context("x86_64", false, false).inputs;
    inputs.flags = tuning.flags;

    let context = BuildContext::resolve(&policy.spec, target, inputs).unwrap();
    assert_eq!(
        context.environment["RUSTFLAGS"],
        "-C strip=none \
         -C link-args=-Wl,--build-id=sha1 \
         -C link-args=-Wl,--compress-debug-sections=zstd \
         -C debuginfo=2 \
         -C split-debuginfo=off \
         -C lto=thin \
         -C linker-plugin-lto \
         -C embed-bitcode=yes \
         -C force-frame-pointers \
         -C opt-level=3 \
         -C codegen-units=1 \
         -Ctarget-cpu=x86-64-v2"
    );
    assert!(!context.environment.contains_key("CARGO_ENCODED_RUSTFLAGS"));
}

#[test]
fn compiler_command_tokens_are_shell_quoted_without_path_lookup() {
    let command = BuildCommandSpec {
        program: BuildProgramSpec {
            path: "/usr/bin/clang".to_owned(),
            requirement: BuildToolSpec::Binary("clang".to_owned()),
        },
        args: vec![
            "-E".to_owned(),
            "two words".to_owned(),
            "has'quote".to_owned(),
            "$HOME".to_owned(),
            String::new(),
        ],
    };
    let wrapper = BuildProgramSpec {
        path: "/usr/bin/ccache".to_owned(),
        requirement: BuildToolSpec::Binary("ccache".to_owned()),
    };

    assert_eq!(
        render_build_command(&command, Some(&wrapper)),
        "/usr/bin/ccache /usr/bin/clang -E 'two words' 'has'\"'\"'quote' '$HOME' ''"
    );
}

#[test]
fn target_environment_overrides_global_tool_values() {
    let context = fixture_context("emul32/x86_64", false, false);

    assert_eq!(context.environment["CC"], "/usr/bin/clang -m32");
    assert_eq!(context.environment["CXX"], "/usr/bin/clang++ -m32");
    assert_eq!(context.environment["CPP"], "/usr/bin/clang-cpp -m32");
    assert_eq!(
        context.environment["PKG_CONFIG_PATH"],
        "/usr/lib32/pkgconfig:/usr/share/pkgconfig:/usr/lib/pkgconfig"
    );
}

#[test]
fn standard_builder_commands_come_from_policy_and_keep_package_arguments() {
    let mut context = fixture_context("x86_64", false, false);
    let Run {
        program,
        args,
        working_dir,
        ..
    } = context
        .resolve_standard_step(&StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_TESTS=OFF".to_owned()],
        })
        .unwrap()
        .unwrap()
    else {
        panic!("expected run")
    };
    assert_eq!(program.path, "/usr/bin/cmake");
    assert_eq!(program.requirement.canonical_name(), "binary(cmake)");
    assert_eq!(working_dir, "/mason/build/x86_64/source");
    assert_eq!(&args[..4], ["-G", "Ninja", "-B", "aerynos-builddir"]);
    assert_eq!(args.last().unwrap(), "-DBUILD_TESTS=OFF");

    let Run { args, .. } = context
        .resolve_standard_step(&StepSpec::MesonSetup {
            flags: vec!["-Ddocs=false".to_owned()],
        })
        .unwrap()
        .unwrap()
    else {
        panic!("expected run")
    };
    assert_eq!(&args[args.len() - 2..], ["-Ddocs=false", "aerynos-builddir"]);

    let cargo_environment = context.policy.builders.cargo.environment.clone();
    context.extend_environment(&cargo_environment).unwrap();
    let Run { args, environment, .. } = context
        .resolve_standard_step(&StepSpec::CargoTest {
            features: vec!["cli".to_owned(), "tls".to_owned()],
        })
        .unwrap()
        .unwrap()
    else {
        panic!("expected run")
    };
    assert_eq!(&args[args.len() - 3..], ["--features", "cli,tls", "--workspace"]);
    assert_eq!(environment["CARGO_BUILD_DEP_INFO_BASEDIR"], context.inputs.work_dir);

    let Run { args, .. } = context
        .resolve_standard_step(&StepSpec::CargoInstall {
            binaries: vec!["one".to_owned(), "two".to_owned()],
        })
        .unwrap()
        .unwrap()
    else {
        panic!("expected run")
    };
    assert_eq!(
        &args[args.len() - 2..],
        [
            "target/x86_64-unknown-linux-gnu/release/one",
            "target/x86_64-unknown-linux-gnu/release/two",
        ]
    );

    let Run { args, .. } = context
        .resolve_standard_step(&StepSpec::AutotoolsConfigure { flags: Vec::new() })
        .unwrap()
        .unwrap()
    else {
        panic!("expected run")
    };
    assert!(args.contains(&"--build=x86_64-aerynos-linux".to_owned()));
    assert!(args.contains(&"--host=x86_64-aerynos-linux".to_owned()));
}

#[test]
fn changing_policy_command_data_changes_frozen_argv() {
    let mut policy = BuildPolicy::repository_for_tests();
    policy.spec.builders.cmake.build.program.path = "/usr/bin/policy-cmake".to_owned();
    policy.spec.builders.cmake.build.program.requirement = BuildToolSpec::Binary("policy-cmake".to_owned());
    policy.spec.builders.cmake.build.args = vec![
        TextSpec::Literal("--policy-build".to_owned()),
        TextSpec::Context(ContextValue::BuilderDir),
    ];
    let inputs = fixture_context("x86_64", false, false).inputs;
    let target = policy.target("x86_64").unwrap();
    let context = BuildContext::resolve(&policy.spec, target, inputs).unwrap();

    let Run { program, args, .. } = context.resolve_standard_step(&StepSpec::CMakeBuild).unwrap().unwrap() else {
        panic!("expected run")
    };
    assert_eq!(program.path, "/usr/bin/policy-cmake");
    assert_eq!(program.requirement.canonical_name(), "binary(policy-cmake)");
    assert_eq!(args, ["--policy-build", "aerynos-builddir"]);
}

#[test]
fn source_context_is_command_local_and_missing_values_are_actionable() {
    let context = fixture_context("x86_64", false, false);
    assert_eq!(
        context.resolve_text(&TextSpec::Context(ContextValue::SourcePath)),
        Err(ContextError::MissingContext {
            value: ContextValue::SourcePath,
        })
    );

    let overlay = TextContextOverlay {
        source_path: Some("/mason/sourcedir/source archive.tar.xz".to_owned()),
        source_destination: Some("source tree".to_owned()),
    };
    let Run {
        program,
        args,
        working_dir,
        ..
    } = context
        .resolve_command(&context.policy.sources.git.copy, &overlay)
        .unwrap()
    else {
        panic!("expected run")
    };
    assert_eq!(program.path, "/usr/bin/cp");
    assert_eq!(working_dir, "/mason/build/x86_64");
    assert_eq!(
        args,
        [
            "-Ra",
            "--no-preserve=ownership",
            "/mason/sourcedir/source archive.tar.xz/.",
            "source tree",
        ]
    );
}

#[test]
fn recursive_policy_context_is_rejected_without_interpolation() {
    let mut policy = BuildPolicy::repository_for_tests();
    policy.spec.layout.prefix = TextSpec::Context(ContextValue::Prefix);
    let mut inputs = fixture_context("x86_64", false, false).inputs;
    inputs.flags = CompilerFlagsSpec::default();
    let target = policy.target("x86_64").unwrap();

    assert_eq!(
        BuildContext::resolve(&policy.spec, target, inputs),
        Err(ContextError::RecursiveContext {
            chain: vec![ContextValue::Prefix, ContextValue::Prefix],
        })
    );
}

#[test]
fn detached_target_is_rejected_before_resolution() {
    let policy = BuildPolicy::repository_for_tests();
    let detached = policy.target("x86_64").unwrap().clone();
    let inputs = fixture_context("x86_64", false, false).inputs;

    assert_eq!(
        BuildContext::resolve(&policy.spec, &detached, inputs),
        Err(ContextError::TargetNotInPolicy)
    );
}

#[test]
fn resolver_item_budget_is_shared_across_layout_tools_and_flags() {
    let context = fixture_context("x86_64", false, false);
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_resolved_items =
        InstallLayout::RESOLVED_ITEMS + ResolvedCompilerTools::RESOLVED_ITEMS + ResolvedCompilerFlags::RESOLVED_ITEMS;
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );

    resolver.resolve_layout().unwrap();
    resolver.resolve_tools().unwrap();
    resolver.resolve_flags_record().unwrap();
    assert_eq!(
        resolver.resolve(&TextSpec::Literal("x".to_owned())),
        Err(ContextError::ResolvedItemLimit {
            count: limits.max_resolved_items + 1,
            limit: limits.max_resolved_items,
        })
    );
}

#[test]
fn resolver_aggregate_bytes_nodes_and_steps_accept_n_and_reject_n_plus_one() {
    let context = fixture_context("x86_64", false, false);

    let mut byte_limits = BuildPolicyValidationLimits::default();
    byte_limits.max_total_resolved_text_bytes = 3;
    let bytes = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        byte_limits,
    );
    assert_eq!(bytes.resolve(&TextSpec::Literal("ab".to_owned())), Ok("ab".to_owned()));
    assert_eq!(bytes.resolve(&TextSpec::Literal("c".to_owned())), Ok("c".to_owned()));
    assert_eq!(
        bytes.resolve(&TextSpec::Literal("d".to_owned())),
        Err(ContextError::TotalResolvedTextBytesLimit { bytes: 4, limit: 3 })
    );

    let mut node_limits = BuildPolicyValidationLimits::default();
    node_limits.max_total_resolved_text_nodes = 2;
    let nodes = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        node_limits,
    );
    assert_eq!(nodes.resolve(&TextSpec::Literal("a".to_owned())), Ok("a".to_owned()));
    assert_eq!(nodes.resolve(&TextSpec::Literal("b".to_owned())), Ok("b".to_owned()));
    assert_eq!(
        nodes.resolve(&TextSpec::Literal("c".to_owned())),
        Err(ContextError::TotalTextNodeLimit { nodes: 3, limit: 2 })
    );

    let mut step_limits = BuildPolicyValidationLimits::default();
    step_limits.max_resolver_steps = 2;
    let steps = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        step_limits,
    );
    assert_eq!(steps.resolve(&TextSpec::Literal("a".to_owned())), Ok("a".to_owned()));
    assert_eq!(steps.resolve(&TextSpec::Literal("b".to_owned())), Ok("b".to_owned()));
    assert_eq!(
        steps.resolve(&TextSpec::Literal("c".to_owned())),
        Err(ContextError::ResolverStepLimit { steps: 3, limit: 2 })
    );
}

#[test]
fn resolver_step_budget_rejects_a_wide_action_stack_before_reserving_it() {
    let context = fixture_context("x86_64", false, false);
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_nodes = 100_001;
    limits.max_resolver_steps = 1;
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );
    let value = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 100_000]);

    assert_eq!(
        resolver.resolve(&value),
        Err(ContextError::ResolverStepLimit {
            steps: 100_001,
            limit: 1,
        })
    );
}

#[test]
fn command_preflights_aggregate_items_before_building_argv() {
    let mut context = fixture_context("x86_64", false, false);
    context.environment.clear();
    let mut command = context.policy.sources.git.create_directory.clone();
    command.args = vec![TextSpec::Literal("a".to_owned()), TextSpec::Literal("b".to_owned())];
    command.environment.clear();
    command.working_dir = TextSpec::Literal("work".to_owned());

    context.limits.max_resolved_items = 4;
    context
        .resolve_command(&command, &TextContextOverlay::default())
        .unwrap();
    context.limits.max_resolved_items = 3;
    assert_eq!(
        context.resolve_command(&command, &TextContextOverlay::default()),
        Err(ContextError::ResolvedItemLimit { count: 4, limit: 3 })
    );
}

#[test]
fn fragment_boundaries_revalidate_commands_and_environment() {
    let mut context = fixture_context("x86_64", false, false);
    let mut command = context.policy.sources.git.create_directory.clone();
    let allowed_arguments = command.args.len();
    command.args.push(TextSpec::Literal("extra".to_owned()));
    context.limits.max_builder_arguments = allowed_arguments;
    assert!(matches!(
        context.resolve_command(&command, &TextContextOverlay::default()),
        Err(ContextError::PolicyValidation(BuildPolicyConversionError::CollectionLimit {
            field,
            count,
            limit,
        })) if field == "command.args" && count == allowed_arguments + 1 && limit == allowed_arguments
    ));

    context.limits = BuildPolicyValidationLimits::default();
    context.limits.max_environment_bindings = 1;
    let bindings = [
        EnvironmentBindingSpec {
            name: "ONE".to_owned(),
            value: TextSpec::Literal("1".to_owned()),
            condition: EnvironmentCondition::Always,
        },
        EnvironmentBindingSpec {
            name: "TWO".to_owned(),
            value: TextSpec::Literal("2".to_owned()),
            condition: EnvironmentCondition::Always,
        },
    ];
    assert!(matches!(
        context.extend_environment(&bindings),
        Err(ContextError::PolicyValidation(BuildPolicyConversionError::CollectionLimit {
            field,
            count: 2,
            limit: 1,
        })) if field == "environment"
    ));
}

#[test]
fn repeated_environment_extension_is_bounded_by_the_retained_final_state() {
    let mut context = fixture_context("x86_64", false, false);
    context.environment.clear();
    context.limits.max_resolved_items = 2;
    let binding = |name: &str| {
        [EnvironmentBindingSpec {
            name: name.to_owned(),
            value: TextSpec::Literal("x".to_owned()),
            condition: EnvironmentCondition::Always,
        }]
    };

    context.extend_environment(&binding("ONE")).unwrap();
    context.extend_environment(&binding("TWO")).unwrap();
    assert_eq!(context.environment.len(), 2);
    assert_eq!(
        context.extend_environment(&binding("THREE")),
        Err(ContextError::ResolvedItemLimit { count: 3, limit: 2 })
    );
    assert_eq!(context.environment.len(), 2);
}

#[test]
fn resolver_output_limit_accepts_n_and_rejects_n_plus_one() {
    let context = fixture_context("x86_64", false, false);
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_resolved_text_bytes = 3;
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );

    assert_eq!(
        resolver.resolve(&TextSpec::Literal("abc".to_owned())),
        Ok("abc".to_owned())
    );
    assert_eq!(
        resolver.resolve(&TextSpec::Literal("abcd".to_owned())),
        Err(ContextError::ResolvedTextBytesLimit { bytes: 4, limit: 3 })
    );
}

#[test]
fn resolver_wide_concat_limit_is_exact_and_linear() {
    let context = fixture_context("x86_64", false, false);
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_nodes = 10_001;
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );
    let at_limit = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 10_000]);
    assert_eq!(resolver.resolve(&at_limit).unwrap().len(), 10_000);

    let over_limit = TextSpec::Concat(vec![TextSpec::Literal("x".to_owned()); 10_001]);
    assert_eq!(
        resolver.resolve(&over_limit),
        Err(ContextError::TextNodeLimit {
            nodes: 10_002,
            limit: 10_001,
        })
    );
}

#[test]
fn resolver_rejects_deep_text_without_recursive_calls() {
    let context = fixture_context("x86_64", false, false);
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_nodes = 25_000;
    limits.max_text_depth = 64;
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );
    let mut value = TextSpec::Literal("x".to_owned());
    for _ in 0..20_000 {
        value = TextSpec::Concat(vec![value]);
    }

    assert_eq!(
        resolver.resolve(&value),
        Err(ContextError::TextDepthLimit { depth: 65, limit: 64 })
    );
}

#[test]
fn resolver_flag_limit_accepts_n_and_rejects_n_plus_one() {
    let mut context = fixture_context("x86_64", false, false);
    context.inputs.flags.c = vec![TextSpec::Literal("x".to_owned()); 3];
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_compiler_flags = 3;
    let value = TextSpec::Context(ContextValue::CFlags);
    {
        let resolver = TextResolver::new(
            &context.policy,
            &context.target,
            &context.inputs,
            TextContextOverlay::default(),
            limits,
        );
        assert_eq!(resolver.resolve(&value), Ok("x x x".to_owned()));
    }

    context.inputs.flags.c.push(TextSpec::Literal("x".to_owned()));
    let resolver = TextResolver::new(
        &context.policy,
        &context.target,
        &context.inputs,
        TextContextOverlay::default(),
        limits,
    );
    assert_eq!(
        resolver.resolve(&value),
        Err(ContextError::FlagCollectionLimit { count: 4, limit: 3 })
    );
}
