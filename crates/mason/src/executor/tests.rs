use std::{
    os::{
        fd::{AsRawFd, FromRawFd as _},
        unix::fs::{PermissionsExt as _, symlink},
    },
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};

use stone_recipe::derivation::BuilderLayout;

use super::*;
use crate::package::{set_test_compiler_cache, test_derivation_plan};

fn compatible_executor_plan() -> DerivationPlan {
    let mut plan = test_derivation_plan();
    let version = tools_buildinfo::get_version();
    let implementation = tools_buildinfo::get_semantic_fingerprint();
    plan.cast_version = version.to_owned();
    plan.cast_fingerprint = implementation.to_owned();
    plan.execution.executor = stone_recipe::derivation::LockedIdentity {
        name: crate::planner::EXECUTOR_ABI.to_owned(),
        fingerprint: crate::planner::executor_fingerprint(version, implementation),
    };
    plan.package.architecture = std::env::consts::ARCH.to_owned();
    plan.build_lock.build_platform.architecture = std::env::consts::ARCH.to_owned();
    plan.build_lock.target_platform.architecture = std::env::consts::ARCH.to_owned();
    plan
}

fn execution_layout(root: &Path) -> BuilderLayout {
    let path = |relative: &str| root.join(relative).to_string_lossy().into_owned();
    BuilderLayout {
        hostname: "scratch-builder".to_owned(),
        guest_root: root.to_string_lossy().into_owned(),
        artifacts_dir: path("artifacts"),
        build_dir: path("build"),
        source_dir: path("sources"),
        recipe_dir: path("recipe"),
        install_dir: path("install"),
        package_dir: path("recipe/package"),
        ccache_dir: path("cache/ccache"),
        sccache_dir: path("cache/sccache"),
        go_cache_dir: path("cache/go-build"),
        go_mod_cache_dir: path("cache/go-mod"),
        cargo_cache_dir: path("cache/cargo"),
        zig_cache_dir: path("cache/zig"),
    }
}

fn poison_directory(path: &Path, symlink_target: &Path) {
    fs::create_dir_all(path.join("stale-dir")).unwrap();
    fs::write(path.join("stale-file"), b"stale").unwrap();
    fs::write(path.join("stale-dir/nested"), b"stale").unwrap();
    symlink(symlink_target, path.join("stale-link")).unwrap();
}

fn assert_directory_empty(path: &Path) {
    assert!(path.is_dir(), "{} was not recreated as a directory", path.display());
    assert!(
        fs::read_dir(path).unwrap().next().is_none(),
        "{} retained poisoned execution state",
        path.display()
    );
}

