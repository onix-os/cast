#[test]
fn typed_relations_lower_to_both_stone_roles_without_reparsing() {
    for kind in [
        StoneRelationKind::PackageName,
        StoneRelationKind::SharedLibrary,
        StoneRelationKind::PkgConfig,
        StoneRelationKind::Interpreter,
        StoneRelationKind::CMake,
        StoneRelationKind::Python,
        StoneRelationKind::Binary,
        StoneRelationKind::SystemBinary,
        StoneRelationKind::PkgConfig32,
    ] {
        let dependency = Dependency::new(kind, "target(with-nesting)").unwrap();
        let relation = RelationPlan::from(&dependency);
        assert_eq!(relation.to_dependency(), dependency);
        assert_eq!(relation.to_provider().kind, dependency.kind);
        assert_eq!(relation.to_provider().name, dependency.name);
    }
}

#[test]
fn validation_rejects_unsupported_artifact_architecture_at_freeze() {
    let mut plan = sample_plan();
    plan.package.architecture = "mips64".to_owned();
    plan.build_lock.target_platform.architecture = "mips64".to_owned();

    let error = plan.validate().unwrap_err();
    assert!(matches!(
        error,
        DerivationValidationError::UnsupportedArtifactArchitecture { ref value, .. }
            if value == "mips64"
    ));
    assert_eq!(
        error.to_string(),
        "package.architecture: unsupported Stone artifact architecture \"mips64\"; expected one of x86_64, x86, aarch64, riscv64"
    );
}

#[test]
fn validation_rejects_every_invalid_output_exclusion_before_freeze() {
    for (field, mutate) in [
        (
            "outputs[0].provides_exclude[0]",
            Box::new(|plan: &mut DerivationPlan| plan.outputs[0].provides_exclude.push("(".to_owned()))
                as Box<dyn Fn(&mut DerivationPlan)>,
        ),
        (
            "outputs[0].runtime_exclude[0]",
            Box::new(|plan: &mut DerivationPlan| plan.outputs[0].runtime_exclude.push("[".to_owned())),
        ),
    ] {
        let mut plan = sample_plan();
        mutate(&mut plan);
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidRegex { field: actual, .. })
                if actual == field
        ));
    }
}

#[test]
fn validation_rejects_invalid_collection_globs_before_freeze() {
    let mut plan = sample_plan();
    plan.collection_rules[1].pattern = "[".to_owned();

    let error = plan.validate().unwrap_err();
    assert!(matches!(
        error,
        DerivationValidationError::InvalidGlob { ref field, .. }
            if field == "collection_rules[1].pattern"
    ));
    assert!(error.to_string().contains("collection_rules[1].pattern"));
}

#[test]
fn validation_requires_the_explicit_root_output_but_allows_empty_splits() {
    let mut missing = sample_plan();
    missing.outputs[0].name = "dev".to_owned();
    for rule in &mut missing.collection_rules {
        rule.output = "dev".to_owned();
    }
    assert!(matches!(
        missing.validate(),
        Err(DerivationValidationError::MissingRootOutput)
    ));

    let mut mismatched = sample_plan();
    mismatched.outputs[0].package_name = "other".to_owned();
    assert!(matches!(
        mismatched.validate(),
        Err(DerivationValidationError::RootOutputPackageMismatch {
            index: 0,
            expected,
            found,
        }) if expected == "hello" && found == "other"
    ));

    let mut excluded = sample_plan();
    excluded.outputs[0].include_in_manifest = false;
    assert!(matches!(
        excluded.validate(),
        Err(DerivationValidationError::RootOutputExcludedFromManifest { index: 0 })
    ));

    let mut empty_split = sample_plan();
    empty_split.outputs.push(OutputPlan {
        name: "empty".to_owned(),
        package_name: "hello-empty".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    });
    empty_split.validate().unwrap();
}

#[test]
fn validation_rejects_invalid_typed_relation_targets_with_exact_fields() {
    let mut manifest = sample_plan();
    manifest.manifest_build_inputs[0].name.clear();
    assert!(matches!(
        manifest.validate(),
        Err(DerivationValidationError::InvalidRelation { field, .. })
            if field == "manifest_build_inputs[0]"
    ));

    let mut conflict = sample_plan();
    conflict.outputs[0].conflicts[0].name = "unbalanced)".to_owned();
    assert!(matches!(
        conflict.validate(),
        Err(DerivationValidationError::InvalidRelation { field, .. })
            if field == "outputs[0].conflicts[0]"
    ));
}

