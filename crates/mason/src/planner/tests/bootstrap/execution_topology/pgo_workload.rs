const FIXTURE: &str = "pgo-workload";
const WORK_DIR: &str = "/mason/build/x86_64/cast-pgo-workload-fixture";
const PGO_DIR: &str = "/mason/build/x86_64-pgo";

pub(super) fn assert_contract(plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(plan.execution.jobs, 1, "{FIXTURE}: executor CPU budget drifted");
    let [stage_one, profile_use] = plan.jobs.as_slice() else {
        panic!("{FIXTURE}: frozen execution must contain exactly instrumented and profile-use jobs");
    };
    assert_job_identity(stage_one, "one");
    assert_job_identity(profile_use, "use");
    assert_eq!(
        stage_one.work_dir, profile_use.work_dir,
        "{FIXTURE}: the executor must recreate one canonical work path before each PGO job"
    );
    assert_eq!(
        stage_one.pgo_dir, profile_use.pgo_dir,
        "{FIXTURE}: instrumented and profile-use jobs must share one bounded profile directory"
    );

    assert_stage_one(stage_one);
    assert_profile_use(profile_use);
    assert_pgo_request(
        plan,
        "binary(bash)",
        PGO_BASH_PACKAGE_ID,
        stone_recipe::derivation::JobExecutableRole::ShellInterpreter,
    );
    assert_pgo_request(
        plan,
        "binary(llvm-profdata)",
        PGO_LLVM_PACKAGE_ID,
        stone_recipe::derivation::JobExecutableRole::ShellDeclaredProgram { index: 0 },
    );
    assert_pgo_request(
        plan,
        "binary(cp)",
        PGO_COREUTILS_PACKAGE_ID,
        stone_recipe::derivation::JobExecutableRole::RunProgram,
    );
    let clang = exact_request(plan, "binary(clang)");
    assert_eq!(clang.package_id, PGO_CLANG_PACKAGE_ID);
    assert_eq!(clang.output, "out");
    assert!(
        plan.build_lock.requests.iter().all(|request| request.request != "binary(rm)"),
        "{FIXTURE}: cspgo=false must not admit the stage-two removal command"
    );
    assert!(
        plan.build_lock.requests.iter().all(|request| request.request != "binary(ld.mold)"),
        "{FIXTURE}: mold=false must not admit the Mold linker"
    );
}

fn assert_job_identity(job: &stone_recipe::derivation::JobPlan, stage: &str) {
    assert_eq!(job.pgo_stage.as_deref(), Some(stage));
    assert_eq!(job.pgo_dir.as_deref(), Some(PGO_DIR));
    assert_eq!(job.build_dir, "/mason/build/x86_64");
    assert_eq!(job.work_dir, WORK_DIR);
}

fn assert_stage_one(job: &stone_recipe::derivation::JobPlan) {
    assert_eq!(
        job.phases.iter().map(|phase| phase.name.as_str()).collect::<Vec<_>>(),
        ["Prepare", "Setup", "Build", "Workload"]
    );
    assert_prepare(&job.phases[0]);
    assert_setup(&job.phases[1]);
    assert_build(&job.phases[2], "ONE", "-fprofile-generate=/mason/build/x86_64-pgo/IR");

    let workload = &job.phases[3];
    assert!(workload.pre.is_empty() && workload.post.is_empty());
    let [
        stone_recipe::derivation::StepPlan::RunBuilt {
            program,
            args,
            environment,
            working_dir,
        },
        stone_recipe::derivation::StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            environment: merge_environment,
            working_dir: merge_working_dir,
        },
        stone_recipe::derivation::StepPlan::Run {
            program: copy,
            args: copy_args,
            environment: copy_environment,
            working_dir: copy_working_dir,
        },
    ] = workload.steps.as_slice()
    else {
        panic!("{FIXTURE}: instrumented workload must train, merge, and copy its exact profile");
    };
    assert_eq!(program, &format!("{WORK_DIR}/build/cast-pgo-workload-fixture"));
    assert_eq!(args.as_slice(), ["--train", "profile-guided-build-2026"]);
    assert_eq!(working_dir, WORK_DIR);
    assert_eq!(environment["PGO_STAGE"], "ONE");
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/llvm-profdata"]
    );
    assert_eq!(merge_environment["PGO_STAGE"], "ONE");
    assert_eq!(merge_working_dir, WORK_DIR);
    assert!(script.contains("-output=/mason/build/x86_64-pgo/ir.profdata"));
    assert!(script.contains("/mason/build/x86_64-pgo/IR/default"));
    assert_eq!(copy.path, "/usr/bin/cp");
    assert_eq!(
        copy_args.as_slice(),
        [
            "/mason/build/x86_64-pgo/ir.profdata",
            "/mason/build/x86_64-pgo/combined.profdata",
        ]
    );
    assert_eq!(copy_environment["PGO_STAGE"], "ONE");
    assert_eq!(copy_working_dir, WORK_DIR);
}

