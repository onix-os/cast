#[test]
fn toolchain_commands_are_complete_exact_locked_and_cache_consistent() {
    let original = sample_plan();
    original.validate().unwrap();

    let mut missing = original.clone();
    missing.toolchain_commands.compilers.pop();
    assert!(matches!(
        missing.validate(),
        Err(DerivationValidationError::CompilerCommandCount {
            found: 12,
            expected: 13,
        })
    ));

    let mut reordered = original.clone();
    reordered.toolchain_commands.compilers.swap(0, 1);
    assert!(matches!(
        reordered.validate(),
        Err(DerivationValidationError::UnexpectedCompilerCommandRole {
            index: 0,
            expected: CompilerExecutableRole::Cc,
            found: CompilerExecutableRole::Cxx,
        })
    ));

    let mut mismatched = original.clone();
    mismatched.toolchain_commands.compilers[0].command.program.path = "/usr/bin/not-cmake".to_owned();
    assert!(matches!(
        mismatched.validate(),
        Err(DerivationValidationError::ExecutablePathMismatch { field, .. })
            if field == "toolchain_commands.compilers[0].command.program.path"
    ));

    let mut unlocked = original.clone();
    unlocked.toolchain_commands.compilers[0].command.program = ExecutablePlan {
        path: "/usr/bin/unlocked-compiler".to_owned(),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: "unlocked-compiler".to_owned(),
        },
    };
    assert!(matches!(
        unlocked.validate(),
        Err(DerivationValidationError::UnlockedExecutable { field, request })
            if field == "toolchain_commands.compilers[0].command.program.requirement"
                && request == "binary(unlocked-compiler)"
    ));

    let mut missing_cache = original.clone();
    missing_cache.execution.compiler_cache = true;
    assert!(matches!(
        missing_cache.validate(),
        Err(DerivationValidationError::CompilerCacheCommandMismatch {
            enabled: true,
            ccache: false,
            sccache: false,
        })
    ));

    let mut cached = original.clone();
    cached.execution.compiler_cache = true;
    cached.toolchain_commands.ccache = Some(sample_analyzer_tool("cmake"));
    cached.toolchain_commands.sccache = Some(sample_analyzer_tool("cmake"));
    let cache_origins = &mut cached
        .build_lock
        .requests
        .iter_mut()
        .find(|request| request.request == "binary(cmake)")
        .unwrap()
        .origins;
    cache_origins.extend([
        InputOrigin::CompilerCache {
            role: CompilerCacheRole::Ccache,
        },
        InputOrigin::CompilerCache {
            role: CompilerCacheRole::Sccache,
        },
    ]);
    cached.build_lock.normalize();
    cached.validate().unwrap();
    assert_ne!(original.derivation_id(), cached.derivation_id());

    let mut mold = original.clone();
    mold.toolchain_commands.mold = Some(ExecutableCommandPlan {
        program: sample_analyzer_tool("cmake"),
        args: vec!["--mold-identity".to_owned()],
    });
    mold.build_lock
        .requests
        .iter_mut()
        .find(|request| request.request == "binary(cmake)")
        .unwrap()
        .origins
        .push(InputOrigin::MoldLinker);
    mold.build_lock.normalize();
    mold.validate().unwrap();
    assert_ne!(original.derivation_id(), mold.derivation_id());

    let mut arguments = original.clone();
    arguments.toolchain_commands.compilers[0]
        .command
        .args
        .push("argument identity".to_owned());
    arguments.validate().unwrap();
    assert_ne!(original.derivation_id(), arguments.derivation_id());
}

#[test]
fn origin_only_role_changes_invalidate_the_derivation_identity() {
    let first = sample_plan();
    first.validate().unwrap();
    let mut changed = first.clone();
    let resolution = {
        let request = &mut changed.build_lock.requests[0];
        let resolution = (
            request.request.clone(),
            request.package_id.clone(),
            request.output.clone(),
        );
        request.origins = vec![InputOrigin::Check {
            selection: PackageInputSelection::Package,
            index: 0,
        }];
        resolution
    };
    changed.validate().unwrap();

    assert_eq!(
        (
            &changed.build_lock.requests[0].request,
            &changed.build_lock.requests[0].package_id,
            &changed.build_lock.requests[0].output,
        ),
        (&resolution.0, &resolution.1, &resolution.2)
    );
    assert_ne!(first.build_lock.digest(), changed.build_lock.digest());
    assert_ne!(first.derivation_id(), changed.derivation_id());
}

