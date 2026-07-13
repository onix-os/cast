// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use fs_err as fs;
use moss::{repository, util};
use stone_recipe::build_policy::TargetPolicySpec;
use stone_recipe::derivation::DerivationPlan;
use thiserror::Error;
use tui::Styled;

use self::job::Job;
use crate::{BuildPolicy, Env, Paths, Recipe, Timing, policy, profile, recipe, timing, upstream};

pub mod context;
pub mod job;
pub mod pgo;
pub(crate) mod root;
pub mod tuning;

pub struct Builder {
    pub target: Target,
    pub recipe: Recipe,
    pub paths: Paths,
    pub profile: profile::Id,
    pub profile_fingerprints: Vec<gluon_config::EvaluationFingerprint>,
    pub ccache: bool,
    pub env: Env,
    repos: repository::Map,
}

pub struct Target {
    pub target_policy: TargetPolicySpec,
    pub build_policy: BuildPolicy,
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
        env: Env,
        profile: profile::Id,
        ccache: bool,
        output_dir: impl Into<PathBuf>,
        jobs: NonZeroUsize,
        source_date_epoch: Option<i64>,
        requested_target: &str,
    ) -> Result<Self, Error> {
        let recipe = match source_date_epoch {
            Some(epoch) => {
                let build_time =
                    chrono::DateTime::from_timestamp(epoch, 0).ok_or(Error::InvalidSourceDateEpoch(epoch))?;
                Recipe::load_at(recipe_path, build_time)?
            }
            None => Recipe::load(recipe_path)?,
        };

        let build_policy = BuildPolicy::load(&env)?;

        let paths = Paths::new(&recipe, &env.cache_dir, "/mason", output_dir)?;

        let target_policy = build_policy.target(requested_target)?.clone();
        if !recipe.supports_target(&target_policy) {
            let supported = build_policy
                .spec
                .targets
                .iter()
                .filter(|target| recipe.supports_target(target))
                .map(|target| target.name.clone())
                .collect();
            return Err(Error::UnsupportedRecipeTarget {
                requested: requested_target.to_owned(),
                supported,
            });
        }
        let stages = pgo::stages(&recipe, &target_policy)
            .map(|stages| stages.into_iter().map(Some).collect::<Vec<_>>())
            .unwrap_or_else(|| vec![None]);
        let target_jobs = stages
            .into_iter()
            .map(|stage| Job::new(&target_policy, stage, &recipe, &paths, &build_policy, ccache, jobs))
            .collect::<Result<Vec<_>, _>>()?;
        let target = Target {
            target_policy,
            build_policy,
            jobs: target_jobs,
        };

        let profiles = profile::Manager::new(&env)?;
        let repos = profiles
            .repositories_for_architecture(&profile, &target.target_policy.build_platform.architecture)?
            .clone();
        let profile_fingerprints = profiles.fingerprints.clone();

        Ok(Self {
            target,
            recipe,
            paths,
            profile,
            profile_fingerprints,
            ccache,
            env,
            repos,
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

pub fn build_target_prefix(target: &str, i: usize) -> String {
    let newline = if i > 0 { "\n".into() } else { String::default() };

    format!("{newline}{}", target.dim())
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

#[derive(Debug, Error)]
pub enum Error {
    #[error("recipe does not support build target `{requested}`; supported targets: {}", supported.join(", "))]
    UnsupportedRecipeTarget { requested: String, supported: Vec<String> },
    #[error("invalid SOURCE_DATE_EPOCH {0}")]
    InvalidSourceDateEpoch(i64),
    #[error("build policy")]
    BuildPolicy(#[from] policy::Error),
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
