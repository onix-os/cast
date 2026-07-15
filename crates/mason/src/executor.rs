//! Execution of an already-frozen derivation plan.
//!
//! This module deliberately has no access to recipes, policy macros,
//! profiles, or Forge provider resolution. Those belong to planning.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    io::{self, Write},
    os::fd::{AsRawFd, RawFd},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use fs_err as fs;
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    sched::{CpuSet, sched_getaffinity, sched_setaffinity},
    sys::{
        signal::{Signal, kill},
        wait::{WaitStatus, waitpid},
    },
    unistd::{Pid, getpgrp, getpid, setpgid},
};
use stone_recipe::derivation::{DerivationPlan, JobPlan, StepPlan};
use thiserror::Error;
use tui::Styled;

use crate::{
    Timing,
    build::{job::Phase, pgo::Stage},
    timing,
};

pub struct Executor<'a> {
    plan: &'a DerivationPlan,
}

/// Frozen package declarations do not control these limits. They are a final,
/// deliberately generous operational ceiling around every already-frozen
/// build step, so a wedged or noisy tool cannot retain the executor forever.
const STEP_WALL_TIME_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);
const STEP_STDOUT_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const STEP_STDERR_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const STEP_TOTAL_OUTPUT_BYTE_LIMIT: u64 = 96 * 1024 * 1024;
const STEP_OPEN_FILE_LIMIT: nix::libc::rlim_t = 4_096;
const LOG_READ_BUFFER_BYTES: usize = 16 * 1024;
const STEP_MONITOR_INTERVAL: Duration = Duration::from_millis(10);

/// Parent-prepared argument and environment vectors for one descriptor-based
/// native-executable handoff in the post-fork child.
///
/// The raw pointer arrays refer only to immutable allocations owned by the two
/// `CString` vectors. Construction completes every allocation before exposing
/// the value, and none of those vectors are mutated afterward. Moving this
/// value therefore never moves the referenced string bytes.
struct DescriptorExec {
    descriptor: RawFd,
    _arguments: Vec<CString>,
    _environment: Vec<CString>,
    argument_pointers: Vec<*const nix::libc::c_char>,
    environment_pointers: Vec<*const nix::libc::c_char>,
}

// SAFETY: all raw pointers refer into immutable allocations owned by the same
// value, remain valid for its complete lifetime, and are only read by the
// post-fork child immediately before `execveat`.
unsafe impl Send for DescriptorExec {}
// SAFETY: see the `Send` justification. Shared access cannot mutate either the
// pointer arrays or their backing `CString` allocations.
unsafe impl Sync for DescriptorExec {}

impl DescriptorExec {
    fn new(
        descriptor: RawFd,
        program: &str,
        args: &[String],
        environment: &BTreeMap<String, String>,
    ) -> io::Result<Self> {
        let mut arguments = Vec::with_capacity(args.len().saturating_add(1));
        arguments.push(process_cstring("built program", program)?);
        for argument in args {
            arguments.push(process_cstring("built-program argument", argument)?);
        }
        let environment = environment
            .iter()
            .map(|(key, value)| process_cstring("built-program environment", &format!("{key}={value}")))
            .collect::<io::Result<Vec<_>>>()?;
        let argument_pointers = arguments
            .iter()
            .map(|value| value.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let environment_pointers = environment
            .iter()
            .map(|value| value.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        Ok(Self {
            descriptor,
            _arguments: arguments,
            _environment: environment,
            argument_pointers,
            environment_pointers,
        })
    }

    /// Replace the post-fork child with the retained executable capability.
    ///
    /// All allocation and pointer construction happened in the parent. On
    /// success this syscall never returns; failure is reported through
    /// `Command`'s existing child-error pipe, with no pathname fallback.
    unsafe fn execveat(&self) -> io::Result<()> {
        // SAFETY: the descriptor names the retained executable; the empty path
        // is a valid static C string; both pointer arrays are null-terminated
        // and backed by live immutable C strings; the kernel copies all inputs
        // synchronously and retains none of these userspace pointers.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_execveat,
                self.descriptor,
                c"".as_ptr(),
                self.argument_pointers.as_ptr(),
                self.environment_pointers.as_ptr(),
                nix::libc::AT_EMPTY_PATH,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Err(io::Error::other("execveat unexpectedly returned"))
        }
    }
}

fn process_cstring(field: &'static str, value: &str) -> io::Result<CString> {
    CString::new(value).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{field} contains NUL")))
}

impl<'a> Executor<'a> {
    pub fn new(plan: &'a DerivationPlan) -> Result<Self, Error> {
        plan.validate()?;
        if plan.execution.executor.name != crate::planner::EXECUTOR_ABI {
            return Err(Error::IncompatibleExecutor {
                expected: crate::planner::EXECUTOR_ABI,
                found: plan.execution.executor.name.clone(),
            });
        }
        let current_version = tools_buildinfo::get_version();
        if plan.cast_version != current_version {
            return Err(Error::IncompatibleCast {
                expected: current_version.to_owned(),
                found: plan.cast_version.clone(),
            });
        }
        let current_fingerprint = tools_buildinfo::get_semantic_fingerprint();
        if plan.cast_fingerprint != current_fingerprint {
            return Err(Error::IncompatibleCastSemantics {
                expected: current_fingerprint.to_owned(),
                found: plan.cast_fingerprint.clone(),
            });
        }
        let expected_executor = crate::planner::executor_fingerprint(current_version, current_fingerprint);
        if plan.execution.executor.fingerprint != expected_executor {
            return Err(Error::IncompatibleExecutorFingerprint {
                expected: expected_executor,
                found: plan.execution.executor.fingerprint.clone(),
            });
        }
        validate_build_host(&plan.build_lock.build_platform.architecture, std::env::consts::ARCH)?;
        Ok(Self { plan })
    }