#[test]
fn shell_interpreter_and_declared_programs_change_identity() {
    let mut original = sample_plan();
    original.jobs[0].phases[0].steps = vec![StepPlan::Shell {
        interpreter: sample_analyzer_tool("bash"),
        declared_programs: vec![sample_analyzer_tool("cmake")],
        script: "cmake --build .".to_owned(),
        environment: BTreeMap::new(),
        working_dir: "/mason/build".to_owned(),
    }];
    original.validate().unwrap();
    let original_id = original.derivation_id();

    let mutations: [fn(&mut StepPlan); 4] = [
        |step: &mut StepPlan| {
            let StepPlan::Shell { interpreter, .. } = step else {
                unreachable!()
            };
            interpreter.path.push_str("-changed");
        },
        |step: &mut StepPlan| {
            let StepPlan::Shell { interpreter, .. } = step else {
                unreachable!()
            };
            interpreter.requirement.name.push_str("-changed");
        },
        |step: &mut StepPlan| {
            let StepPlan::Shell { declared_programs, .. } = step else {
                unreachable!()
            };
            declared_programs[0].path.push_str("-changed");
        },
        |step: &mut StepPlan| {
            let StepPlan::Shell { declared_programs, .. } = step else {
                unreachable!()
            };
            declared_programs[0].requirement.name.push_str("-changed");
        },
    ];
    for mutate in mutations {
        let mut changed = original.clone();
        mutate(&mut changed.jobs[0].phases[0].steps[0]);
        assert_ne!(original_id, changed.derivation_id());
    }
}

#[test]
fn run_built_is_contained_and_fully_identity_bearing() {
    let mut original = sample_plan();
    make_sample_run_built(&mut original);
    original.validate().unwrap();
    let original_id = original.derivation_id();

    let mut changed_path = original.clone();
    let StepPlan::RunBuilt { program, .. } = sample_step_mut(&mut changed_path) else {
        unreachable!()
    };
    *program = "/mason/build/bin/other-test".to_owned();
    changed_path.validate().unwrap();
    assert_ne!(original_id, changed_path.derivation_id());

    let mut changed_args = original.clone();
    let StepPlan::RunBuilt { args, .. } = sample_step_mut(&mut changed_args) else {
        unreachable!()
    };
    args.push("--thorough".to_owned());
    changed_args.validate().unwrap();
    assert_ne!(original_id, changed_args.derivation_id());

    for invalid in [
        "/mason/build",
        "/mason/other/self-test",
        "mason/build/bin/self-test",
        "/mason/build/../escape",
    ] {
        let mut plan = original.clone();
        let StepPlan::RunBuilt { program, .. } = sample_step_mut(&mut plan) else {
            unreachable!()
        };
        *program = invalid.to_owned();
        assert!(plan.validate().is_err(), "{invalid:?} escaped RunBuilt validation");
    }
}

#[test]
fn every_frozen_layout_value_changes_identity() {
    let original = sample_plan();
    let original_id = original.derivation_id();
    let mutations: Vec<(&str, Box<dyn Fn(&mut BuilderLayout)>)> = vec![
        ("hostname", Box::new(|layout| layout.hostname.push_str("-changed"))),
        ("guest-root", Box::new(|layout| layout.guest_root.push_str("-changed"))),
        (
            "artifacts-dir",
            Box::new(|layout| layout.artifacts_dir.push_str("-changed")),
        ),
        ("build-dir", Box::new(|layout| layout.build_dir.push_str("-changed"))),
        ("source-dir", Box::new(|layout| layout.source_dir.push_str("-changed"))),
        ("recipe-dir", Box::new(|layout| layout.recipe_dir.push_str("-changed"))),
        (
            "install-dir",
            Box::new(|layout| layout.install_dir.push_str("-changed")),
        ),
        (
            "package-dir",
            Box::new(|layout| layout.package_dir.push_str("-changed")),
        ),
        ("ccache-dir", Box::new(|layout| layout.ccache_dir.push_str("-changed"))),
        (
            "sccache-dir",
            Box::new(|layout| layout.sccache_dir.push_str("-changed")),
        ),
        (
            "go-cache-dir",
            Box::new(|layout| layout.go_cache_dir.push_str("-changed")),
        ),
        (
            "go-mod-cache-dir",
            Box::new(|layout| layout.go_mod_cache_dir.push_str("-changed")),
        ),
        (
            "cargo-cache-dir",
            Box::new(|layout| layout.cargo_cache_dir.push_str("-changed")),
        ),
        (
            "zig-cache-dir",
            Box::new(|layout| layout.zig_cache_dir.push_str("-changed")),
        ),
    ];

    for (name, mutate) in mutations {
        let mut changed = original.clone();
        mutate(&mut changed.layout);
        assert_ne!(original_id, changed.derivation_id(), "{name} mutation was not hashed");
    }
}