#[test]
fn analyzer_handler_order_is_semantic_while_output_order_is_not() {
    let mut first = sample_plan();
    first.outputs.push(OutputPlan {
        name: "dev".to_owned(),
        package_name: "hello-devel".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    });
    let mut outputs_reordered = first.clone();
    outputs_reordered.outputs.reverse();

    assert_eq!(first.canonical_bytes(), outputs_reordered.canonical_bytes());
    assert_eq!(first.derivation_id(), outputs_reordered.derivation_id());

    let mut handlers_reordered = first.clone();
    handlers_reordered.analysis.handlers.swap(0, 1);

    assert_ne!(first.canonical_bytes(), handlers_reordered.canonical_bytes());
    assert_ne!(first.derivation_id(), handlers_reordered.derivation_id());
}

#[test]
fn analysis_handler_validation_repeats_policy_invariants() {
    let mut empty = sample_plan();
    empty.analysis.handlers.clear();
    assert!(matches!(
        empty.validate(),
        Err(DerivationValidationError::Empty { field }) if field == "analysis.handlers"
    ));

    let mut duplicate = sample_plan();
    duplicate.analysis.handlers.insert(1, AnalyzerKind::Elf);
    assert!(matches!(
        duplicate.validate(),
        Err(DerivationValidationError::DuplicateAnalyzer { name }) if name == "Elf"
    ));

    let mut missing = sample_plan();
    missing.analysis.handlers.pop();
    assert!(matches!(
        missing.validate(),
        Err(DerivationValidationError::MissingAnalyzer { name }) if name == "IncludeAny"
    ));

    let mut misplaced = sample_plan();
    misplaced.analysis.handlers.swap(0, 2);
    assert!(matches!(
        misplaced.validate(),
        Err(DerivationValidationError::AnalyzerMustBeLast { name }) if name == "IncludeAny"
    ));
}

#[test]
fn analyzer_tool_validation_is_exact_and_fail_closed() {
    let mut missing = sample_plan();
    missing.analysis.tools.python = None;
    assert!(matches!(
        missing.validate(),
        Err(DerivationValidationError::MissingAnalyzerTool { field })
            if field == "analysis.tools.python"
    ));

    let mut unexpected = sample_plan();
    unexpected.analysis.tools.pkg_config = Some(sample_analyzer_tool("pkg-config"));
    assert!(matches!(
        unexpected.validate(),
        Err(DerivationValidationError::UnexpectedAnalyzerTool { field })
            if field == "analysis.tools.pkg_config"
    ));

    let mut non_executable = sample_plan();
    non_executable.analysis.tools.python.as_mut().unwrap().requirement.kind = RelationKind::PkgConfig;
    assert!(matches!(
        non_executable.validate(),
        Err(DerivationValidationError::ExecutableRequirementNotRunnable { field, .. })
            if field == "analysis.tools.python.requirement"
    ));

    let mut unsafe_name = sample_plan();
    unsafe_name.analysis.tools.python.as_mut().unwrap().requirement.name = "../python3".to_owned();
    assert!(matches!(
        unsafe_name.validate(),
        Err(DerivationValidationError::InvalidExecutableRequirement { field, .. })
            if field == "analysis.tools.python.requirement"
    ));

    let mut program_mismatch = sample_plan();
    program_mismatch.analysis.tools.python.as_mut().unwrap().path = "/usr/bin/not-python".to_owned();
    assert!(matches!(
        program_mismatch.validate(),
        Err(DerivationValidationError::ExecutablePathMismatch { field, .. })
            if field == "analysis.tools.python.path"
    ));

    let mut unlocked = sample_plan();
    let python = unlocked.analysis.tools.python.as_mut().unwrap();
    python.requirement.name = "unlocked-python".to_owned();
    python.path = "/usr/bin/unlocked-python".to_owned();
    assert!(matches!(
        unlocked.validate(),
        Err(DerivationValidationError::UnlockedExecutable { field, request })
            if field == "analysis.tools.python.requirement" && request == "binary(unlocked-python)"
    ));
}

