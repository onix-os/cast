#[test]
fn identical_plans_have_identical_bytes_and_ids() {
    let first = sample_plan();
    let repeated = sample_plan();

    assert_eq!(first.canonical_bytes(), repeated.canonical_bytes());
    assert_eq!(first.derivation_id(), repeated.derivation_id());
    assert_eq!(first.derivation_id().as_str().len(), 64);
    first.validate().unwrap();
}

#[test]
fn validation_rejects_pre_structural_archive_schema_fourteen() {
    let mut plan = sample_plan();
    plan.schema_version = 14;

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::UnsupportedSchema {
            found: 14,
            expected: DERIVATION_PLAN_SCHEMA_VERSION,
        })
    ));
}

#[test]
fn structural_archive_steps_are_prepare_only_locked_bounded_and_nonoverlapping() {
    let mut valid = sample_plan();
    insert_prepare_archive_steps(&mut valid, vec![archive_step(0, "vendor/source", 1)]);
    valid.validate().unwrap();

    let mut hook = sample_plan();
    hook.jobs[0].phases.insert(
        0,
        PhasePlan {
            name: "Prepare".to_owned(),
            pre: vec![archive_step(0, "source", 1)],
            steps: Vec::new(),
            post: Vec::new(),
        },
    );
    assert!(matches!(
        hook.validate(),
        Err(DerivationValidationError::ArchiveStepOutsidePrepare { ref field })
            if field == "jobs[0].phases[0].pre[0]"
    ));

    let mut wrong_kind = sample_plan();
    wrong_kind.sources.push(sample_git_source(1, "git-source"));
    insert_prepare_archive_steps(&mut wrong_kind, vec![archive_step(1, "source", 1)]);
    assert!(matches!(
        wrong_kind.validate(),
        Err(DerivationValidationError::InvalidArchiveStepSource { source_index: 1, .. })
    ));

    let mut unsafe_destination = sample_plan();
    insert_prepare_archive_steps(&mut unsafe_destination, vec![archive_step(0, "../source", 1)]);
    assert!(matches!(
        unsafe_destination.validate(),
        Err(DerivationValidationError::UnsafeArchiveStepDestination { .. })
    ));

    let mut excessive_strip = sample_plan();
    insert_prepare_archive_steps(&mut excessive_strip, vec![archive_step(0, "source", 129)]);
    assert!(matches!(
        excessive_strip.validate(),
        Err(DerivationValidationError::ArchiveStripComponentsLimit {
            found: 129,
            limit: 128,
            ..
        })
    ));

    let mut overlapping = sample_plan();
    insert_prepare_archive_steps(
        &mut overlapping,
        vec![archive_step(0, "source", 1), archive_step(0, "source/nested", 1)],
    );
    assert!(matches!(
        overlapping.validate(),
        Err(DerivationValidationError::OverlappingArchiveDestinations { job: 0 })
    ));
}

#[test]
fn archive_destinations_cannot_merge_with_git_sources_in_either_source_order() {
    let mut archive_first = sample_plan();
    archive_first.sources.push(sample_git_source(1, "source"));
    insert_prepare_archive_steps(&mut archive_first, vec![archive_step(0, "source/nested", 1)]);

    let mut git_first = sample_plan();
    git_first.sources = vec![
        sample_git_source(0, "source"),
        LockedSource::Archive {
            order: 1,
            url: "https://example.invalid/hello.tar.zst".to_owned(),
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            filename: "hello.tar.zst".to_owned(),
        },
    ];
    insert_prepare_archive_steps(&mut git_first, vec![archive_step(1, "source", 1)]);

    for plan in [archive_first, git_first] {
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::ArchiveDestinationOverlapsGitSource {
                job: 0,
                ref destination,
                ref directory,
                ..
            }) if destination.starts_with("source") && directory == "source"
        ));
    }
}