#[test]
fn non_default_frozen_layout_is_valid_and_changes_identity() {
    let original = sample_plan();
    let mut changed = original.clone();
    changed.layout = BuilderLayout {
        hostname: "forge-builder".to_owned(),
        guest_root: "/forge".to_owned(),
        artifacts_dir: "/forge/output".to_owned(),
        build_dir: "/forge/work".to_owned(),
        source_dir: "/forge/sources".to_owned(),
        recipe_dir: "/forge/recipe".to_owned(),
        install_dir: "/forge/destination".to_owned(),
        package_dir: "/forge/recipe/package".to_owned(),
        ccache_dir: "/forge/cache-cc".to_owned(),
        sccache_dir: "/forge/cache-rust".to_owned(),
        go_cache_dir: "/forge/cache-go".to_owned(),
        go_mod_cache_dir: "/forge/cache-go-mod".to_owned(),
        cargo_cache_dir: "/forge/cache-cargo".to_owned(),
        zig_cache_dir: "/forge/cache-zig".to_owned(),
    };
    changed.jobs[0].build_dir = "/forge/work".to_owned();
    changed.jobs[0].work_dir = "/forge/work/hello".to_owned();
    let StepPlan::Run { working_dir, .. } = &mut changed.jobs[0].phases[0].steps[0] else {
        unreachable!()
    };
    *working_dir = "/forge/work".to_owned();
    changed.environment.insert("HOME".to_owned(), "/forge/work".to_owned());

    changed.validate().unwrap();
    assert_ne!(original.derivation_id(), changed.derivation_id());
}

#[test]
fn phase_order_remains_semantic() {
    let mut first = sample_plan();
    first.jobs.push(JobPlan {
        pgo_stage: Some("use".to_owned()),
        pgo_dir: Some("/mason/build-pgo".to_owned()),
        build_dir: "/mason/build".to_owned(),
        work_dir: "/mason/build/hello".to_owned(),
        phases: Vec::new(),
    });
    let mut reordered = first.clone();
    reordered.jobs.reverse();

    assert_ne!(first.derivation_id(), reordered.derivation_id());
}

#[test]
fn validation_requires_normalized_non_root_absolute_layout_paths() {
    for value in [
        "relative/build",
        "/",
        "/mason/../escape",
        "/mason/./build",
        "/mason//build",
        "/mason/build/",
    ] {
        let mut plan = sample_plan();
        plan.layout.build_dir = value.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsafeAbsolutePath { field, value: found })
                if field == "layout.build_dir" && found == value
        ));
    }
}

#[test]
fn validation_rejects_invalid_hostnames_and_overlapping_layout_paths() {
    for hostname in ["", "-builder", "builder-", "bad host", "bad/host"] {
        let mut plan = sample_plan();
        plan.layout.hostname = hostname.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidSandboxHostname { value }) if value == hostname
        ));
    }

    let mut overlapping = sample_plan();
    overlapping.layout.ccache_dir = "/mason/build/cache".to_owned();
    assert!(matches!(
        overlapping.validate(),
        Err(DerivationValidationError::OverlappingLayoutPath {
            field,
            other_field,
            ..
        }) if field == "layout.ccache_dir" && other_field == "layout.build_dir"
    ));
}

#[test]
fn validation_contains_layout_and_job_paths_in_their_frozen_roots() {
    let mut outside_layout = sample_plan();
    outside_layout.layout.source_dir = "/outside/sources".to_owned();
    assert!(matches!(
        outside_layout.validate(),
        Err(DerivationValidationError::PathOutsideRoot { field, root, .. })
            if field == "layout.source_dir" && root == "/mason"
    ));

    let mut outside_layout_build = sample_plan();
    outside_layout_build.jobs[0].build_dir = "/outside/build".to_owned();
    outside_layout_build.jobs[0].work_dir = "/outside/build/work".to_owned();
    assert!(matches!(
        outside_layout_build.validate(),
        Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
            if field == "jobs[0].build_dir" && root_field == "layout.build_dir"
    ));

    let mut outside_job_build = sample_plan();
    outside_job_build.jobs[0].work_dir = "/mason/other".to_owned();
    assert!(matches!(
        outside_job_build.validate(),
        Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
            if field == "jobs[0].work_dir" && root_field == "jobs[0].build_dir"
    ));

    let mut outside_pgo = sample_plan();
    outside_pgo.jobs[0].pgo_stage = Some("one".to_owned());
    outside_pgo.jobs[0].pgo_dir = Some("/outside/pgo".to_owned());
    assert!(matches!(
        outside_pgo.validate(),
        Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
            if field == "jobs[0].pgo_dir" && root_field == "layout.build_dir"
    ));
}

