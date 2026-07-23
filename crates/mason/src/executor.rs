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

include!("executor/process_supervision.rs");
include!("executor/output_capture.rs");
include!("executor/errors.rs");

#[cfg(test)]
mod tests;