    pub fn run(&self, timing: &mut Timing) -> Result<(), Error> {
        require_pid_namespace_init(getpid())?;
        // Linux affinity is inherited by every subsequently created thread
        // and process. Pin PID 1 before execution scratch can create any
        // workers; the restriction therefore also remains in force for the
        // frozen package analyzers that run after all build steps.
        restrict_current_cpu_affinity(self.plan.execution.jobs)?;
        prepare_execution_scratch(self.plan)?;
        setpgid(Pid::from_raw(0), Pid::from_raw(0))?;
        let pgid = getpgrp();
        ::container::set_term_fg(pgid)?;
        let mut archive_session = crate::archive::ArchiveSessionBudget::production();

        let target = &self.plan.build_lock.target.name;
        for (job_index, job) in self.plan.jobs.iter().enumerate() {
            println!("{}", target_prefix(target, job_index));
            fs::create_dir_all(&job.build_dir)?;
            forge::util::recreate_dir(Path::new(&job.work_dir))?;
            if let Some(stage) = &job.pgo_stage {
                println!("{}", pgo_stage_prefix(stage, job_index));
            }

            for (phase_index, phase) in job.phases.iter().enumerate() {
                println!("{}", phase_prefix(&phase.name, job.pgo_stage.is_some(), phase_index));
                let timer = timing.begin(timing::Kind::Build(timing::Build {
                    target: target.clone(),
                    pgo_stage: job.pgo_stage.as_deref().map(parse_pgo_stage).transpose()?,
                    phase: parse_phase(&phase.name)?,
                }));
                for step in phase.pre.iter().chain(&phase.steps).chain(&phase.post) {
                    self.run_step(step, job, &mut archive_session)?;
                }
                timing.finish(timer);
            }
        }
        println!();
        Ok(())
    }

    fn run_step(
        &self,
        step: &StepPlan,
        job: &JobPlan,
        archive_session: &mut crate::archive::ArchiveSessionBudget,
    ) -> Result<(), Error> {
        if let StepPlan::ExtractArchive {
            source,
            destination,
            strip_components,
        } = step
        {
            let source_index = usize::try_from(*source).map_err(|_| Error::InvalidArchiveSource(*source))?;
            let Some(stone_recipe::derivation::LockedSource::Archive { sha256, filename, .. }) =
                self.plan.sources.get(source_index)
            else {
                return Err(Error::InvalidArchiveSource(*source));
            };
            crate::archive::extract_locked_tar(
                Path::new(&self.plan.layout.source_dir),
                filename,
                sha256,
                Path::new(&job.build_dir),
                destination,
                *strip_components,
                self.plan.source_date_epoch,
                archive_session,
            )?;
            return Ok(());
        }
        let (program, args, step_environment, working_dir, retained_program) = match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => (
                program.path.clone(),
                args.clone(),
                environment,
                working_dir.as_str(),
                None,
            ),
            StepPlan::RunBuilt {
                program,
                args,
                environment,
                working_dir,
            } => {
                let retained = crate::linux_fs::open_built_executable(Path::new(working_dir), Path::new(program))
                    .map_err(|source| Error::BuiltExecutable {
                        path: program.clone(),
                        source,
                    })?;
                (
                    program.clone(),
                    args.clone(),
                    environment,
                    working_dir.as_str(),
                    Some(retained),
                )
            }
            StepPlan::Shell {
                interpreter,
                script,
                environment,
                working_dir,
                ..
            } => (
                interpreter.path.clone(),
                vec!["-c".to_owned(), script.clone()],
                environment,
                working_dir.as_str(),
                None,
            ),
            StepPlan::ExtractArchive { .. } => unreachable!("archive extraction returned above"),
        };
        let environment = merged_environment(&self.plan.environment, step_environment);
        let descriptor_exec = retained_program
            .as_ref()
            .map(|retained| DescriptorExec::new(retained.as_raw_fd(), &program, &args, &environment))
            .transpose()
            .map_err(|source| Error::BuiltExecutable {
                path: program.clone(),
                source,
            })?;
        let status = logged_retaining(
            &program,
            descriptor_exec,
            DescendantContainment::PidNamespace,
            |command| {
                command
                    .args(args)
                    .env_clear()
                    .envs(environment)
                    .current_dir(working_dir)
            },
        )?;
        if status.success() {
            return Ok(());
        }
        match status.code() {
            Some(code) => Err(Error::Code(code)),
            None => {
                if let Some(signal) = status
                    .signal()
                    .or_else(|| status.stopped_signal())
                    .and_then(|signal| Signal::try_from(signal).ok())
                {
                    Err(Error::Signal(signal))
                } else {
                    Err(Error::UnknownSignal)
                }
            }
        }
    }
}