fn assert_profile_use(job: &stone_recipe::derivation::JobPlan) {
    assert_eq!(
        job.phases.iter().map(|phase| phase.name.as_str()).collect::<Vec<_>>(),
        ["Prepare", "Setup", "Build", "Install", "Check"]
    );
    assert_prepare(&job.phases[0]);
    assert_setup(&job.phases[1]);
    assert_build(
        &job.phases[2],
        "USE",
        "-fprofile-use=/mason/build/x86_64-pgo/combined.profdata",
    );

    let install = &job.phases[3];
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        environment,
        working_dir,
    }] = install.steps.as_slice()
    else {
        panic!("{FIXTURE}: profile-use job must install exactly its rebuilt binary");
    };
    assert_eq!(interpreter.path, "/usr/bin/dash");
    assert_eq!(
        declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );
    assert!(script.contains("build/cast-pgo-workload-fixture"));
    assert_eq!(environment["PGO_STAGE"], "USE");
    assert_eq!(working_dir, WORK_DIR);

    let check = &job.phases[4];
    let [stone_recipe::derivation::StepPlan::RunBuilt {
        program,
        args,
        environment,
        working_dir,
    }] = check.steps.as_slice()
    else {
        panic!("{FIXTURE}: final job must execute one structural self-test");
    };
    assert_eq!(program, &format!("{WORK_DIR}/build/cast-pgo-workload-fixture"));
    assert_eq!(args.as_slice(), ["--self-test"]);
    assert_eq!(environment["PGO_STAGE"], "USE");
    assert_eq!(working_dir, WORK_DIR);
}

fn assert_prepare(phase: &stone_recipe::derivation::PhasePlan) {
    assert!(phase.pre.is_empty() && phase.post.is_empty());
    assert!(matches!(
        phase.steps.as_slice(),
        [stone_recipe::derivation::StepPlan::ExtractArchive {
            source: 0,
            destination,
            strip_components: 1,
        }] if destination == "cast-pgo-workload-fixture"
    ));
}

fn assert_setup(phase: &stone_recipe::derivation::PhasePlan) {
    assert!(phase.pre.is_empty() && phase.post.is_empty());
    assert!(matches!(
        phase.steps.as_slice(),
        [stone_recipe::derivation::StepPlan::Run { program, args, .. }]
            if program.path == "/usr/bin/mkdir" && args.as_slice() == ["-p", "build"]
    ));
}

fn assert_build(phase: &stone_recipe::derivation::PhasePlan, stage: &str, profile_flag: &str) {
    assert!(phase.pre.is_empty() && phase.post.is_empty());
    let [stone_recipe::derivation::StepPlan::Shell {
        interpreter,
        declared_programs,
        script,
        environment,
        working_dir,
    }] = phase.steps.as_slice()
    else {
        panic!("{FIXTURE}: each PGO job must compile through one declared custom step");
    };
    assert_eq!(interpreter.path, "/usr/bin/dash");
    assert_eq!(
        declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/clang"]
    );
    assert!(script.contains("test ! -e build/stage1-only.marker"));
    assert!(script.contains("${CC}"));
    assert_eq!(environment["PGO_STAGE"], stage);
    assert!(environment["CFLAGS"].contains(profile_flag));
    assert!(environment["LDFLAGS"].contains(profile_flag));
    assert!(!environment["CFLAGS"].contains("sample"));
    assert!(!environment["LDFLAGS"].contains("sample"));
    assert!(!environment["LDFLAGS"].contains("mold"));
    assert_eq!(working_dir, WORK_DIR);
}

fn assert_pgo_request(
    plan: &stone_recipe::derivation::DerivationPlan,
    relation: &str,
    package_id: &str,
    role: stone_recipe::derivation::JobExecutableRole,
) {
    let request = exact_request(plan, relation);
    assert_eq!(request.package_id, package_id);
    assert_eq!(request.output, "out");
    assert_eq!(
        request.origins,
        [stone_recipe::derivation::InputOrigin::JobExecutable {
            job: 0,
            phase: 3,
            phase_name: "Workload".to_owned(),
            section: stone_recipe::derivation::JobStepSection::Steps,
            step: match role {
                stone_recipe::derivation::JobExecutableRole::RunProgram => 2,
                _ => 1,
            },
            role,
        }]
    );
}

fn exact_request<'a>(
    plan: &'a stone_recipe::derivation::DerivationPlan,
    relation: &str,
) -> &'a stone_recipe::derivation::LockedRequest {
    let matches = plan
        .build_lock
        .requests
        .iter()
        .filter(|request| request.request == relation)
        .collect::<Vec<_>>();
    let [request] = matches.as_slice() else {
        panic!("{FIXTURE}: build lock must contain exactly one {relation} request");
    };
    request
}
