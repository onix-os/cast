// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    num::NonZeroUsize,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process, thread,
};

use fs_err as fs;
use itertools::Itertools;
use moss::{repository, util};
use nix::{
    sys::signal::Signal,
    unistd::{Pid, getpgrp, setpgid},
};
use stone_recipe::{
    Script,
    derivation::DerivationPlan,
    script::{self, Breakpoint},
};
use thiserror::Error;
use tui::Styled;

use self::job::Job;
use crate::{
    Env, Macros, Paths, Recipe, Timing,
    architecture::BuildTarget,
    container, macros, profile, recipe, timing,
    upstream::{self, Upstream},
};

pub mod job;
pub mod pgo;
pub(crate) mod root;

pub struct Builder {
    pub targets: Vec<Target>,
    pub recipe: Recipe,
    pub paths: Paths,
    pub macros: Macros,
    pub profile: profile::Id,
    pub profile_fingerprints: Vec<gluon_config::EvaluationFingerprint>,
    pub ccache: bool,
    pub env: Env,
    upstreams: Vec<Upstream>,
    repos: repository::Map,
}

pub struct Target {
    pub build_target: BuildTarget,
    pub policy: macros::PolicySelection,
    pub jobs: Vec<Job>,
}

impl Builder {
    pub fn new(
        recipe_path: &Path,
        verify_against_manifest: Option<PathBuf>,
        env: Env,
        profile: profile::Id,
        ccache: bool,
        output_dir: impl Into<PathBuf>,
    ) -> Result<Self, Error> {
        Self::new_with_jobs(
            recipe_path,
            verify_against_manifest,
            env,
            profile,
            ccache,
            output_dir,
            util::num_cpus(),
            None,
        )
    }

    pub(crate) fn new_with_jobs(
        recipe_path: &Path,
        verify_against_manifest: Option<PathBuf>,
        env: Env,
        profile: profile::Id,
        ccache: bool,
        output_dir: impl Into<PathBuf>,
        jobs: NonZeroUsize,
        source_date_epoch: Option<i64>,
    ) -> Result<Self, Error> {
        let recipe = match source_date_epoch {
            Some(epoch) => {
                let build_time =
                    chrono::DateTime::from_timestamp(epoch, 0).ok_or(Error::InvalidSourceDateEpoch(epoch))?;
                Recipe::load_at(recipe_path, build_time)?
            }
            None => Recipe::load(recipe_path)?,
        };

        let macros = Macros::load(&env)?;

        let paths = Paths::new(&recipe, verify_against_manifest, &env.cache_dir, "/mason", output_dir)?;

        let build_targets = recipe.build_targets();

        if build_targets.is_empty() {
            return Err(Error::NoBuildTargets);
        }

        let targets = build_targets
            .into_iter()
            .map(|build_target| {
                let stages = pgo::stages(&recipe, build_target)
                    .map(|stages| stages.into_iter().map(Some).collect::<Vec<_>>())
                    .unwrap_or_else(|| vec![None]);

                let jobs = stages
                    .into_iter()
                    .map(|stage| Job::new(build_target, stage, &recipe, &paths, &macros, ccache, jobs))
                    .collect::<Result<Vec<_>, _>>()?;

                Ok(Target {
                    build_target,
                    policy: macros.selection(build_target),
                    jobs,
                })
            })
            .collect::<Result<Vec<_>, job::Error>>()?;

        let upstreams = upstream::parse_recipe(&recipe)?;

        let profiles = profile::Manager::new(&env)?;
        let repos = profiles.repositories(&profile)?.clone();
        let profile_fingerprints = profiles.fingerprints.clone();

        Ok(Self {
            targets,
            recipe,
            paths,
            macros,
            profile,
            profile_fingerprints,
            ccache,
            env,
            upstreams,
            repos,
        })
    }