#[test]
fn every_structural_executable_is_path_bound_and_exactly_locked() {
    for path in ["cmake", "/", "/usr/bin/../bin/cmake", "/usr/bin//cmake"] {
        let mut plan = sample_plan();
        let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        program.path = path.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
                if field == "jobs[0].phases[0].steps[0].program.path"
        ));
    }

    let mut unsupported = sample_plan();
    let StepPlan::Run { program, .. } = &mut unsupported.jobs[0].phases[0].steps[0] else {
        unreachable!()
    };
    program.requirement.kind = RelationKind::PkgConfig;
    assert!(matches!(
        unsupported.validate(),
        Err(DerivationValidationError::ExecutableRequirementNotRunnable { field, .. })
            if field == "jobs[0].phases[0].steps[0].program.requirement"
    ));

    let mut mismatched = sample_plan();
    let StepPlan::Run { program, .. } = &mut mismatched.jobs[0].phases[0].steps[0] else {
        unreachable!()
    };
    program.requirement.kind = RelationKind::SystemBinary;
    assert!(matches!(
        mismatched.validate(),
        Err(DerivationValidationError::ExecutablePathMismatch { field, expected, .. })
            if field == "jobs[0].phases[0].steps[0].program.path" && expected == "/usr/sbin/cmake"
    ));

    let mut unlocked_run = sample_plan();
    let StepPlan::Run { program, .. } = &mut unlocked_run.jobs[0].phases[0].steps[0] else {
        unreachable!()
    };
    program.path = "/usr/bin/unlocked-run".to_owned();
    program.requirement.name = "unlocked-run".to_owned();
    assert!(matches!(
        unlocked_run.validate(),
        Err(DerivationValidationError::UnlockedExecutable { field, request })
            if field == "jobs[0].phases[0].steps[0].program.requirement"
                && request == "binary(unlocked-run)"
    ));

    let shell = |interpreter: ExecutablePlan, declared_programs: Vec<ExecutablePlan>| StepPlan::Shell {
        interpreter,
        declared_programs,
        script: "true".to_owned(),
        environment: BTreeMap::new(),
        working_dir: "/mason/build".to_owned(),
    };
    let mut unlocked_interpreter = sample_plan();
    unlocked_interpreter.jobs[0].phases[0].steps = vec![shell(
        ExecutablePlan {
            path: "/usr/bin/unlocked-shell".to_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: "unlocked-shell".to_owned(),
            },
        },
        Vec::new(),
    )];
    assert!(matches!(
        unlocked_interpreter.validate(),
        Err(DerivationValidationError::UnlockedExecutable { field, .. })
            if field == "jobs[0].phases[0].steps[0].interpreter.requirement"
    ));

    let mut unlocked_declared = sample_plan();
    unlocked_declared.jobs[0].phases[0].steps = vec![shell(
        sample_analyzer_tool("bash"),
        vec![ExecutablePlan {
            path: "/usr/bin/unlocked-declared".to_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: "unlocked-declared".to_owned(),
            },
        }],
    )];
    assert!(matches!(
        unlocked_declared.validate(),
        Err(DerivationValidationError::UnlockedExecutable { field, .. })
            if field == "jobs[0].phases[0].steps[0].declared_programs[0].requirement"
    ));
}

#[test]
fn unusual_program_paths_require_an_explicit_package_capability() {
    let mut plan = sample_plan();
    plan.build_lock.requests.push(LockedRequest {
        request: "odd-tool".to_owned(),
        package_id: "hello-id".to_owned(),
        output: "out".to_owned(),
        origins: vec![InputOrigin::JobExecutable {
            job: 0,
            phase: 0,
            phase_name: "build".to_owned(),
            section: JobStepSection::Steps,
            step: 0,
            role: JobExecutableRole::RunProgram,
        }],
    });
    plan.build_lock.normalize();
    {
        let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        *program = ExecutablePlan {
            path: "/opt/odd/bin/tool".to_owned(),
            requirement: RelationPlan {
                kind: RelationKind::PackageName,
                name: "odd-tool".to_owned(),
            },
        };
    }
    plan.validate().unwrap();

    let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
        unreachable!()
    };
    program.path = "/usr/bin/tool".to_owned();
    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::AmbiguousPackageExecutable { field, .. })
            if field == "jobs[0].phases[0].steps[0].program.path"
    ));
}

