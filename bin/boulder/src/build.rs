// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use fs_err as fs;
use itertools::Itertools;
use moss::{repository, util};
use stone_recipe::{Script, derivation::DerivationPlan};
use thiserror::Error;
use tui::Styled;

use self::job::Job;
use crate::{Env, Macros, Paths, Recipe, Timing, architecture::BuildTarget, macros, profile, recipe, timing, upstream};

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
    repos: repository::Map,
}

pub struct Target {
    pub build_target: BuildTarget,
    pub policy: macros::PolicySelection,
    pub jobs: Vec<Job>,
}

/// Host runtime resources retained after semantic planning is complete.
pub struct Runtime {
    pub paths: Paths,
    moss_dir: PathBuf,
    repositories: repository::Map,
}

impl Builder {
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

    pub fn into_runtime(self) -> Runtime {
        Runtime {
            paths: self.paths,
            moss_dir: self.env.moss_dir,
            repositories: self.repos,
        }
    }
}

impl Runtime {
    pub fn setup(
        &self,
        plan: &DerivationPlan,
        timing: &mut Timing,
        initialize_timer: timing::Timer,
    ) -> Result<Vec<upstream::Stored>, Error> {
        util::recreate_dir(&self.paths.artefacts().host).map_err(Error::RecreateArtefactsDir)?;
        root::recreate_frozen(&self.paths, plan)?;
        root::populate_frozen(
            &self.paths,
            &self.moss_dir,
            self.repositories.clone(),
            &plan.build_lock,
            timing,
            initialize_timer,
        )?;
        let timer = timing.begin(timing::Kind::Fetch);
        let stored = upstream::sync_locked(
            &plan.sources,
            &self.paths.upstreams().host,
            &self.paths.guest_host_path(&self.paths.upstreams()),
        )?;
        timing.finish(timer);
        Ok(stored)
    }

    pub fn cleanup(&self, plan: &DerivationPlan) -> Result<(), Error> {
        root::remove_frozen(&self.paths, plan)?;
        for path in [self.paths.artefacts().host, self.paths.build().host] {
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        upstream::remove_locked(&self.paths.upstreams().host, &plan.sources)?;
        moss::Client::builder("boulder", moss::Installation::open(&self.moss_dir, None)?)
            .repositories(self.repositories.clone())
            .build()?
            .prune_cache()?;
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
    #[error("recipe")]
    Recipe(#[from] recipe::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("recreate artefacts dir")]
    RecreateArtefactsDir(#[source] io::Error),
    #[error("moss client")]
    MossClient(#[from] moss::client::Error),
    #[error("moss installation")]
    MossInstallation(#[from] moss::installation::Error),
}