    pub fn extra_deps(&self) -> impl Iterator<Item = &str> {
        self.targets.iter().flat_map(|target| {
            target.jobs.iter().flat_map(|job| {
                job.phases
                    .values()
                    .flat_map(|script| script.dependencies.iter().map(String::as_str))
            })
        })
    }

    pub(crate) fn repositories(&self) -> &repository::Map {
        &self.repos
    }

    pub fn setup(
        &self,
        timing: &mut Timing,
        initialize_timer: timing::Timer,
        update_repos: bool,
    ) -> Result<Vec<upstream::Stored>, Error> {
        // Recreate artifacts
        util::recreate_dir(&self.paths.artefacts().host).map_err(Error::RecreateArtefactsDir)?;

        // Recreate rootfs
        root::recreate(self)?;

        // Populate rootfs
        root::populate(self, self.repos.clone(), timing, initialize_timer, update_repos)?;

        let timer = timing.begin(timing::Kind::Fetch);

        // Sync (fetch & share) upstreams to rootfs
        let stored = upstream::sync(
            &self.recipe,
            &self.upstreams,
            &self.paths.upstreams().host,
            &self.paths.guest_host_path(&self.paths.upstreams()),
        )?;

        timing.finish(timer);

        Ok(stored)
    }

    /// Prepare runtime state from an already-frozen derivation.
    ///
    /// Repository refresh and provider resolution are intentionally absent:
    /// they must finish before the plan crosses the freeze boundary.
    pub fn setup_locked(
        &self,
        plan: &DerivationPlan,
        timing: &mut Timing,
        initialize_timer: timing::Timer,
    ) -> Result<Vec<upstream::Stored>, Error> {
        util::recreate_dir(&self.paths.artefacts().host).map_err(Error::RecreateArtefactsDir)?;
        root::recreate(self)?;
        root::populate_locked(self, &plan.build_lock, timing, initialize_timer)?;

        let timer = timing.begin(timing::Kind::Fetch);
        let stored = upstream::sync_locked(
            &plan.sources,
            &self.paths.upstreams().host,
            &self.paths.guest_host_path(&self.paths.upstreams()),
        )?;
        timing.finish(timer);
        Ok(stored)
    }

    pub fn cleanup(&self) -> Result<(), Error> {
        // Remove rootfs
        root::remove(self)?;

        // Remove artifacts
        if self.paths.artefacts().host.exists() {
            fs::remove_dir_all(&self.paths.artefacts().host)?;
        }

        // Remove build dir
        if self.paths.build().host.exists() {
            fs::remove_dir_all(&self.paths.build().host)?;
        }

        // Remove downloaded upstreams
        upstream::remove(&self.paths.upstreams().host, &self.upstreams)?;

        // Prune moss cache, retaining stones from the repos defined
        // by our boulder profile
        moss::Client::builder("boulder", moss::Installation::open(&self.env.moss_dir, None)?)
            .repositories(self.repos.clone())
            .build()?
            .prune_cache()?;

        Ok(())
    }