#[test]
fn validation_rejects_traversal_and_escape_in_every_step_working_directory() {
    for working_dir in [
        "relative",
        "/mason/build/../outside",
        "/mason/build//nested",
        "/mason/install",
    ] {
        let mut plan = sample_plan();
        let StepPlan::Run {
            working_dir: frozen, ..
        } = &mut plan.jobs[0].phases[0].steps[0]
        else {
            unreachable!()
        };
        *frozen = working_dir.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsafeAbsolutePath { .. })
                | Err(DerivationValidationError::PathOutsideRoot { .. })
        ));
    }

    let mut shell_plan = sample_plan();
    shell_plan.jobs[0].phases[0].steps = vec![StepPlan::Shell {
        interpreter: ExecutablePlan {
            path: "/usr/bin/bash".to_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: "bash".to_owned(),
            },
        },
        declared_programs: Vec::new(),
        script: "true".to_owned(),
        environment: BTreeMap::new(),
        working_dir: "/tmp/ambient".to_owned(),
    }];
    assert!(matches!(
        shell_plan.validate(),
        Err(DerivationValidationError::PathOutsideRoot { field, .. })
            if field == "jobs[0].phases[0].steps[0].working_dir"
    ));
}

#[test]
fn validation_freezes_only_the_executable_phase_vocabulary() {
    let mut supported = sample_plan();
    supported.jobs[0].phases = ["Prepare", "setup", "BUILD", "install", "check", "workload"]
        .into_iter()
        .map(|name| PhasePlan {
            name: name.to_owned(),
            pre: Vec::new(),
            steps: Vec::new(),
            post: Vec::new(),
        })
        .collect();
    supported.validate().unwrap();

    for name in ["environment", "ambient-phase", ""] {
        let mut plan = sample_plan();
        plan.jobs[0].phases[0].name = name.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsupportedPhase {
                job: 0,
                phase: 0,
                name: found,
            }) if found == name
        ));
    }

    let mut duplicate = sample_plan();
    duplicate.jobs[0].phases.push(PhasePlan {
        name: "BUILD".to_owned(),
        pre: Vec::new(),
        steps: Vec::new(),
        post: Vec::new(),
    });
    assert!(matches!(
        duplicate.validate(),
        Err(DerivationValidationError::DuplicatePhase { job: 0, .. })
    ));
}

#[test]
fn validation_requires_exact_pgo_vocabulary_and_stage_directory_pairing() {
    for stage in ["one", "two", "use"] {
        let mut plan = sample_plan();
        plan.jobs[0].pgo_stage = Some(stage.to_owned());
        plan.jobs[0].pgo_dir = Some("/mason/build/profile".to_owned());
        plan.validate().unwrap();
    }

    let mut unsupported = sample_plan();
    unsupported.jobs[0].pgo_stage = Some("ONE".to_owned());
    unsupported.jobs[0].pgo_dir = Some("/mason/build/profile".to_owned());
    assert!(matches!(
        unsupported.validate(),
        Err(DerivationValidationError::UnsupportedPgoStage { job: 0, stage })
            if stage == "ONE"
    ));

    for (stage, directory) in [
        (Some("one".to_owned()), None),
        (None, Some("/mason/build/profile".to_owned())),
    ] {
        let mut plan = sample_plan();
        plan.jobs[0].pgo_stage = stage;
        plan.jobs[0].pgo_dir = directory;
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::PgoStageDirectoryMismatch { job: 0, .. })
        ));
    }
}

#[test]
fn validation_rejects_output_relations_outside_the_locked_closure() {
    let mut plan = sample_plan();
    plan.outputs[0].runtime_inputs.push(OutputRelation::Locked {
        relation: RelationPlan {
            kind: RelationKind::PackageName,
            name: "missing".to_owned(),
        },
        reference: LockedOutputRef {
            package_id: "missing".to_owned(),
            output: "out".to_owned(),
        },
    });

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::UnknownOutputReference { field, .. })
            if field == "outputs[0].runtime_inputs[0]"
    ));
}

#[test]
fn validation_rejects_duplicate_emitted_package_names() {
    let mut plan = sample_plan();
    let mut duplicate = plan.outputs[0].clone();
    duplicate.name = "dev".to_owned();
    plan.outputs.push(duplicate);

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::DuplicateOutputPackage { package })
            if package == "hello"
    ));
}

#[test]
fn validation_binds_artifact_architecture_to_the_frozen_target_platform() {
    let mut plan = sample_plan();
    plan.package.architecture = "x86".to_owned();

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::ArtifactTargetArchitectureMismatch {
            artifact,
            target,
        }) if artifact == "x86" && target == "x86_64"
    ));
}

#[test]
fn validation_rejects_source_materialization_path_escape() {
    for value in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "nested/file",
        "nested\\file",
        "line\nbreak",
        "escape\u{1b}",
    ] {
        let mut plan = sample_plan();
        let LockedSource::Archive { filename, .. } = &mut plan.sources[0] else {
            unreachable!()
        };
        *filename = value.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsafeSourceDestination {
                index: 0,
                field: "filename",
                ..
            })
        ));
    }
}

