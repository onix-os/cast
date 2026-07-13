// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Execution of an already-frozen derivation plan.
//!
//! This module deliberately has no access to recipes, policy macros,
//! profiles, or Moss provider resolution. Those belong to planning.

use std::{
    collections::BTreeMap,
    io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process, thread,
};

use fs_err as fs;
use nix::{
    sys::signal::Signal,
    unistd::{Pid, getpgrp, setpgid},
};
use stone_recipe::derivation::{DerivationPlan, JobPlan, PhasePlan, StepPlan};
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
        if plan.build_lock.builder.name != crate::planner::EXECUTOR_ABI {
            return Err(Error::IncompatibleExecutor {
                expected: crate::planner::EXECUTOR_ABI,
                found: plan.build_lock.builder.name.clone(),
            });
        }
        let current = tools_buildinfo::get_simple_version();
        if plan.boulder_version != current {
            return Err(Error::IncompatibleBoulder {
                expected: current,
                found: plan.boulder_version.clone(),
            });
        }
        validate_build_host(&plan.build_lock.build_platform.architecture, std::env::consts::ARCH)?;
        validate_jobs(plan)?;
        validate_execution_symbols(&plan.jobs)?;
        Ok(Self { plan })
    }

    pub fn run(&self, timing: &mut Timing) -> Result<(), Error> {
        setpgid(Pid::from_raw(0), Pid::from_raw(0))?;
        let pgid = getpgrp();
        ::container::set_term_fg(pgid)?;

        clear_directory_contents(Path::new(&self.plan.layout.build_dir))?;
        let target = &self.plan.build_lock.target.name;
        for pgo_dir in unique_pgo_dirs(&self.plan.jobs) {
            moss::util::recreate_dir(Path::new(pgo_dir))?;
        }
        for (job_index, job) in self.plan.jobs.iter().enumerate() {
            println!("{}", target_prefix(target, job_index));
            fs::create_dir_all(&job.build_dir)?;
            moss::util::recreate_dir(Path::new(&job.work_dir))?;
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
            } => (program.as_str(), args.clone(), environment, working_dir.as_str()),
            StepPlan::Shell {
                interpreter,
                script,
                environment,
                working_dir,
            } => (
                interpreter.as_str(),
                vec!["-c".to_owned(), script.clone()],
                environment,
                working_dir.as_str(),
            ),
        };
        let environment = merged_environment(&self.plan.environment, step_environment);
        let status = logged(program, |command| {
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

fn unique_pgo_dirs(jobs: &[JobPlan]) -> std::collections::BTreeSet<&str> {
    jobs.iter().filter_map(|job| job.pgo_dir.as_deref()).collect()
}

fn merged_environment(global: &BTreeMap<String, String>, step: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    global
        .iter()
        .chain(step)
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn validate_jobs(plan: &DerivationPlan) -> Result<(), Error> {
    let layout_root = Path::new(&plan.layout.build_dir);
    validate_job_paths(layout_root, &plan.jobs)
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

/// Reject executor vocabulary this Boulder cannot run before runtime setup
/// mutates the build root, fetches sources, or installs the locked closure.
fn validate_execution_symbols(jobs: &[JobPlan]) -> Result<(), Error> {
    for job in jobs {
        if let Some(stage) = &job.pgo_stage {
            parse_pgo_stage(stage)?;
        }
        for phase in &job.phases {
            parse_phase(&phase.name)?;
        }
    }
    Ok(())
}

fn validate_job_paths(layout_root: &Path, jobs: &[JobPlan]) -> Result<(), Error> {
    for (job_index, job) in jobs.iter().enumerate() {
        let build_dir = Path::new(&job.build_dir);
        let work_dir = Path::new(&job.work_dir);
        if !safe_absolute(build_dir)
            || !safe_absolute(work_dir)
            || !build_dir.starts_with(layout_root)
            || !work_dir.starts_with(build_dir)
        {
            return Err(Error::NonAbsoluteJobPath { job: job_index });
        }
        match (&job.pgo_stage, &job.pgo_dir) {
            (Some(_), Some(pgo_dir))
                if safe_absolute(Path::new(pgo_dir)) && Path::new(pgo_dir).starts_with(layout_root) => {}
            (None, None) => {}
            _ => return Err(Error::InvalidPgoDirectory { job: job_index }),
        }
        for phase in &job.phases {
            validate_phase_working_dirs(job_index, job, phase)?;
        }
    }
    Ok(())
}

fn validate_phase_working_dirs(job_index: usize, job: &JobPlan, phase: &PhasePlan) -> Result<(), Error> {
    let build_dir = Path::new(&job.build_dir);
    for step in phase.pre.iter().chain(&phase.steps).chain(&phase.post) {
        let working_dir = match step {
            StepPlan::Run { working_dir, .. } | StepPlan::Shell { working_dir, .. } => Path::new(working_dir),
        };
        if !safe_absolute(working_dir) || !working_dir.starts_with(build_dir) {
            return Err(Error::WorkingDirectoryOutsideBuild {
                job: job_index,
                phase: phase.name.clone(),
                working_dir: working_dir.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

fn logged(
    command: &str,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> io::Result<process::ExitStatus> {
    let mut command = process::Command::new(command);
    configure(&mut command);
    let mut child = command
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()?;
    let stdout = log(child.stdout.take().expect("piped stdout"));
    let stderr = log(child.stderr.take().expect("piped stderr"));
    ::container::forward_sigint(Pid::from_raw(child.id() as i32))?;
    let result = child.wait()?;
    let _ = stdout.join();
    let _ = stderr.join();
    Ok(result)
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
    #[error("plan requires executor ABI {found}, but this Boulder provides {expected}")]
    IncompatibleExecutor { expected: &'static str, found: String },
    #[error("plan was created by Boulder {found}, but executor is {expected}")]
    IncompatibleBoulder { expected: String, found: String },
    #[error("plan job {job} has a non-absolute build or work directory")]
    NonAbsoluteJobPath { job: usize },
    #[error("plan job {job} phase {phase} has working directory outside its build root: {working_dir:?}")]
    WorkingDirectoryOutsideBuild {
        job: usize,
        phase: String,
        working_dir: PathBuf,
    },
    #[error("plan job {job} has an invalid or missing frozen PGO directory")]
    InvalidPgoDirectory { job: usize },
    #[error("frozen plan requires build host `{required}`, but Boulder is running on `{actual}`")]
    IncompatibleBuildHost { required: String, actual: String },
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
    use super::*;

    fn shell(script: &str, working_dir: &str, environment: &[(&str, &str)]) -> StepPlan {
        StepPlan::Shell {
            interpreter: "/usr/bin/bash".to_owned(),
            script: script.to_owned(),
            environment: environment
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            working_dir: working_dir.to_owned(),
        }
    }

    #[test]
    fn validation_rejects_step_working_directories_outside_frozen_build_root() {
        let job = JobPlan {
            pgo_stage: None,
            pgo_dir: None,
            build_dir: "/mason/build/target".to_owned(),
            work_dir: "/mason/build/target/source".to_owned(),
            phases: vec![PhasePlan {
                name: "build".to_owned(),
                pre: Vec::new(),
                steps: vec![shell("true", "/tmp/ambient", &[])],
                post: Vec::new(),
            }],
        };

        assert!(matches!(
            validate_job_paths(Path::new("/mason/build"), &[job]),
            Err(Error::WorkingDirectoryOutsideBuild { .. })
        ));
    }

    #[test]
    fn validation_contains_job_and_pgo_paths_under_frozen_layout() {
        let mut job = JobPlan {
            pgo_stage: Some("one".to_owned()),
            pgo_dir: Some("/outside/pgo".to_owned()),
            build_dir: "/mason/build/target".to_owned(),
            work_dir: "/mason/build/target/source".to_owned(),
            phases: Vec::new(),
        };
        assert!(matches!(
            validate_job_paths(Path::new("/mason/build"), &[job.clone()]),
            Err(Error::InvalidPgoDirectory { job: 0 })
        ));

        job.pgo_dir = Some("/mason/build/target-pgo".to_owned());
        validate_job_paths(Path::new("/mason/build"), &[job.clone()]).unwrap();
        let repeated = job.clone();
        assert_eq!(
            unique_pgo_dirs(&[job.clone(), repeated]),
            ["/mason/build/target-pgo"].into()
        );

        job.build_dir = "/outside/target".to_owned();
        assert!(matches!(
            validate_job_paths(Path::new("/mason/build"), &[job]),
            Err(Error::NonAbsoluteJobPath { job: 0 })
        ));
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
    fn frozen_build_root_is_cleared_before_execution() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stale-file"), b"stale").unwrap();
        fs::create_dir(root.path().join("stale-dir")).unwrap();
        fs::write(root.path().join("stale-dir/nested"), b"stale").unwrap();

        clear_directory_contents(root.path()).unwrap();

        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn executor_vocabulary_is_accepted_before_execution() {
        let mut jobs = vec![JobPlan {
            pgo_stage: Some("one".to_owned()),
            pgo_dir: Some("/mason/build/target-pgo".to_owned()),
            build_dir: "/mason/build/target".to_owned(),
            work_dir: "/mason/build/target/source".to_owned(),
            phases: vec![PhasePlan {
                name: "build".to_owned(),
                pre: Vec::new(),
                steps: Vec::new(),
                post: Vec::new(),
            }],
        }];

        validate_execution_symbols(&jobs).unwrap();
        jobs[0].phases[0].name = "ambient-phase".to_owned();
        assert!(matches!(
            validate_execution_symbols(&jobs),
            Err(Error::UnsupportedPhase(phase)) if phase == "ambient-phase"
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
}