#[test]
fn every_required_semantic_mutation_changes_identity() {
    let original = sample_plan();
    let original_id = original.derivation_id();
    let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
        ("cast-version", Box::new(|plan| plan.cast_version.push_str("-changed"))),
        (
            "cast-implementation",
            Box::new(|plan| plan.cast_fingerprint.push_str("-changed")),
        ),
        (
            "source",
            Box::new(|plan| match &mut plan.sources[0] {
                LockedSource::Archive { sha256, .. } => sha256.push_str("-changed"),
                LockedSource::Git { .. } => unreachable!(),
            }),
        ),
        (
            "source-materialization",
            Box::new(|plan| match &mut plan.sources[0] {
                LockedSource::Archive { filename, .. } => filename.push_str("-changed"),
                LockedSource::Git { .. } => unreachable!(),
            }),
        ),
        (
            "dependency",
            Box::new(|plan| plan.build_lock.packages[0].package_id.push_str("-changed")),
        ),
        (
            "input-origin",
            Box::new(|plan| {
                plan.build_lock.requests[0].origins[0] = InputOrigin::Check {
                    selection: PackageInputSelection::Package,
                    index: 0,
                };
            }),
        ),
        (
            "target-platform",
            Box::new(|plan| plan.build_lock.target_platform.architecture = "aarch64".to_owned()),
        ),
        (
            "policy",
            Box::new(|plan| plan.build_lock.policy.fingerprint.push_str("-changed")),
        ),
        (
            "target-policy",
            Box::new(|plan| plan.build_lock.target.fingerprint.push_str("-changed")),
        ),
        (
            "profile",
            Box::new(|plan| plan.build_lock.profile.fingerprint.push_str("-changed")),
        ),
        (
            "toolchain",
            Box::new(|plan| plan.build_lock.toolchain.fingerprint.push_str("-changed")),
        ),
        (
            "builder",
            Box::new(|plan| plan.build_lock.builder.fingerprint.push_str("-changed")),
        ),
        (
            "phase",
            Box::new(|plan| match &mut plan.jobs[0].phases[0].steps[0] {
                StepPlan::Run { args, .. } => args.push("--verbose".to_owned()),
                StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => {
                    unreachable!()
                }
            }),
        ),
        (
            "step-program-path",
            Box::new(|plan| {
                let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                    unreachable!()
                };
                program.path.push_str("-changed");
            }),
        ),
        (
            "step-program-requirement",
            Box::new(|plan| {
                let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                    unreachable!()
                };
                program.requirement.name.push_str("-changed");
            }),
        ),
        (
            "compiler-command-path",
            Box::new(|plan| {
                plan.toolchain_commands.compilers[0]
                    .command
                    .program
                    .path
                    .push_str("-changed");
            }),
        ),
        (
            "compiler-command-requirement",
            Box::new(|plan| {
                plan.toolchain_commands.compilers[0]
                    .command
                    .program
                    .requirement
                    .name
                    .push_str("-changed");
            }),
        ),
        (
            "compiler-command-argument",
            Box::new(|plan| {
                plan.toolchain_commands.compilers[0]
                    .command
                    .args
                    .push("--identity".to_owned());
            }),
        ),
        (
            "environment",
            Box::new(|plan| {
                plan.environment.insert("LANG".to_owned(), "C".to_owned());
            }),
        ),
        (
            "root-materialization",
            Box::new(|plan| {
                plan.execution.root_materialization = RootMaterializationMode::PackageManagerState;
            }),
        ),
        (
            "credentials",
            Box::new(|plan| plan.execution.credentials = ExecutionCredentials::Unspecified),
        ),
        (
            "executor",
            Box::new(|plan| plan.execution.executor.fingerprint.push_str("-changed")),
        ),
        (
            "package-metadata",
            Box::new(|plan| plan.package.homepage.push_str("/changed")),
        ),
        (
            "package-architecture",
            Box::new(|plan| plan.package.architecture = "aarch64".to_owned()),
        ),
        ("analysis", Box::new(|plan| plan.analysis.strip = !plan.analysis.strip)),
        (
            "analysis-tool-program",
            Box::new(|plan| {
                plan.analysis.tools.python.as_mut().unwrap().path.push_str("-changed");
            }),
        ),
        (
            "analysis-tool-requirement",
            Box::new(|plan| {
                plan.analysis
                    .tools
                    .python
                    .as_mut()
                    .unwrap()
                    .requirement
                    .name
                    .push_str("-changed");
            }),
        ),
        (
            "manifest-build-input-name",
            Box::new(|plan| plan.manifest_build_inputs[0].name.push_str("-changed")),
        ),
        (
            "manifest-build-input-kind",
            Box::new(|plan| plan.manifest_build_inputs[0].kind = RelationKind::SystemBinary),
        ),
        (
            "collection-rule-order",
            Box::new(|plan| plan.collection_rules.reverse()),
        ),
        (
            "collection-rule-kind",
            Box::new(|plan| plan.collection_rules[0].kind = PathRuleKind::Special),
        ),
        (
            "output",
            Box::new(|plan| plan.outputs[0].conflicts[0].name.push_str("-changed")),
        ),
        (
            "output-manifest-membership",
            Box::new(|plan| plan.outputs[0].include_in_manifest = false),
        ),
        ("timestamp", Box::new(|plan| plan.source_date_epoch += 1)),
    ];

    for (name, mutate) in mutations {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert_ne!(original_id, changed.derivation_id(), "{name} mutation was not hashed");
    }
}