    pub fn build(&self, timing: &mut Timing) -> Result<(), Error> {
        // Set ourselves into our own process group
        // and set it as fg term
        //
        // This is so we can restore this process back as
        // the fg term after using `bash` for chroot below
        // so we can reestablish SIGINT forwarding to scripts
        setpgid(Pid::from_raw(0), Pid::from_raw(0))?;
        let pgid = getpgrp();
        ::container::set_term_fg(pgid)?;

        for (i, target) in self.targets.iter().enumerate() {
            println!("{}", build_target_prefix(target.build_target, i));

            for (i, job) in target.jobs.iter().enumerate() {
                let is_pgo = job.pgo_stage.is_some();

                // Recreate work dir for each job
                util::recreate_dir(&job.work_dir)?;
                // Ensure pgo dir exists
                if is_pgo {
                    let pgo_dir = PathBuf::from(format!("{}-pgo", job.build_dir.display()));
                    util::ensure_dir_exists(&pgo_dir)?;
                }

                if let Some(stage) = job.pgo_stage {
                    println!("{}", pgo_stage_prefix(stage, i));
                }

                for (i, (phase, script)) in job.phases.iter().enumerate() {
                    println!("{}", phase_prefix(*phase, is_pgo, i));

                    let build_dir = &job.build_dir;
                    let work_dir = &job.work_dir;
                    let current_dir = if work_dir.exists() { &work_dir } else { &build_dir };

                    let timer = timing.begin(timing::Kind::Build(timing::Build {
                        target: job.target,
                        pgo_stage: job.pgo_stage,
                        phase: *phase,
                    }));

                    for command in &script.commands {
                        match command {
                            script::Command::Break(breakpoint) => {
                                let line_num = breakpoint_script_line(breakpoint);

                                println!(
                                    "\n{} in {phase} script at line {line_num} {}",
                                    "Breakpoint".bold(),
                                    if breakpoint.exit {
                                        "(exit)".dim()
                                    } else {
                                        "(continue)".dim()
                                    },
                                );

                                // Write env to $HOME/.profile
                                fs::write(build_dir.join(".profile"), format_profile(script))?;

                                let mut command = process::Command::new("/usr/bin/bash")
                                    .arg("--login")
                                    .env_clear()
                                    .env("HOME", build_dir)
                                    .env("PATH", "/usr/bin:/usr/sbin")
                                    .env("TERM", "xterm-256color")
                                    .current_dir(current_dir)
                                    .spawn()?;

                                command.wait()?;

                                // Restore ourselves as fg term since bash steals it
                                ::container::set_term_fg(pgid)?;

                                if breakpoint.exit {
                                    return Ok(());
                                }
                            }
                            script::Command::Content(content) => {
                                // TODO: Proper temp file
                                let script_path = "/tmp/script";
                                fs::write(script_path, content).unwrap();

                                let result = logged(*phase, is_pgo, "/usr/bin/bash", |command| {
                                    command
                                        .arg(script_path)
                                        .env_clear()
                                        .env("HOME", build_dir)
                                        .env("PATH", "/usr/bin:/usr/sbin")
                                        .current_dir(current_dir)
                                })?;

                                if !result.success() {
                                    match result.code() {
                                        Some(code) => {
                                            return Err(Error::Code(code));
                                        }
                                        None => {
                                            if let Some(signal) = result
                                                .signal()
                                                .or_else(|| result.stopped_signal())
                                                .and_then(|i| Signal::try_from(i).ok())
                                            {
                                                return Err(Error::Signal(signal));
                                            } else {
                                                return Err(Error::UnknownSignal);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    timing.finish(timer);
                }
            }
        }

        println!();

        Ok(())
    }
}

pub fn build_target_prefix(target: BuildTarget, i: usize) -> String {
    let newline = if i > 0 { "\n".into() } else { String::default() };

    format!("{newline}{}", target.to_string().dim())
}

pub fn pgo_stage_prefix(stage: pgo::Stage, i: usize) -> String {
    let newline = if i > 0 {
        format!("{}\n", "│".dim())
    } else {
        String::default()
    };

    format!("{newline}{}", format!("│pgo-{stage}").dim())
}

pub fn phase_prefix(phase: job::Phase, is_pgo: bool, i: usize) -> String {
    let pipes = if is_pgo { "││".dim() } else { "│".dim() };
    let newline = if i > 0 { format!("{pipes}\n") } else { String::default() };

    format!("{newline}{pipes}{}", phase.styled(phase))
}

fn logged(
    phase: job::Phase,
    is_pgo: bool,
    command: &str,
    f: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> io::Result<process::ExitStatus> {
    let mut command = process::Command::new(command);

    f(&mut command);

    let mut child = command
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()?;

    // Log stdout and stderr
    let stdout_log = log(phase, is_pgo, child.stdout.take().unwrap());
    let stderr_log = log(phase, is_pgo, child.stderr.take().unwrap());

    // Forward SIGINT to this process
    ::container::forward_sigint(Pid::from_raw(child.id() as i32))?;

    let result = child.wait()?;

    let _ = stdout_log.join();
    let _ = stderr_log.join();

    Ok(result)
}

fn log<R>(phase: job::Phase, is_pgo: bool, pipe: R) -> thread::JoinHandle<()>
where
    R: io::Read + Send + 'static,
{
    use std::io::BufRead;

    thread::spawn(move || {
        let pgo = if is_pgo { "│" } else { "" }.dim();
        let kind = phase.styled(format!("{}│", phase.abbrev()));
        let tag = format!("{}{pgo}{kind}", "│".dim());

        let mut lines = io::BufReader::new(pipe).lines();

        while let Some(Ok(line)) = lines.next() {
            println!("{tag} {line}");
        }
    })
}

pub fn format_profile(script: &Script) -> String {
    let env = script
        .env
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with("#!") && !line.starts_with("set -") && !line.starts_with("TERM="))
        .join("\n");

    let action_functions = script
        .resolved_actions
        .iter()
        .map(|(identifier, command)| format!("a_{identifier}() {{\n{command}\n}}\nexport -f a_{identifier}"))
        .join("\n");

    let definition_vars = script
        .resolved_definitions
        .iter()
        .map(|(identifier, var)| format!("d_{identifier}=\"{var}\"; export d_{identifier}"))
        .join("\n");

    format!("{env}\n{action_functions}\n{definition_vars}")
}

/// Return the one-based line of the breakpoint in the evaluated phase script.
///
/// A Gluon phase may be constructed by functions or imported from another
/// module, so searching the root recipe text cannot recover an authoritative
/// authored source line. The script parser, however, always knows the stable
/// phase-local line at which it encountered the breakpoint.
fn breakpoint_script_line(breakpoint: &Breakpoint) -> usize {
    breakpoint.line_num + 1
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("no supported build targets for recipe")]
    NoBuildTargets,
    #[error("invalid SOURCE_DATE_EPOCH {0}")]
    InvalidSourceDateEpoch(i64),
    #[error("macros")]
    Macros(#[from] macros::Error),
    #[error("job")]
    Job(#[from] job::Error),
    #[error("profile")]
    Profile(#[from] profile::Error),
    #[error("root")]
    Root(#[from] root::Error),
    #[error("upstream")]
    Upstream(#[from] upstream::Error),
    #[error("container")]
    Container(#[from] container::Error),
    #[error("recipe")]
    Recipe(#[from] recipe::Error),
    #[error("failed with status code {0}")]
    Code(i32),
    #[error("stopped by signal {}", .0.as_str())]
    Signal(Signal),
    #[error("stopped by unknown signal")]
    UnknownSignal,
    #[error("nix")]
    Nix(#[from] nix::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("recreate artefacts dir")]
    RecreateArtefactsDir(#[source] io::Error),
    #[error("moss client")]
    MossClient(#[from] moss::client::Error),
    #[error("moss installation")]
    MossInstallation(#[from] moss::installation::Error),
}

#[cfg(test)]
mod tests {
    use stone_recipe::script::{Command, Parser};

    use super::*;

    #[test]
    fn breakpoint_line_is_one_based_within_the_evaluated_phase() {
        let script = Parser::new()
            .parse("echo preparing\n\n%break_continue\necho continuing")
            .unwrap();
        let breakpoint = script
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Break(breakpoint) => Some(breakpoint),
                Command::Content(_) => None,
            })
            .unwrap();

        assert_eq!(breakpoint_script_line(breakpoint), 3);
    }

    #[test]
    fn breakpoint_on_first_phase_line_is_line_one() {
        let breakpoint = Breakpoint {
            line_num: 0,
            exit: true,
        };

        assert_eq!(breakpoint_script_line(&breakpoint), 1);
    }
}
