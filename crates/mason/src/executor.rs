// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Execution of an already-frozen derivation plan.
//!
//! This module deliberately has no access to recipes, policy macros,
//! profiles, or Forge provider resolution. Those belong to planning.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process, thread,
};

use fs_err as fs;
use nix::{
    errno::Errno,
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
                    self.run_step(step)?;
                }
                timing.finish(timer);
            }
        }
        println!();
        Ok(())
    }

    fn run_step(&self, step: &StepPlan) -> Result<(), Error> {
        let (program, args, step_environment, working_dir) = match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => (program.path.as_str(), args.clone(), environment, working_dir.as_str()),
            StepPlan::Shell {
                interpreter,
                script,
                environment,
                working_dir,
                ..
            } => (
                interpreter.path.as_str(),
                vec!["-c".to_owned(), script.clone()],
                environment,
                working_dir.as_str(),
            ),
        };
        let environment = merged_environment(&self.plan.environment, step_environment);
        let status = logged(program, DescendantContainment::PidNamespace, |command| {
            command
                .args(args)
                .env_clear()
                .envs(environment)
                .current_dir(working_dir)
        })?;
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

fn logged(
    command: &str,
    containment: DescendantContainment,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> io::Result<process::ExitStatus> {
    let mut command = process::Command::new(command);
    configure(&mut command);
    // Frozen steps receive only their configured stdio. Mark every other
    // descriptor close-on-exec in the post-fork child; this also covers
    // descriptors inherited by Cast from its own launcher.
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let stdout = log(child.stdout.take().expect("piped stdout"));
    let stderr = log(child.stderr.take().expect("piped stderr"));
    ::container::forward_sigint(child_pid)?;
    let result = child.wait()?;
    terminate_step_descendants(containment, child_pid)?;
    let _ = stdout.join();
    let _ = stderr.join();
    Ok(result)
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

fn log<R>(pipe: R) -> thread::JoinHandle<()>
where
    R: io::Read + Send + 'static,
{
    use std::io::BufRead;
    thread::spawn(move || {
        for line in io::BufReader::new(pipe).lines().map_while(Result::ok) {
            println!("{} {line}", "│".dim());
        }
    })
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
        os::{fd::AsRawFd, unix::fs::symlink},
        path::PathBuf,
        process::Command,
        time::{Duration, Instant},
    };

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    use stone_recipe::derivation::BuilderLayout;

    use super::*;
    use crate::package::test_derivation_plan;

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
        plan.execution.compiler_cache = true;
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
        plan.execution.compiler_cache = false;
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
