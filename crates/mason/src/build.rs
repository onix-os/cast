// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{io, num::NonZeroUsize, path::PathBuf};

use forge::repository;
use stone_recipe::build_policy::TargetPolicySpec;
use stone_recipe::derivation::{BuilderLayout, DerivationPlan, ProfileFragmentProvenance};
use thiserror::Error;
use tui::Styled;

use self::job::Job;
use crate::{BuildPolicy, Env, Paths, Recipe, Timing, policy, profile, recipe, timing, upstream};

pub mod context;
pub mod job;
pub mod pgo;
pub(crate) mod root;
pub mod tuning;

/// Forge repository-cache identity shared by build-lock resolution and frozen
/// root materialization.
///
/// Forge deliberately gives different identities independent metadata and
/// immutable-index namespaces. Both halves of one build must therefore use
/// this exact identity or the runtime cannot authenticate the index generation
/// recorded by the planner.
pub(crate) const BUILD_REPOSITORY_CACHE_IDENTITY: &str = "cast-plan";

pub struct Builder {
    pub target: Target,
    pub recipe: Recipe,
    pub paths: Paths,
    pub profile: profile::Id,
    pub profile_fragments: Vec<ProfileFragmentProvenance>,
    pub ccache: bool,
    pub env: Env,
    repos: repository::Map,
}

pub struct Target {
    pub target_policy: TargetPolicySpec,
    pub build_policy: BuildPolicy,
    pub jobs: Vec<Job>,
}

pub(crate) struct BuilderRequest {
    pub recipe_path: PathBuf,
    pub env: Env,
    pub profile: profile::Id,
    pub compiler_cache: bool,
    pub output_dir: PathBuf,
    pub jobs: NonZeroUsize,
    pub source_date_epoch: Option<i64>,
    pub requested_target: String,
}

/// Host runtime resources retained after semantic planning is complete.
pub struct Runtime {
    pub paths: Paths,
    forge_dir: PathBuf,
    repositories: repository::Map,
}

/// All descriptor-backed state required to execute one verified derivation.
///
/// The frozen-root guard and the pinned external mount sources intentionally
/// share one lifetime. Dropping or consuming this value is the only supported
/// transition from an executable workspace to cleanup.
#[must_use = "a prepared execution must be retained through the frozen build"]
pub struct PreparedExecution {
    derivation_id: String,
    sandbox: crate::container::FrozenSandbox,
    root_guard: forge::FrozenRootGuard,
}

impl PreparedExecution {
    pub(crate) fn sandbox(&self) -> &crate::container::FrozenSandbox {
        &self.sandbox
    }

    pub(crate) fn root_guard(&self) -> &forge::FrozenRootGuard {
        &self.root_guard
    }

    /// Borrow the exact artefact directory mounted for this execution after
    /// revalidating every retained external mount witness.
    pub(crate) fn artefacts(&self) -> Result<&std::fs::File, Error> {
        Ok(self.sandbox.revalidated_artefacts()?)
    }

    pub(crate) fn require_for(&self, paths: &Paths, plan: &DerivationPlan) -> Result<(), Error> {
        let expected_derivation = plan.derivation_id().to_string();
        if self.derivation_id != expected_derivation {
            return Err(Error::PreparedDerivationMismatch {
                expected: expected_derivation,
                found: self.derivation_id.clone(),
            });
        }
        if self.root_guard.root_path() != paths.rootfs().host {
            return Err(Error::PreparedRootMismatch {
                expected: paths.rootfs().host,
                found: self.root_guard.root_path().to_owned(),
            });
        }
        Ok(())
    }
}