fn assert_poison_preserved(path: &Path) {
    assert_eq!(fs::read(path.join("stale-file")).unwrap(), b"stale");
    assert_eq!(fs::read(path.join("stale-dir/nested")).unwrap(), b"stale");
    assert!(
        fs::symlink_metadata(path.join("stale-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

struct AffinityRestore(CpuSet);

impl Drop for AffinityRestore {
    fn drop(&mut self) {
        let _ = sched_setaffinity(Pid::from_raw(0), &self.0);
    }
}

fn assert_child_inherits_single_cpu(cpu: usize) {
    let status = Command::new("/bin/sh")
        .args([
            "-c",
            r#"
                found=
                while read -r key value rest; do
                    if [ "$key" = "Cpus_allowed_list:" ]; then
                        found=$value
                        break
                    fi
                done < /proc/self/status
                [ "$found" = "$EXPECTED_CPU" ]
            "#,
        ])
        .env_clear()
        .env("EXPECTED_CPU", cpu.to_string())
        .status()
        .unwrap();
    assert!(status.success(), "child did not inherit the single-CPU affinity");
}

fn test_step_limits(stdout_bytes: u64, stderr_bytes: u64, total_output_bytes: u64) -> StepExecutionLimits {
    StepExecutionLimits {
        wall_time: Duration::from_secs(5),
        stdout_bytes,
        stderr_bytes,
        total_output_bytes,
    }
}

fn logged_quiet(
    limits: StepExecutionLimits,
    configure: impl FnOnce(&mut Command) -> &mut Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    logged_with_limits(
        "/bin/sh",
        None,
        DescendantContainment::ProcessGroup,
        limits,
        LogMode::Discard,
        configure,
    )
}

#[test]
fn retained_built_executable_uses_execveat_without_pathname_or_procfs_fallback() {
    const CHILD_TEST: &str = "executor::tests::descriptor_exec_child_observes_retained_executable_fd_closed";
    const HIGH_DESCRIPTOR_MINIMUM: RawFd = 512;
    const RETAINED_DESCRIPTOR_ENV: &str = "CAST_TEST_RETAINED_EXECUTABLE_FD";

    let temporary = crate::private_tempdir();
    let work = temporary.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let program = work.join("test-binary");
    std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let retained = crate::linux_fs::open_built_executable(&work, &program).unwrap();
    let retained_descriptor = fcntl(retained.as_raw_fd(), FcntlArg::F_DUPFD_CLOEXEC(HIGH_DESCRIPTOR_MINIMUM)).unwrap();
    assert!(retained_descriptor >= HIGH_DESCRIPTOR_MINIMUM);
    drop(retained);
    // SAFETY: F_DUPFD_CLOEXEC returned one fresh owned descriptor.
    let retained = unsafe { std::fs::File::from_raw_fd(retained_descriptor) };

    std::fs::remove_file(&program).unwrap();
    std::fs::write(&program, b"#!/bin/sh\nexit 91\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

    let args = vec![CHILD_TEST.to_owned(), "--exact".to_owned()];
    let environment = BTreeMap::from([(RETAINED_DESCRIPTOR_ENV.to_owned(), retained.as_raw_fd().to_string())]);
    let descriptor_exec =
        DescriptorExec::new(retained.as_raw_fd(), program.to_str().unwrap(), &args, &environment).unwrap();
    let status = logged_with_limits(
        "/descriptor-exec-has-no-pathname-fallback",
        Some(descriptor_exec),
        DescendantContainment::ProcessGroup,
        test_step_limits(1024 * 1024, 1024 * 1024, 1024 * 1024),
        LogMode::Discard,
        |command| command.current_dir(&work),
    )
    .unwrap();
    assert!(
        status.success(),
        "descriptor execution followed the replaced public path"
    );
}

#[test]
fn descriptor_exec_child_observes_retained_executable_fd_closed() {
    const RETAINED_DESCRIPTOR_ENV: &str = "CAST_TEST_RETAINED_EXECUTABLE_FD";

    let Ok(descriptor) = std::env::var(RETAINED_DESCRIPTOR_ENV) else {
        return;
    };
    let descriptor = descriptor.parse::<RawFd>().unwrap();
    // SAFETY: F_GETFD only inspects the numeric descriptor slot.
    let result = unsafe { nix::libc::fcntl(descriptor, nix::libc::F_GETFD) };
    assert_eq!(result, -1, "retained executable descriptor {descriptor} survived exec");
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(nix::libc::EBADF));
}

#[test]
fn descriptor_exec_rejects_shebang_without_pathname_fallback() {
    let temporary = crate::private_tempdir();
    let work = temporary.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let program = work.join("script");
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let retained = crate::linux_fs::open_built_executable(&work, &program).unwrap();
    let descriptor_exec =
        DescriptorExec::new(retained.as_raw_fd(), program.to_str().unwrap(), &[], &BTreeMap::new()).unwrap();

    let error = logged_with_limits(
        "/descriptor-exec-has-no-pathname-fallback",
        Some(descriptor_exec),
        DescendantContainment::ProcessGroup,
        test_step_limits(4_096, 4_096, 8_192),
        LogMode::Discard,
        |command| command.current_dir(&work),
    )
    .unwrap_err();
    match error {
        StepExecutionError::Spawn { source } => {
            assert_eq!(source.raw_os_error(), Some(nix::libc::ENOENT), "{source}");
        }
        other => panic!("descriptor-executed shebang did not fail during execveat: {other}"),
    }
}

#[test]
fn repeated_pgo_directories_are_recreated_once() {
    let job = JobPlan {
        pgo_stage: Some("one".to_owned()),
        pgo_dir: Some("/mason/build/target-pgo".to_owned()),
        build_dir: "/mason/build/target".to_owned(),
        work_dir: "/mason/build/target/source".to_owned(),
        phases: Vec::new(),
    };
    let repeated = job.clone();
    assert_eq!(unique_pgo_dirs(&[job, repeated]), ["/mason/build/target-pgo"].into());
}

#[test]
fn step_environment_overrides_only_frozen_global_values() {
    let global = BTreeMap::from([
        ("GLOBAL".to_owned(), "kept".to_owned()),
        ("OVERRIDE".to_owned(), "global".to_owned()),
    ]);
    let step = BTreeMap::from([
        ("OVERRIDE".to_owned(), "step".to_owned()),
        ("STEP".to_owned(), "present".to_owned()),
    ]);

    assert_eq!(
        merged_environment(&global, &step),
        BTreeMap::from([
            ("GLOBAL".to_owned(), "kept".to_owned()),
            ("OVERRIDE".to_owned(), "step".to_owned()),
            ("STEP".to_owned(), "present".to_owned()),
        ])
    );
}

#[test]
fn cpu_selection_uses_the_lowest_unique_representable_allowed_ids() {
    assert_eq!(select_cpu_ids(&[9, 5, 3, 5, 7], 3, 8).unwrap(), [3, 5, 7]);

    assert!(matches!(
        select_cpu_ids(&[2, 4], 3, 8),
        Err(Error::InsufficientCpuAffinity {
            requested: 3,
            available: 2
        })
    ));
    assert!(matches!(
        select_cpu_ids(&[0, 1, 2, 3], 4, 3),
        Err(Error::UnrepresentableCpuAffinity {
            requested: 4,
            representable: 3
        })
    ));
}

#[test]
fn linux_cpu_affinity_is_exact_parent_relative_and_inherited() {
    let current_task = Pid::from_raw(0);
    let original = sched_getaffinity(current_task).unwrap();
    let original_ids = affinity_cpu_ids(&original).unwrap();
    assert!(!original_ids.is_empty(), "the test task must have an allowed CPU");

    {
        let _restore = AffinityRestore(original);

        restrict_current_cpu_affinity(1).unwrap();
        assert_eq!(
            affinity_cpu_ids(&sched_getaffinity(current_task).unwrap()).unwrap(),
            [original_ids[0]]
        );
        assert_child_inherits_single_cpu(original_ids[0]);

        // Restore the complete parent mask before constructing a distinct
        // one. An unprivileged task may widen its mask only within the
        // enclosing cpuset, which is precisely the original set here.
        sched_setaffinity(current_task, &original).unwrap();
        if original_ids.len() > 1 {
            let alternate_parent = affinity_mask(&original_ids[1..]).unwrap();
            sched_setaffinity(current_task, &alternate_parent).unwrap();
            let alternate_jobs = original_ids[1..].len().min(2) as u32;
            restrict_current_cpu_affinity(alternate_jobs).unwrap();
            assert_eq!(
                affinity_cpu_ids(&sched_getaffinity(current_task).unwrap()).unwrap(),
                original_ids[1..]
                    .iter()
                    .copied()
                    .take(alternate_jobs as usize)
                    .collect::<Vec<_>>()
            );
        }

        sched_setaffinity(current_task, &original).unwrap();
        let unavailable_jobs = u32::try_from(original_ids.len() + 1).unwrap();
        assert!(matches!(
            restrict_current_cpu_affinity(unavailable_jobs),
            Err(Error::InsufficientCpuAffinity { .. } | Error::UnrepresentableCpuAffinity { .. })
        ));
        assert_eq!(sched_getaffinity(current_task).unwrap(), original);
    }

    assert_eq!(
        sched_getaffinity(current_task).unwrap(),
        original,
        "the affinity test must restore its caller mask"
    );
}

#[test]
fn frozen_commands_get_eof_on_stdin_and_no_inherited_extra_descriptors() {
    let inherited = tempfile::tempfile().unwrap();
    let inherited_fd = inherited.as_raw_fd();
    fcntl(inherited_fd, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();
    let script = format!("test ! -e /proc/self/fd/{inherited_fd} && ! read value");

    let status = logged("/bin/sh", DescendantContainment::ProcessGroup, |command| {
        command.args(["-c", &script])
    })
    .unwrap();

    assert!(status.success());
}

#[test]
fn frozen_children_disable_core_dumps_and_cap_open_descriptors() {
    let status = logged_quiet(test_step_limits(4_096, 4_096, 8_192), |command| {
        command.args([
            "-c",
            concat!(
                "test \"$(ulimit -c)\" = 0 && test \"$(ulimit -Hc)\" = 0 && ",
                "test \"$(ulimit -n)\" -le 4096 && test \"$(ulimit -Hn)\" -le 4096",
            ),
        ])
    })
    .unwrap();

    assert!(status.success());
}

#[test]
fn ordinary_success_exit_code_and_signal_status_are_preserved() {
    let limits = test_step_limits(4_096, 4_096, 8_192);
    let success = logged_quiet(limits, |command| {
        command.args(["-c", "printf 'ordinary output\\n'; printf 'ordinary error\\n' >&2"])
    })
    .unwrap();
    assert!(success.success());

    let failure = logged_quiet(limits, |command| command.args(["-c", "exit 23"])).unwrap();
    assert_eq!(failure.code(), Some(23));

    let signaled = logged_quiet(limits, |command| command.args(["-c", "kill -TERM $$"])).unwrap();
    assert_eq!(signaled.signal(), Some(Signal::SIGTERM as i32));
}

#[test]
fn per_stream_output_ceiling_accepts_exact_n_and_rejects_n_plus_one() {
    const LIMIT: u64 = 4_096;
    for (stream, redirect) in [(OutputStream::Stdout, ""), (OutputStream::Stderr, " >&2")] {
        let limits = test_step_limits(LIMIT, LIMIT, LIMIT * 2);
        let exact = format!("/usr/bin/head -c {LIMIT} /dev/zero{redirect}");
        assert!(
            logged_quiet(limits, |command| command.args(["-c", &exact]))
                .unwrap()
                .success()
        );

        let over = format!("/usr/bin/head -c {} /dev/zero{redirect}", LIMIT + 1);
        assert!(matches!(
            logged_quiet(limits, |command| command.args(["-c", &over])),
            Err(StepExecutionError::OutputLimit {
                stream: found,
                limit: LIMIT,
                observed,
            }) if found == stream && observed == LIMIT + 1
        ));
    }
}

#[test]
fn combined_output_ceiling_accepts_exact_n_and_rejects_n_plus_one() {
    const HALF: u64 = 2_048;
    const TOTAL: u64 = HALF * 2;
    let limits = test_step_limits(TOTAL, TOTAL, TOTAL);
    let exact = format!("/usr/bin/head -c {HALF} /dev/zero; /usr/bin/head -c {HALF} /dev/zero >&2");
    assert!(
        logged_quiet(limits, |command| command.args(["-c", &exact]))
            .unwrap()
            .success()
    );

    let over = format!(
        "/usr/bin/head -c {HALF} /dev/zero; /usr/bin/head -c {} /dev/zero >&2",
        HALF + 1
    );
    assert!(matches!(
        logged_quiet(limits, |command| command.args(["-c", &over])),
        Err(StepExecutionError::TotalOutputLimit {
            limit: TOTAL,
            observed,
        }) if observed == TOTAL + 1
    ));
}

#[test]
fn unbroken_line_flood_is_bounded_without_allocating_a_line() {
    const LIMIT: u64 = 8_192;
    let started = Instant::now();
    let result = logged_quiet(test_step_limits(LIMIT, LIMIT, LIMIT * 2), |command| {
        command.args(["-c", "while :; do printf 0123456789abcdef; done"])
    });

    assert!(matches!(
        result,
        Err(StepExecutionError::OutputLimit {
            stream: OutputStream::Stdout,
            limit: LIMIT,
            observed,
        }) if observed == LIMIT + 1
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn log_read_failures_keep_the_stream_and_original_io_error() {
    struct BrokenReader;

    impl io::Read for BrokenReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "injected read failure"))
        }
    }

    let budget = Mutex::new(OutputBudget::default());
    let logs = Mutex::new(LogMux::new(LogMode::Discard));
    let stop = AtomicBool::new(false);
    let result = drain_log(
        BrokenReader,
        OutputStream::Stderr,
        test_step_limits(10, 10, 20),
        &budget,
        &logs,
        &stop,
    );

    assert!(matches!(
        result,
        Err(StepExecutionError::OutputRead {
            stream: OutputStream::Stderr,
            source,
        }) if source.kind() == io::ErrorKind::Other && source.to_string() == "injected read failure"
    ));
}

#[test]
fn timeout_kills_stalled_child_and_its_delayed_background_work() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("late-write");
    let mut limits = test_step_limits(4_096, 4_096, 8_192);
    limits.wall_time = Duration::from_millis(100);

    let started = Instant::now();
    let result = logged_quiet(limits, |command| {
        command.env("MARKER", &marker).args([
            "-c",
            "(/usr/bin/sleep 1; printf late > \"$MARKER\") & /usr/bin/sleep 30",
        ])
    });
    assert!(matches!(
        result,
        Err(StepExecutionError::Timeout { limit }) if limit == Duration::from_millis(100)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));

    thread::sleep(Duration::from_millis(1_200));
    assert!(
        !marker.exists(),
        "timed-out background work escaped containment cleanup"
    );
}

#[test]
fn containment_cleanup_failure_is_structured_and_does_not_target_the_host_namespace() {
    assert_ne!(
        getpid().as_raw(),
        1,
        "unit tests must not own a production PID namespace"
    );
    let result = logged_with_limits(
        "/bin/true",
        None,
        DescendantContainment::PidNamespace,
        test_step_limits(32, 32, 64),
        LogMode::Discard,
        |command| command,
    );

    assert!(matches!(
        result,
        Err(StepExecutionError::Cleanup {
            operation: "terminate containment boundary",
            source,
        }) if source.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn background_process_holding_log_pipes_cannot_stall_a_completed_step() {
    let started = Instant::now();
    let status = logged("/bin/sh", DescendantContainment::ProcessGroup, |command| {
        command.args(["-c", "sleep 30 &"])
    })
    .unwrap();

    assert!(status.success());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn background_process_cannot_write_after_its_step_completes() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("late-write");
    let status = logged("/bin/sh", DescendantContainment::ProcessGroup, |command| {
        command
            .env("MARKER", &marker)
            .args(["-c", "(sleep 1; printf late > \"$MARKER\") >/dev/null 2>&1 &"])
    })
    .unwrap();

    assert!(status.success());
    thread::sleep(Duration::from_millis(1_200));
    assert!(!marker.exists());
}

#[test]
fn production_containment_targets_the_complete_pid_namespace() {
    let child = Pid::from_raw(42);
    assert_eq!(
        descendant_signal_target(DescendantContainment::PidNamespace, child),
        Pid::from_raw(-1)
    );
    assert_eq!(
        descendant_signal_target(DescendantContainment::ProcessGroup, child),
        Pid::from_raw(-42)
    );
    assert!(matches!(
        require_pid_namespace_init(Pid::from_raw(2)),
        Err(Error::PidNamespaceInitRequired(2))
    ));
}

#[test]
fn frozen_build_root_is_cleared_before_execution() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("stale-file"), b"stale").unwrap();
    fs::create_dir(root.path().join("stale-dir")).unwrap();
    fs::write(root.path().join("stale-dir/nested"), b"stale").unwrap();

    clear_directory_contents(root.path()).unwrap();

    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
fn execution_scratch_clears_enabled_plan_caches_but_never_touches_disabled_caches() {
    let temp = tempfile::tempdir().unwrap();
    let guest_root = temp.path().join("non-default-sandbox");
    let sentinel = temp.path().join("outside-cache-sentinel");
    fs::write(&sentinel, b"keep").unwrap();

    let mut plan = test_derivation_plan();
    plan.layout = execution_layout(&guest_root);
    set_test_compiler_cache(&mut plan, true);
    plan.validate().unwrap();
    let build_dir = PathBuf::from(&plan.layout.build_dir);
    let cache_dirs = plan
        .layout
        .cache_destinations()
        .into_iter()
        .map(|(_, destination)| PathBuf::from(destination));
    let cache_dirs = cache_dirs.collect::<Vec<_>>();

    poison_directory(&build_dir, &sentinel);
    for cache_dir in &cache_dirs {
        poison_directory(cache_dir, &sentinel);
    }

    prepare_execution_scratch(&plan).unwrap();

    assert_directory_empty(&build_dir);
    for cache_dir in &cache_dirs {
        assert_directory_empty(cache_dir);
    }
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");

    poison_directory(&build_dir, &sentinel);
    for cache_dir in &cache_dirs {
        poison_directory(cache_dir, &sentinel);
    }
    set_test_compiler_cache(&mut plan, false);
    plan.validate().unwrap();

    prepare_execution_scratch(&plan).unwrap();

    assert_directory_empty(&build_dir);
    for cache_dir in &cache_dirs {
        assert_poison_preserved(cache_dir);
    }
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");

    let missing_guest_root = temp.path().join("disabled-missing-cache-sandbox");
    let mut missing_plan = test_derivation_plan();
    missing_plan.layout = execution_layout(&missing_guest_root);
    missing_plan.execution.compiler_cache = false;
    missing_plan.validate().unwrap();
    let missing_build_dir = PathBuf::from(&missing_plan.layout.build_dir);
    let missing_cache_dirs = missing_plan
        .layout
        .cache_destinations()
        .into_iter()
        .map(|(_, destination)| PathBuf::from(destination))
        .collect::<Vec<_>>();
    poison_directory(&missing_build_dir, &sentinel);
    assert!(missing_cache_dirs.iter().all(|path| !path.exists()));

    prepare_execution_scratch(&missing_plan).unwrap();

    assert_directory_empty(&missing_build_dir);
    assert!(missing_cache_dirs.iter().all(|path| !path.exists()));
}

#[test]
fn runtime_symbol_parsing_remains_a_defensive_backstop() {
    for stage in ["one", "two", "use"] {
        parse_pgo_stage(stage).unwrap();
    }
    for phase in ["Prepare", "setup", "BUILD", "install", "check", "workload"] {
        parse_phase(phase).unwrap();
    }
    assert!(matches!(
        parse_pgo_stage("ONE"),
        Err(Error::UnsupportedPgoStage(stage)) if stage == "ONE"
    ));
    assert!(matches!(
        parse_phase("environment"),
        Err(Error::UnsupportedPhase(phase)) if phase == "environment"
    ));
}

#[test]
fn frozen_build_platform_is_checked_only_at_executor_preflight() {
    validate_build_host("x86_64", "x86_64").unwrap();
    assert!(matches!(
        validate_build_host("aarch64", "x86_64"),
        Err(Error::IncompatibleBuildHost { required, actual })
            if required == "aarch64" && actual == "x86_64"
    ));
}

#[test]
fn executor_preflight_uses_execution_identity_not_structural_builder_identity() {
    let mut plan = compatible_executor_plan();
    plan.build_lock.builder = stone_recipe::derivation::LockedIdentity {
        name: "authored-custom-builder".to_owned(),
        fingerprint: "authored-structural-fingerprint".to_owned(),
    };

    Executor::new(&plan).unwrap();

    plan.execution.executor.name = "different-executor-abi".to_owned();
    assert!(matches!(
        Executor::new(&plan),
        Err(Error::IncompatibleExecutor { found, .. }) if found == "different-executor-abi"
    ));
}

#[test]
fn executor_preflight_rejects_changed_executor_fingerprint() {
    let mut plan = compatible_executor_plan();
    plan.execution.executor.fingerprint.push_str("-changed");

    assert!(matches!(
        Executor::new(&plan),
        Err(Error::IncompatibleExecutorFingerprint { found, .. }) if found.ends_with("-changed")
    ));
}
