// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Execution of an already-frozen derivation plan.
//!
//! This module deliberately has no access to recipes, policy macros,
//! profiles, or Moss provider resolution. Those belong to planning.

use std::{collections::BTreeMap, io, os::unix::process::ExitStatusExt, path::Path, process, thread};

use fs_err as fs;
use nix::{
    sys::signal::Signal,
    unistd::{Pid, getpgrp, setpgid},
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
    fn frozen_build_root_is_cleared_before_execution() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stale-file"), b"stale").unwrap();
        fs::create_dir(root.path().join("stale-dir")).unwrap();
        fs::write(root.path().join("stale-dir/nested"), b"stale").unwrap();

        clear_directory_contents(root.path()).unwrap();

        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
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
}