/// Prepare mutable execution state exclusively from frozen plan paths.
///
/// Compiler and tool caches may be shared by phases within this execution,
/// but no cache bytes from an earlier execution are allowed to influence it.
fn prepare_execution_scratch(plan: &DerivationPlan) -> io::Result<()> {
    clear_directory_contents(Path::new(&plan.layout.build_dir))?;

    if plan.execution.compiler_cache {
        for (_, destination) in plan.layout.cache_destinations() {
            clear_directory_contents(Path::new(destination))?;
        }
    }

    for pgo_dir in unique_pgo_dirs(&plan.jobs) {
        forge::util::recreate_dir(Path::new(pgo_dir))?;
    }

    Ok(())
}

fn clear_directory_contents(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(child)?;
        } else {
            fs::remove_file(child)?;
        }
    }
    Ok(())
}

fn unique_pgo_dirs(jobs: &[JobPlan]) -> BTreeSet<&str> {
    jobs.iter().filter_map(|job| job.pgo_dir.as_deref()).collect()
}

fn merged_environment(global: &BTreeMap<String, String>, step: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    global
        .iter()
        .chain(step)
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn validate_build_host(required: &str, actual: &str) -> Result<(), Error> {
    if required == actual {
        Ok(())
    } else {
        Err(Error::IncompatibleBuildHost {
            required: required.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

/// Restrict the calling task to the deterministic prefix of its current CPU
/// affinity. The container payload is a single-task PID 1 at this point, so
/// all later threads and subprocesses inherit this exact mask.
fn restrict_current_cpu_affinity(jobs: u32) -> Result<(), Error> {
    let current = sched_getaffinity(Pid::from_raw(0))?;
    let allowed = affinity_cpu_ids(&current)?;
    let selected = select_cpu_ids(&allowed, jobs, CpuSet::count())?;
    let selected_mask = affinity_mask(&selected)?;

    sched_setaffinity(Pid::from_raw(0), &selected_mask)?;

    // The kernel may intersect a requested mask with cpuset/cgroup policy.
    // Never continue with fewer CPUs (or a different equally-sized set) than
    // the plan declares.
    let applied = sched_getaffinity(Pid::from_raw(0))?;
    let applied = affinity_cpu_ids(&applied)?;
    let expected = usize::try_from(jobs).map_err(|_| Error::UnrepresentableCpuAffinity {
        requested: jobs,
        representable: CpuSet::count(),
    })?;
    if applied.len() != expected {
        return Err(Error::CpuAffinityCardinalityMismatch {
            expected: jobs,
            actual: applied.len(),
        });
    }
    if applied != selected {
        return Err(Error::CpuAffinityMaskMismatch {
            expected: selected,
            actual: applied,
        });
    }

    Ok(())
}

fn affinity_cpu_ids(mask: &CpuSet) -> Result<Vec<usize>, Errno> {
    let mut cpus = Vec::new();
    for cpu in 0..CpuSet::count() {
        if mask.is_set(cpu)? {
            cpus.push(cpu);
        }
    }
    Ok(cpus)
}

fn affinity_mask(cpus: &[usize]) -> Result<CpuSet, Errno> {
    let mut mask = CpuSet::new();
    for cpu in cpus {
        mask.set(*cpu)?;
    }
    Ok(mask)
}

/// Select the lowest numbered representable CPUs, independent of the order in
/// which the parent mask was supplied.
fn select_cpu_ids(allowed: &[usize], jobs: u32, representable: usize) -> Result<Vec<usize>, Error> {
    let requested = usize::try_from(jobs).map_err(|_| Error::UnrepresentableCpuAffinity {
        requested: jobs,
        representable,
    })?;
    if requested > representable {
        return Err(Error::UnrepresentableCpuAffinity {
            requested: jobs,
            representable,
        });
    }

    let allowed = allowed
        .iter()
        .copied()
        .filter(|cpu| *cpu < representable)
        .collect::<BTreeSet<_>>();
    if requested > allowed.len() {
        return Err(Error::InsufficientCpuAffinity {
            requested: jobs,
            available: allowed.len(),
        });
    }

    Ok(allowed.into_iter().take(requested).collect())
}

#[cfg(test)]
fn logged(
    command: &str,
    containment: DescendantContainment,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    logged_retaining(command, None, containment, configure)
}

fn logged_retaining(
    command: &str,
    descriptor_exec: Option<DescriptorExec>,
    containment: DescendantContainment,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    logged_with_limits(
        command,
        descriptor_exec,
        containment,
        StepExecutionLimits::production(),
        LogMode::Stream,
        configure,
    )
}

#[derive(Debug, Clone, Copy)]
struct StepExecutionLimits {
    wall_time: Duration,
    stdout_bytes: u64,
    stderr_bytes: u64,
    total_output_bytes: u64,
}

impl StepExecutionLimits {
    const fn production() -> Self {
        Self {
            wall_time: STEP_WALL_TIME_LIMIT,
            stdout_bytes: STEP_STDOUT_BYTE_LIMIT,
            stderr_bytes: STEP_STDERR_BYTE_LIMIT,
            total_output_bytes: STEP_TOTAL_OUTPUT_BYTE_LIMIT,
        }
    }

    fn stream_limit(self, stream: OutputStream) -> u64 {
        match stream {
            OutputStream::Stdout => self.stdout_bytes,
            OutputStream::Stderr => self.stderr_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogMode {
    Stream,
    Discard,
}

fn logged_with_limits(
    command: &str,
    descriptor_exec: Option<DescriptorExec>,
    containment: DescendantContainment,
    limits: StepExecutionLimits,
    log_mode: LogMode,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    let mut command = process::Command::new(command);
    configure(&mut command);
    // Frozen steps receive only their configured stdio. Mark every other
    // descriptor close-on-exec in the post-fork child; this also covers
    // descriptors inherited by Cast from its own launcher.
    unsafe {
        command.pre_exec(move || {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            set_child_resource_limits()?;
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(descriptor_exec) = &descriptor_exec {
                // SAFETY: `DescriptorExec` prepared and retained every C
                // string and pointer in the parent. The child has completed
                // its stdio, working-directory, limit, process-group, and
                // descriptor setup, so this is the final operation before
                // replacing the child image.
                descriptor_exec.execveat()?;
            }
            Ok(())
        });
    }
    let mut child = command
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .map_err(|source| StepExecutionError::Spawn { source })?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut setup_failure = set_nonblocking(stdout.as_raw_fd())
        .and_then(|()| set_nonblocking(stderr.as_raw_fd()))
        .err()
        .map(|source| StepExecutionError::PipeSetup { source });

    let output_budget = Arc::new(Mutex::new(OutputBudget::default()));
    let log_mux = Arc::new(Mutex::new(LogMux::new(log_mode)));
    let stop_readers = Arc::new(AtomicBool::new(false));
    let (alert_sender, alert_receiver) = mpsc::channel();

    let stdout_reader = spawn_log_reader(
        stdout,
        OutputStream::Stdout,
        limits,
        Arc::clone(&output_budget),
        Arc::clone(&log_mux),
        Arc::clone(&stop_readers),
        alert_sender.clone(),
    );
    let stderr_reader = spawn_log_reader(
        stderr,
        OutputStream::Stderr,
        limits,
        output_budget,
        log_mux,
        Arc::clone(&stop_readers),
        alert_sender,
    );

    let mut stdout_reader = match stdout_reader {
        Ok(reader) => Some(reader),
        Err(source) => {
            setup_failure.get_or_insert(StepExecutionError::ReaderThreadSpawn {
                stream: OutputStream::Stdout,
                source,
            });
            None
        }
    };
    let mut stderr_reader = match stderr_reader {
        Ok(reader) => Some(reader),
        Err(source) => {
            setup_failure.get_or_insert(StepExecutionError::ReaderThreadSpawn {
                stream: OutputStream::Stderr,
                source,
            });
            None
        }
    };

    let started = Instant::now();
    let terminal = if let Some(failure) = setup_failure {
        StepTerminal::Failure(failure)
    } else if let Err(source) = ::container::forward_sigint(child_pid) {
        StepTerminal::Failure(StepExecutionError::SignalForward { source })
    } else {
        monitor_step(&mut child, started, limits.wall_time, &alert_receiver)
    };

    // Readers are deliberately stopped only after the complete containment
    // boundary has been killed and the direct child has been reaped. This
    // ordering prevents a daemonized pipe holder from blocking the joins and
    // prevents descendants from surviving into the next frozen step.
    let child_was_reaped = matches!(terminal, StepTerminal::Exited(_));
    let cleanup_failure = cleanup_step(&mut child, containment, child_pid, child_was_reaped);
    stop_readers.store(true, Ordering::Release);

    let stdout_result = join_log_reader(&mut stdout_reader, OutputStream::Stdout);
    let stderr_result = join_log_reader(&mut stderr_reader, OutputStream::Stderr);
    let reader_failure = stdout_result.err().or_else(|| stderr_result.err());

    let result = match terminal {
        StepTerminal::Exited(status) => reader_failure.map_or(Ok(status), Err),
        StepTerminal::ReaderAlert => Err(reader_failure.unwrap_or(StepExecutionError::ReaderAlertLost)),
        StepTerminal::Failure(failure) => Err(failure),
    };

    match (result, cleanup_failure) {
        (result, None) => result,
        (Ok(_), Some(cleanup)) => Err(StepExecutionError::Cleanup {
            operation: cleanup.operation,
            source: cleanup.source,
        }),
        (Err(failure), Some(cleanup)) => Err(StepExecutionError::CleanupAfterFailure {
            failure: Box::new(failure),
            operation: cleanup.operation,
            source: cleanup.source,
        }),
    }
}

/// Apply only child-local limits whose semantics do not depend on the host UID
/// or on a guessed build memory/file-size requirement.
fn set_child_resource_limits() -> io::Result<()> {
    let core = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_CORE, &core) } == -1 {
        return Err(io::Error::last_os_error());
    }

    let mut inherited_nofile = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited_nofile) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let nofile_max = inherited_nofile.rlim_max.min(STEP_OPEN_FILE_LIMIT);
    let nofile = nix::libc::rlimit {
        rlim_cur: inherited_nofile.rlim_cur.min(nofile_max),
        rlim_max: nofile_max,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &nofile) } == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(io_error_from_errno)?;
    let flags = OFlag::from_bits_truncate(flags);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .map(|_| ())
        .map_err(io_error_from_errno)
}

fn io_error_from_errno(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

#[derive(Debug)]
enum StepTerminal {
    Exited(process::ExitStatus),
    ReaderAlert,
    Failure(StepExecutionError),
}

fn monitor_step(
    child: &mut process::Child,
    started: Instant,
    wall_time: Duration,
    alerts: &mpsc::Receiver<()>,
) -> StepTerminal {
    loop {
        if alerts.try_recv().is_ok() {
            return StepTerminal::ReaderAlert;
        }

        match child.try_wait() {
            Ok(Some(status)) => return StepTerminal::Exited(status),
            Ok(None) => {}
            Err(source) => return StepTerminal::Failure(StepExecutionError::Wait { source }),
        }

        if started.elapsed() >= wall_time {
            return StepTerminal::Failure(StepExecutionError::Timeout { limit: wall_time });
        }

        thread::sleep(STEP_MONITOR_INTERVAL.min(wall_time.saturating_sub(started.elapsed())));
    }
}

#[derive(Debug)]
struct CleanupFailure {
    operation: &'static str,
    source: io::Error,
}

fn cleanup_step(
    child: &mut process::Child,
    containment: DescendantContainment,
    child_pid: Pid,
    child_was_reaped: bool,
) -> Option<CleanupFailure> {
    let mut failure = terminate_step_descendants(containment, child_pid)
        .err()
        .map(|source| CleanupFailure {
            operation: "terminate containment boundary",
            source,
        });

    if !child_was_reaped {
        // Namespace-wide cleanup normally reaps the direct child itself. A
        // direct kill is also attempted so a boundary-cleanup error cannot
        // leave child.wait() blocking forever.
        match kill(child_pid, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => {
                failure.get_or_insert(CleanupFailure {
                    operation: "kill direct child",
                    source: error.into(),
                });
            }
        }

        if let Err(source) = child.wait()
            && !(containment == DescendantContainment::PidNamespace
                && source.raw_os_error() == Some(Errno::ECHILD as i32))
        {
            failure.get_or_insert(CleanupFailure {
                operation: "reap direct child",
                source,
            });
        }
    }

    failure
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescendantContainment {
    /// Production execution runs as PID 1 in a dedicated namespace. Signaling
    /// every other visible process catches daemonization, `setsid`, and
    /// double-fork escapes before the next step or packaging can begin.
    PidNamespace,
    /// Direct unit tests do not own their PID namespace. A private process
    /// group provides a safe behavioral test boundary there.
    #[cfg(test)]
    ProcessGroup,
}

fn require_pid_namespace_init(pid: Pid) -> Result<(), Error> {
    if pid.as_raw() == 1 {
        Ok(())
    } else {
        Err(Error::PidNamespaceInitRequired(pid.as_raw()))
    }
}

fn terminate_step_descendants(containment: DescendantContainment, child_pid: Pid) -> io::Result<()> {
    match containment {
        DescendantContainment::PidNamespace => {
            if getpid().as_raw() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing namespace-wide descendant cleanup outside PID 1",
                ));
            }

            match kill(descendant_signal_target(containment, child_pid), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
            loop {
                match waitpid(Pid::from_raw(-1), None) {
                    Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {}
                    Ok(
                        WaitStatus::Stopped(..)
                        | WaitStatus::PtraceEvent(..)
                        | WaitStatus::PtraceSyscall(..)
                        | WaitStatus::Continued(..)
                        | WaitStatus::StillAlive,
                    ) => {}
                    Err(Errno::EINTR) => {}
                    Err(Errno::ECHILD) => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        #[cfg(test)]
        DescendantContainment::ProcessGroup => {
            match kill(descendant_signal_target(containment, child_pid), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn descendant_signal_target(containment: DescendantContainment, _child_pid: Pid) -> Pid {
    match containment {
        DescendantContainment::PidNamespace => Pid::from_raw(-1),
        #[cfg(test)]
        DescendantContainment::ProcessGroup => Pid::from_raw(-_child_pid.as_raw()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

impl std::fmt::Display for OutputStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug, Default)]
struct OutputBudget {
    total: u64,
}

#[derive(Debug)]
struct OutputAdmission {
    accepted: usize,
    violation: Option<StepExecutionError>,
}

impl OutputBudget {
    fn admit(
        &mut self,
        stream: OutputStream,
        stream_bytes: &mut u64,
        bytes: usize,
        limits: StepExecutionLimits,
    ) -> OutputAdmission {
        let bytes = u64::try_from(bytes).expect("log read buffer length fits in u64");
        let stream_limit = limits.stream_limit(stream);
        let stream_remaining = stream_limit.saturating_sub(*stream_bytes);
        let total_remaining = limits.total_output_bytes.saturating_sub(self.total);
        let accepted = bytes.min(stream_remaining).min(total_remaining);
        *stream_bytes += accepted;
        self.total += accepted;

        let violation = if accepted == bytes {
            None
        } else if stream_remaining <= total_remaining {
            Some(StepExecutionError::OutputLimit {
                stream,
                limit: stream_limit,
                observed: stream_limit.saturating_add(1),
            })
        } else {
            Some(StepExecutionError::TotalOutputLimit {
                limit: limits.total_output_bytes,
                observed: limits.total_output_bytes.saturating_add(1),
            })
        };

        OutputAdmission {
            accepted: usize::try_from(accepted).expect("accepted bytes came from a usize-sized read"),
            violation,
        }
    }
}

#[derive(Debug)]
struct LogMux {
    mode: LogMode,
    current: Option<OutputStream>,
    at_line_start: bool,
}

impl LogMux {
    const fn new(mode: LogMode) -> Self {
        Self {
            mode,
            current: None,
            at_line_start: true,
        }
    }

    fn emit(&mut self, stream: OutputStream, mut bytes: &[u8]) -> io::Result<()> {
        if self.mode == LogMode::Discard || bytes.is_empty() {
            return Ok(());
        }

        let stdout = io::stdout();
        let mut output = stdout.lock();
        if self.current != Some(stream) && !self.at_line_start {
            output.write_all(b"\n")?;
            self.at_line_start = true;
        }
        self.current = Some(stream);

        while !bytes.is_empty() {
            if self.at_line_start {
                write!(output, "{} ", "│".dim())?;
                self.at_line_start = false;
            }

            let segment_len = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |newline| newline + 1);
            let (segment, remaining) = bytes.split_at(segment_len);
            output.write_all(segment)?;
            if segment.last() == Some(&b'\n') {
                self.at_line_start = true;
            }
            bytes = remaining;
        }
        output.flush()
    }

    fn finish(&mut self, stream: OutputStream) -> io::Result<()> {
        if self.mode == LogMode::Discard || self.current != Some(stream) || self.at_line_start {
            return Ok(());
        }

        let stdout = io::stdout();
        let mut output = stdout.lock();
        output.write_all(b"\n")?;
        output.flush()?;
        self.at_line_start = true;
        Ok(())
    }
}

type LogReader = thread::JoinHandle<Result<(), StepExecutionError>>;

#[allow(clippy::too_many_arguments)]
fn spawn_log_reader<R>(
    pipe: R,
    stream: OutputStream,
    limits: StepExecutionLimits,
    output_budget: Arc<Mutex<OutputBudget>>,
    log_mux: Arc<Mutex<LogMux>>,
    stop: Arc<AtomicBool>,
    alert: mpsc::Sender<()>,
) -> io::Result<LogReader>
where
    R: io::Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("mason-step-{stream}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drain_log(pipe, stream, limits, &output_budget, &log_mux, &stop)
            }));
            match result {
                Ok(result) => {
                    if result.is_err() {
                        let _ = alert.send(());
                    }
                    result
                }
                Err(payload) => {
                    let _ = alert.send(());
                    std::panic::resume_unwind(payload)
                }
            }
        })
}

fn drain_log<R>(
    mut pipe: R,
    stream: OutputStream,
    limits: StepExecutionLimits,
    output_budget: &Mutex<OutputBudget>,
    log_mux: &Mutex<LogMux>,
    stop: &AtomicBool,
) -> Result<(), StepExecutionError>
where
    R: io::Read,
{
    let mut buffer = [0_u8; LOG_READ_BUFFER_BYTES];
    let mut stream_bytes = 0_u64;

    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(STEP_MONITOR_INTERVAL);
                continue;
            }
            Err(source) => return Err(StepExecutionError::OutputRead { stream, source }),
        };

        let admission = output_budget
            .lock()
            .map_err(|_| StepExecutionError::OutputBudgetPoisoned)?
            .admit(stream, &mut stream_bytes, read, limits);

        if admission.accepted > 0 {
            log_mux
                .lock()
                .map_err(|_| StepExecutionError::LogMuxPoisoned)?
                .emit(stream, &buffer[..admission.accepted])
                .map_err(|source| StepExecutionError::OutputWrite { stream, source })?;
        }

        if let Some(violation) = admission.violation {
            return Err(violation);
        }
    }

    log_mux
        .lock()
        .map_err(|_| StepExecutionError::LogMuxPoisoned)?
        .finish(stream)
        .map_err(|source| StepExecutionError::OutputWrite { stream, source })
}

fn join_log_reader(reader: &mut Option<LogReader>, stream: OutputStream) -> Result<(), StepExecutionError> {
    let Some(reader) = reader.take() else {
        return Ok(());
    };
    match reader.join() {
        Ok(result) => result,
        Err(_) => Err(StepExecutionError::ReaderThreadPanicked { stream }),
    }
}

fn target_prefix(target: &str, index: usize) -> String {
    format!("{}{}", if index > 0 { "\n" } else { "" }, target.dim())
}

fn pgo_stage_prefix(stage: &str, index: usize) -> String {
    let newline = if index > 0 {
        format!("{}\n", "│".dim())
    } else {
        String::new()
    };
    format!("{newline}{}", format!("│pgo-{stage}").dim())
}

fn phase_prefix(phase: &str, is_pgo: bool, index: usize) -> String {
    let pipes = if is_pgo { "││".dim() } else { "│".dim() };
    let newline = if index > 0 { format!("{pipes}\n") } else { String::new() };
    format!("{newline}{pipes}{}", phase.dim())
}

fn parse_pgo_stage(stage: &str) -> Result<Stage, Error> {
    match stage {
        "one" => Ok(Stage::One),
        "two" => Ok(Stage::Two),
        "use" => Ok(Stage::Use),
        _ => Err(Error::UnsupportedPgoStage(stage.to_owned())),
    }
}

fn parse_phase(phase: &str) -> Result<Phase, Error> {
    match phase.to_ascii_lowercase().as_str() {
        "prepare" => Ok(Phase::Prepare),
        "setup" => Ok(Phase::Setup),
        "build" => Ok(Phase::Build),
        "install" => Ok(Phase::Install),
        "check" => Ok(Phase::Check),
        "workload" => Ok(Phase::Workload),
        _ => Err(Error::UnsupportedPhase(phase.to_owned())),
    }
}

#[derive(Debug, Error)]
pub enum StepExecutionError {
    #[error("could not spawn frozen build step: {source}")]
    Spawn {
        #[source]
        source: io::Error,
    },
    #[error("could not configure frozen build-step output pipes: {source}")]
    PipeSetup {
        #[source]
        source: io::Error,
    },
    #[error("could not start frozen build-step {stream} reader: {source}")]
    ReaderThreadSpawn {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("could not install frozen build-step SIGINT forwarding: {source}")]
    SignalForward {
        #[source]
        source: nix::Error,
    },
    #[error("frozen build step exceeded its operational wall limit of {limit:?}")]
    Timeout { limit: Duration },
    #[error("could not wait for frozen build step: {source}")]
    Wait {
        #[source]
        source: io::Error,
    },
    #[error("frozen build-step {stream} produced {observed} bytes, exceeding its {limit}-byte ceiling")]
    OutputLimit {
        stream: OutputStream,
        limit: u64,
        observed: u64,
    },
    #[error(
        "frozen build-step stdout and stderr produced {observed} bytes, exceeding their combined {limit}-byte ceiling"
    )]
    TotalOutputLimit { limit: u64, observed: u64 },
    #[error("could not read frozen build-step {stream}: {source}")]
    OutputRead {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("could not stream frozen build-step {stream}: {source}")]
    OutputWrite {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("frozen build-step output budget lock was poisoned")]
    OutputBudgetPoisoned,
    #[error("frozen build-step log multiplexer lock was poisoned")]
    LogMuxPoisoned,
    #[error("frozen build-step output reader reported a failure without preserving it")]
    ReaderAlertLost,
    #[error("frozen build-step {stream} reader panicked")]
    ReaderThreadPanicked { stream: OutputStream },
    #[error("frozen build-step cleanup `{operation}` failed: {source}")]
    Cleanup {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{failure}; frozen build-step cleanup `{operation}` also failed: {source}")]
    CleanupAfterFailure {
        failure: Box<StepExecutionError>,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    InvalidPlan(#[from] stone_recipe::derivation::DerivationValidationError),
    #[error("plan requires executor ABI {found}, but this Cast provides {expected}")]
    IncompatibleExecutor { expected: &'static str, found: String },
    #[error("plan was created by Cast {found}, but executor is {expected}")]
    IncompatibleCast { expected: String, found: String },
    #[error("plan requires Cast implementation {found}, but executor provides {expected}")]
    IncompatibleCastSemantics { expected: String, found: String },
    #[error("plan executor identity is {found}, but this Cast requires {expected}")]
    IncompatibleExecutorFingerprint { expected: String, found: String },
    #[error("frozen plan requires build host `{required}`, but Cast is running on `{actual}`")]
    IncompatibleBuildHost { required: String, actual: String },
    #[error("frozen executor must run as PID 1 in its dedicated PID namespace, got PID {0}")]
    PidNamespaceInitRequired(i32),
    #[error("frozen execution requests {requested} CPUs, but this executor can represent at most {representable}")]
    UnrepresentableCpuAffinity { requested: u32, representable: usize },
    #[error(
        "frozen execution requests {requested} CPUs, but the current allowed affinity provides only {available} representable CPUs"
    )]
    InsufficientCpuAffinity { requested: u32, available: usize },
    #[error("kernel applied {actual} CPUs to frozen execution; expected exactly {expected}")]
    CpuAffinityCardinalityMismatch { expected: u32, actual: usize },
    #[error("kernel applied CPU affinity {actual:?}; expected deterministic affinity {expected:?}")]
    CpuAffinityMaskMismatch { expected: Vec<usize>, actual: Vec<usize> },
    #[error("unsupported frozen PGO stage {0}")]
    UnsupportedPgoStage(String),
    #[error("unsupported frozen phase {0}")]
    UnsupportedPhase(String),
    #[error("frozen archive source index {0} is invalid")]
    InvalidArchiveSource(u32),
    #[error("retain frozen built executable {path:?}")]
    BuiltExecutable {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("archive extraction")]
    Archive(#[from] crate::archive::Error),
    #[error(transparent)]
    StepExecution(#[from] StepExecutionError),
    #[error("build step failed with status code {0}")]
    Code(i32),
    #[error("build step stopped by signal {}", .0.as_str())]
    Signal(Signal),
    #[error("build step stopped by an unknown signal")]
    UnknownSignal,
    #[error("container")]
    Container(#[from] ::container::Error),
    #[error("nix")]
    Nix(#[from] nix::Error),
    #[error("io")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
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
        let retained_descriptor =
            fcntl(retained.as_raw_fd(), FcntlArg::F_DUPFD_CLOEXEC(HIGH_DESCRIPTOR_MINIMUM)).unwrap();
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
}