#[test]
fn validation_rejects_embedded_nul_in_every_process_data_class() {
    let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
        (
            "environment[0].name",
            Box::new(|plan| {
                plan.environment.clear();
                plan.environment.insert("BAD\0NAME".to_owned(), String::new());
            }),
        ),
        (
            "environment[0].value",
            Box::new(|plan| {
                plan.environment.clear();
                plan.environment.insert("GOOD".to_owned(), "bad\0value".to_owned());
            }),
        ),
        (
            "layout.build_dir",
            Box::new(|plan| plan.layout.build_dir = "/mason/bu\0ild".to_owned()),
        ),
        (
            "jobs[0].pgo_dir",
            Box::new(|plan| {
                plan.jobs[0].pgo_stage = Some("one".to_owned());
                plan.jobs[0].pgo_dir = Some("/mason/pgo\0one".to_owned());
            }),
        ),
        (
            "toolchain_commands.compilers[0].command.program.path",
            Box::new(|plan| {
                plan.toolchain_commands.compilers[0].command.program.path = "/usr/bin/cm\0ake".to_owned();
            }),
        ),
        (
            "toolchain_commands.compilers[0].command.args[0]",
            Box::new(|plan| {
                plan.toolchain_commands.compilers[0]
                    .command
                    .args
                    .push("bad\0argument".to_owned());
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].program.path",
            Box::new(|plan| {
                let StepPlan::Run { program, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                program.path = "/usr/bin/cm\0ake".to_owned();
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].args[0]",
            Box::new(|plan| {
                let StepPlan::Run { args, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                args[0] = "--bu\0ild".to_owned();
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].environment[0].name",
            Box::new(|plan| {
                let StepPlan::Run { environment, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                environment.clear();
                environment.insert("BAD\0NAME".to_owned(), String::new());
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].environment[0].value",
            Box::new(|plan| {
                let StepPlan::Run { environment, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                environment.clear();
                environment.insert("GOOD".to_owned(), "bad\0value".to_owned());
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].working_dir",
            Box::new(|plan| {
                let StepPlan::Run { working_dir, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                *working_dir = "/mason/bu\0ild".to_owned();
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].interpreter.path",
            Box::new(|plan| {
                make_sample_shell(plan);
                let StepPlan::Shell { interpreter, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                interpreter.path = "/usr/bin/ba\0sh".to_owned();
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].declared_programs[0].path",
            Box::new(|plan| {
                make_sample_shell(plan);
                let StepPlan::Shell { declared_programs, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                declared_programs[0].path = "/usr/bin/cm\0ake".to_owned();
            }),
        ),
        (
            "jobs[0].phases[0].steps[0].script",
            Box::new(|plan| {
                make_sample_shell(plan);
                let StepPlan::Shell { script, .. } = sample_step_mut(plan) else {
                    unreachable!()
                };
                *script = "printf bad\0script".to_owned();
            }),
        ),
    ];

    for (expected_field, mutate) in mutations {
        let mut plan = sample_plan();
        mutate(&mut plan);
        assert!(
            matches!(
                plan.validate(),
                Err(DerivationValidationError::EmbeddedNul { field }) if field == expected_field
            ),
            "NUL in {expected_field} crossed the freeze boundary"
        );
    }
}

#[test]
fn validation_requires_portable_environment_names_globally_and_per_step() {
    for invalid in ["", "9STARTS_WITH_DIGIT", "HAS-DASH", "HAS=EQUALS", "NÖN_ASCII"] {
        let mut global = sample_plan();
        global.environment.clear();
        global.environment.insert(invalid.to_owned(), String::new());
        assert!(matches!(
            global.validate(),
            Err(DerivationValidationError::InvalidEnvironmentName { field })
                if field == "environment[0].name"
        ));

        let mut local = sample_plan();
        let StepPlan::Run { environment, .. } = sample_step_mut(&mut local) else {
            unreachable!()
        };
        environment.clear();
        environment.insert(invalid.to_owned(), String::new());
        assert!(matches!(
            local.validate(),
            Err(DerivationValidationError::InvalidEnvironmentName { field })
                if field == "jobs[0].phases[0].steps[0].environment[0].name"
        ));
    }

    for valid in ["_", "A", "A9_B"] {
        let mut plan = sample_plan();
        plan.environment.clear();
        plan.environment.insert(valid.to_owned(), String::new());
        plan.validate().unwrap();
    }
}

#[test]
fn validation_distinguishes_safe_empty_arguments_from_missing_programs_and_scripts() {
    let mut no_arguments = sample_plan();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut no_arguments) else {
        unreachable!()
    };
    args.clear();
    no_arguments.validate().unwrap();

    let mut empty_argument = sample_plan();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut empty_argument) else {
        unreachable!()
    };
    *args = vec![String::new()];
    empty_argument.validate().unwrap();

    let mut missing_program = sample_plan();
    let StepPlan::Run { program, .. } = sample_step_mut(&mut missing_program) else {
        unreachable!()
    };
    program.path.clear();
    assert!(matches!(
        missing_program.validate(),
        Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
            if field == "jobs[0].phases[0].steps[0].program.path"
    ));

    let mut missing_script = sample_plan();
    make_sample_shell(&mut missing_script);
    let StepPlan::Shell { script, .. } = sample_step_mut(&mut missing_script) else {
        unreachable!()
    };
    script.clear();
    assert!(matches!(
        missing_script.validate(),
        Err(DerivationValidationError::Empty { field })
            if field == "jobs[0].phases[0].steps[0].script"
    ));

    let mut missing_interpreter = sample_plan();
    make_sample_shell(&mut missing_interpreter);
    let StepPlan::Shell { interpreter, .. } = sample_step_mut(&mut missing_interpreter) else {
        unreachable!()
    };
    interpreter.path.clear();
    assert!(matches!(
        missing_interpreter.validate(),
        Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
            if field == "jobs[0].phases[0].steps[0].interpreter.path"
    ));
}

#[test]
fn process_collection_limits_accept_n_and_reject_n_plus_one() {
    let mut two_jobs = sample_plan();
    two_jobs.jobs.push(two_jobs.jobs[0].clone());
    let job_limits = DerivationValidationLimits {
        max_jobs: 2,
        ..DerivationValidationLimits::default()
    };
    two_jobs.validate_with_limits(job_limits).unwrap();
    let mut three_jobs = two_jobs;
    three_jobs.jobs.push(three_jobs.jobs[0].clone());
    assert_process_limit(
        three_jobs.validate_with_limits(job_limits).unwrap_err(),
        "jobs",
        3,
        2,
        "items",
    );

    let mut two_phases = sample_plan();
    let mut second_phase = two_phases.jobs[0].phases[0].clone();
    second_phase.name = "check".to_owned();
    two_phases.jobs[0].phases.push(second_phase);
    let phase_limits = DerivationValidationLimits {
        max_phases_per_job: 2,
        ..DerivationValidationLimits::default()
    };
    two_phases.validate_with_limits(phase_limits).unwrap();
    let mut three_phases = two_phases;
    let mut third_phase = three_phases.jobs[0].phases[0].clone();
    third_phase.name = "install".to_owned();
    three_phases.jobs[0].phases.push(third_phase);
    assert_process_limit(
        three_phases.validate_with_limits(phase_limits).unwrap_err(),
        "jobs[0].phases",
        3,
        2,
        "items",
    );

    let mut two_steps = sample_plan();
    let second_step = two_steps.jobs[0].phases[0].steps[0].clone();
    two_steps.jobs[0].phases[0].steps.push(second_step);
    let section_limits = DerivationValidationLimits {
        max_steps_per_section: 2,
        max_total_steps: 2,
        ..DerivationValidationLimits::default()
    };
    two_steps.validate_with_limits(section_limits).unwrap();
    let mut three_steps = two_steps;
    let third_step = three_steps.jobs[0].phases[0].steps[0].clone();
    three_steps.jobs[0].phases[0].steps.push(third_step);
    assert_process_limit(
        three_steps.validate_with_limits(section_limits).unwrap_err(),
        "jobs[0].phases[0].steps",
        3,
        2,
        "items",
    );

    let mut two_total_steps = sample_plan();
    let pre_step = two_total_steps.jobs[0].phases[0].steps[0].clone();
    two_total_steps.jobs[0].phases[0].pre = vec![pre_step];
    let total_step_limits = DerivationValidationLimits {
        max_steps_per_section: 2,
        max_total_steps: 2,
        ..DerivationValidationLimits::default()
    };
    two_total_steps.validate_with_limits(total_step_limits).unwrap();
    let mut three_total_steps = two_total_steps;
    let post_step = three_total_steps.jobs[0].phases[0].steps[0].clone();
    three_total_steps.jobs[0].phases[0].post = vec![post_step];
    assert_process_limit(
        three_total_steps.validate_with_limits(total_step_limits).unwrap_err(),
        "jobs[0].phases[0].post",
        3,
        2,
        "total steps",
    );

    let argument_limits = DerivationValidationLimits {
        max_arguments_per_step: 2,
        ..DerivationValidationLimits::default()
    };
    sample_plan().validate_with_limits(argument_limits).unwrap();
    let mut three_arguments = sample_plan();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut three_arguments) else {
        unreachable!()
    };
    args.push("--verbose".to_owned());
    assert_process_limit(
        three_arguments.validate_with_limits(argument_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].args",
        3,
        2,
        "items",
    );

    let mut two_programs = sample_plan();
    make_sample_shell(&mut two_programs);
    let StepPlan::Shell { declared_programs, .. } = sample_step_mut(&mut two_programs) else {
        unreachable!()
    };
    declared_programs.push(declared_programs[0].clone());
    let program_limits = DerivationValidationLimits {
        max_declared_programs_per_step: 2,
        ..DerivationValidationLimits::default()
    };
    two_programs.validate_with_limits(program_limits).unwrap();
    let mut three_programs = two_programs;
    let StepPlan::Shell { declared_programs, .. } = sample_step_mut(&mut three_programs) else {
        unreachable!()
    };
    declared_programs.push(declared_programs[0].clone());
    assert_process_limit(
        three_programs.validate_with_limits(program_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].declared_programs",
        3,
        2,
        "items",
    );
}

#[test]
fn environment_and_string_limits_accept_n_and_reject_n_plus_one() {
    let environment_limits = DerivationValidationLimits {
        max_environment_entries: 3,
        ..DerivationValidationLimits::default()
    };
    sample_plan().validate_with_limits(environment_limits).unwrap();
    let mut four_effective = sample_plan();
    let StepPlan::Run { environment, .. } = sample_step_mut(&mut four_effective) else {
        unreachable!()
    };
    environment.insert("LDFLAGS".to_owned(), "-Wl,--as-needed".to_owned());
    assert_process_limit(
        four_effective.validate_with_limits(environment_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].effective_environment",
        4,
        3,
        "items",
    );

    let name_limits = DerivationValidationLimits {
        max_environment_name_bytes: 6,
        ..DerivationValidationLimits::default()
    };
    sample_plan().validate_with_limits(name_limits).unwrap();
    let mut seven_byte_name = sample_plan();
    let StepPlan::Run { environment, .. } = sample_step_mut(&mut seven_byte_name) else {
        unreachable!()
    };
    environment.clear();
    environment.insert("CFLAGSS".to_owned(), String::new());
    assert_process_limit(
        seven_byte_name.validate_with_limits(name_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].environment[0].name",
        7,
        6,
        "bytes",
    );

    let mut sixty_four_bytes = sample_plan();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut sixty_four_bytes) else {
        unreachable!()
    };
    args[0] = "x".repeat(64);
    let string_limits = DerivationValidationLimits {
        max_process_string_bytes: 64,
        ..DerivationValidationLimits::default()
    };
    sixty_four_bytes.validate_with_limits(string_limits).unwrap();
    let mut sixty_five_bytes = sixty_four_bytes;
    let StepPlan::Run { args, .. } = sample_step_mut(&mut sixty_five_bytes) else {
        unreachable!()
    };
    args[0].push('x');
    assert_process_limit(
        sixty_five_bytes.validate_with_limits(string_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].args[0]",
        65,
        64,
        "bytes",
    );

    let mut sixty_four_byte_path = sample_plan();
    sixty_four_byte_path.layout.cargo_cache_dir = format!("/mason/{}", "x".repeat(57));
    assert_eq!(sixty_four_byte_path.layout.cargo_cache_dir.len(), 64);
    let path_limits = DerivationValidationLimits {
        max_path_bytes: 64,
        ..DerivationValidationLimits::default()
    };
    sixty_four_byte_path.validate_with_limits(path_limits).unwrap();
    let mut sixty_five_byte_path = sixty_four_byte_path;
    sixty_five_byte_path.layout.cargo_cache_dir.push('x');
    assert_process_limit(
        sixty_five_byte_path.validate_with_limits(path_limits).unwrap_err(),
        "layout.cargo_cache_dir",
        65,
        64,
        "path bytes",
    );
}

#[test]
fn execve_and_aggregate_limits_accept_n_and_reject_n_plus_one() {
    let plan = sample_plan();
    let probe_limits = DerivationValidationLimits {
        max_execve_bytes: 0,
        ..DerivationValidationLimits::default()
    };
    let DerivationValidationError::LimitExceeded {
        actual: execve_bytes,
        field,
        ..
    } = plan.validate_with_limits(probe_limits).unwrap_err()
    else {
        unreachable!()
    };
    assert_eq!(field, "jobs[0].phases[0].steps[0].execve");

    let execve_limits = DerivationValidationLimits {
        max_execve_bytes: execve_bytes,
        ..DerivationValidationLimits::default()
    };
    plan.validate_with_limits(execve_limits).unwrap();
    let mut one_more_execve_byte = plan.clone();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_execve_byte) else {
        unreachable!()
    };
    args[0].push('x');
    assert_process_limit(
        one_more_execve_byte.validate_with_limits(execve_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].execve",
        execve_bytes + 1,
        execve_bytes,
        "bytes",
    );

    let measured = measured_process_budget(&plan);
    let item_limits = DerivationValidationLimits {
        max_total_process_items: measured.total_items,
        ..DerivationValidationLimits::default()
    };
    plan.validate_with_limits(item_limits).unwrap();
    let mut one_more_item = plan.clone();
    let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_item) else {
        unreachable!()
    };
    args.push(String::new());
    assert_process_limit(
        one_more_item.validate_with_limits(item_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].environment",
        measured.total_items + 1,
        measured.total_items,
        "total process items",
    );

    let text_limits = DerivationValidationLimits {
        max_total_process_text_bytes: measured.total_text_bytes,
        ..DerivationValidationLimits::default()
    };
    plan.validate_with_limits(text_limits).unwrap();
    let mut one_more_text_byte = plan;
    let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_text_byte) else {
        unreachable!()
    };
    args[0].push('x');
    assert_process_limit(
        one_more_text_byte.validate_with_limits(text_limits).unwrap_err(),
        "jobs[0].phases[0].steps[0].working_dir",
        measured.total_text_bytes + 1,
        measured.total_text_bytes,
        "total process text bytes",
    );
}

#[test]
fn frozen_filesystem_policy_is_explicit_restricted_and_ordered() {
    let policy = FilesystemPolicy::default();
    assert_eq!(policy.proc, ProcFilesystem::None);
    assert_eq!(policy.tmp, TmpFilesystem::Empty);
    assert_eq!(policy.sys, SysFilesystem::None);
    assert_eq!(policy.dev, DevFilesystem::Minimal);

    let mut encoder = CanonicalEncoder::new(&[]);
    policy.encode(&mut encoder);
    assert_eq!(encoder.finish(), [0, 0, 0, 1]);
}