impl Builder {
    pub(crate) fn new(request: BuilderRequest) -> Result<Self, Error> {
        let BuilderRequest {
            recipe_path,
            env,
            profile,
            compiler_cache,
            output_dir,
            jobs,
            source_date_epoch,
            requested_target,
        } = request;
        let recipe = match source_date_epoch {
            Some(epoch) => {
                let build_time =
                    chrono::DateTime::from_timestamp(epoch, 0).ok_or(Error::InvalidSourceDateEpoch(epoch))?;
                Recipe::load_at(&recipe_path, build_time)?
            }
            None => Recipe::load(&recipe_path)?,
        };

        let build_policy = BuildPolicy::load(&env)?;

        let layout =
            BuilderLayout::from_policy(&build_policy.spec.sandbox, &build_policy.spec.build_root.compiler_cache);
        let paths = Paths::new(&recipe, layout, &env.cache_dir, output_dir)?;

        // Keep the exact validated policy member borrowed while lowering jobs.
        // BuildContext intentionally rejects an equal-but-cloned target because
        // pointer membership is what proves it came from this policy value.
        let target_policy = build_policy.target(&requested_target)?;
        if !recipe.supports_target(target_policy) {
            let supported = build_policy
                .spec
                .targets
                .iter()
                .filter(|target| recipe.supports_target(target))
                .map(|target| target.name.clone())
                .collect();
            return Err(Error::UnsupportedRecipeTarget {
                requested: requested_target,
                supported,
            });
        }
        let stages = pgo::stages(&recipe, target_policy)
            .map(|stages| stages.into_iter().map(Some).collect::<Vec<_>>())
            .unwrap_or_else(|| vec![None]);
        let target_jobs = stages
            .into_iter()
            .map(|stage| {
                Job::new(
                    target_policy,
                    stage,
                    &recipe,
                    &paths,
                    &build_policy,
                    compiler_cache,
                    jobs,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let target = Target {
            target_policy: target_policy.clone(),
            build_policy,
            jobs: target_jobs,
        };

        let profiles = profile::Manager::new(&env)?;
        let repos = profiles
            .repositories_for_architecture(&profile, &target.target_policy.build_platform.architecture)?
            .clone();
        let profile_fragments = profiles.fragments.clone();

        Ok(Self {
            target,
            recipe,
            paths,
            profile,
            profile_fragments,
            ccache: compiler_cache,
            env,
            repos,
        })
    }

    pub(crate) fn repositories(&self) -> &repository::Map {
        &self.repos
    }

    pub fn into_runtime(mut self, plan: &DerivationPlan) -> Result<Runtime, Error> {
        self.paths.bind_to_plan(plan)?;
        Ok(Runtime {
            paths: self.paths,
            forge_dir: self.env.forge_dir,
            repositories: self.repos,
        })
    }
}

impl Runtime {
    pub fn acquire_execution_lock(&self, plan: &DerivationPlan) -> Result<crate::paths::ExecutionLock, Error> {
        self.paths.require_plan(plan)?;
        Ok(self.paths.acquire_execution_lock(plan)?)
    }

    pub fn setup(
        &self,
        plan: &DerivationPlan,
        execution_lock: &crate::paths::ExecutionLock,
        timing: &mut Timing,
        initialize_timer: timing::Timer,
    ) -> Result<PreparedExecution, Error> {
        self.paths.require_execution_lock(execution_lock, plan)?;
        // Scratch roots are atomically detached, boundedly discarded, created
        // exact-private, and pinned before any frozen-root work begins. Caches
        // are authenticated and retained by this same preparation boundary.
        let sandbox = crate::container::prepare_frozen_sandbox(&self.paths, plan)?;
        let pending_root = root::populate_frozen(
            &self.paths,
            &self.forge_dir,
            self.repositories.clone(),
            plan,
            timing,
            initialize_timer,
        )?;
        let timer = timing.begin(timing::Kind::Fetch);
        let stored = upstream::sync_locked_into_root(
            &plan.sources,
            &self.paths.upstreams().host,
            pending_root.materialized_root(),
            std::path::Path::new(&plan.layout.source_dir),
            plan.source_date_epoch,
        )?;
        timing.finish(timer);
        if stored.len() != plan.sources.len() {
            return Err(Error::PreparedSourceCountMismatch {
                expected: plan.sources.len(),
                found: stored.len(),
            });
        }
        drop(stored);

        // Every root-visible mutation happens before verification. External
        // writable sources are also opened and retained before the final
        // root proof is issued, so activation performs no path-based setup.
        crate::container::prepare_frozen_mount_targets(&self.paths, plan, pending_root.materialized_root())?;
        let root_guard = pending_root.verify()?;
        let prepared = PreparedExecution {
            derivation_id: plan.derivation_id().to_string(),
            sandbox,
            root_guard,
        };
        prepared.require_for(&self.paths, plan)?;
        Ok(prepared)
    }

    pub fn cleanup(
        &self,
        plan: &DerivationPlan,
        execution_lock: &crate::paths::ExecutionLock,
        prepared: PreparedExecution,
    ) -> Result<(), Error> {
        self.paths.require_execution_lock(execution_lock, plan)?;
        prepared.require_for(&self.paths, plan)?;
        // Cleanup cannot race a live activation proof because it consumes and
        // drops the proof before asking Forge to detach the root.
        drop(prepared);
        root::discard_frozen(&self.paths, &self.forge_dir, self.repositories.clone(), plan)?;
        for path in [self.paths.artefacts().host, self.paths.build().host] {
            self.paths.remove_private_host_directory(&path)?;
        }
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
    #[error("container")]
    Container(#[from] crate::container::Error),
    #[error("prepared execution belongs to derivation {found}, expected {expected}")]
    PreparedDerivationMismatch { expected: String, found: String },
    #[error("prepared execution anchors root {found:?}, expected {expected:?}")]
    PreparedRootMismatch { expected: PathBuf, found: PathBuf },
    #[error("prepared execution contains {found} locked sources, expected {expected}")]
    PreparedSourceCountMismatch { expected: usize, found: usize },
    #[error(transparent)]
    ForgeClient(#[from] forge::client::Error),
    #[error(transparent)]
    ForgeInstallation(#[from] forge::installation::Error),
}
