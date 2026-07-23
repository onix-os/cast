// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeMap,
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use stone_recipe::{
    UpstreamSpec, build_policy::TargetPolicySpec, derivation::PhasePlan, spec::UpstreamValidationError,
};
use thiserror::Error;

pub use self::phase::Phase;
use crate::build::pgo;
use crate::{BuildPolicy, Paths, Recipe};

mod phase;

#[derive(Debug)]
pub struct Job {
    pub pgo_stage: Option<pgo::Stage>,
    pub phases: BTreeMap<Phase, PhasePlan>,
    pub work_dir: PathBuf,
    pub build_dir: PathBuf,
}

impl Job {
    pub fn new(
        target: &TargetPolicySpec,
        pgo_stage: Option<pgo::Stage>,
        recipe: &Recipe,
        paths: &Paths,
        policy: &BuildPolicy,
        ccache: bool,
        jobs: NonZeroUsize,
    ) -> Result<Self, Error> {
        let build_dir = paths.build().guest.join(&target.name);
        let work_dir = work_dir(&build_dir, &recipe.declaration.sources)?;

        let plan_context = phase::PlanContext {
            target,
            pgo_stage,
            recipe,
            paths,
            policy,
            compiler_cache: ccache,
            jobs,
        };

        let phases = phase::list(pgo_stage)
            .into_iter()
            .filter_map(|phase| {
                let result = phase.plan(&plan_context).transpose()?;
                Some(result.map(|plan| (phase, plan)))
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            pgo_stage,
            phases,
            work_dir,
            build_dir,
        })
    }
}

fn work_dir(build_dir: &Path, sources: &[UpstreamSpec]) -> Result<PathBuf, Error> {
    let mut work_dir = build_dir.to_path_buf();

    // Work dir is the first upstream that should be unpacked
    if let Some((index, source)) = sources.iter().enumerate().find(|(_, source)| match source {
        UpstreamSpec::Archive { unpack, .. } => *unpack,
        UpstreamSpec::Git { .. } => true,
    }) {
        let materialization_name = source
            .materialization_name()
            .map_err(|source| Error::InvalidSource { index, source })?;
        match source {
            UpstreamSpec::Archive { unpack_dir, .. } => {
                work_dir = build_dir.join(unpack_dir.as_deref().unwrap_or(&materialization_name));
            }
            UpstreamSpec::Git { .. } => {
                work_dir = build_dir.join(materialization_name);
            }
        }
    }

    Ok(work_dir)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("typed tuning policy")]
    Tuning(#[from] crate::build::tuning::Error),
    #[error("build context")]
    Context(#[from] crate::build::context::ContextError),
    #[error("build policy")]
    BuildPolicy(#[from] crate::policy::Error),
    #[error("package executable has an invalid typed requirement")]
    InvalidProgramRequirement {
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("source {index} is invalid: {source}")]
    InvalidSource {
        index: usize,
        #[source]
        source: UpstreamValidationError,
    },
    #[error("PGO path {path:?} must be normalized and remain beneath {pgo_dir:?}")]
    UnsafePgoPath { path: String, pgo_dir: String },
    #[error("io")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuildPolicy;
    use stone_recipe::derivation::BuilderLayout;

    #[test]
    fn job_directories_follow_non_default_sandbox_policy() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let mut policy = BuildPolicy::repository_for_tests();
        policy.spec.sandbox.hostname = "forge-builder".to_owned();
        policy.spec.sandbox.guest_root = "/forge".to_owned();
        policy.spec.sandbox.artifacts_dir = "/forge/output".to_owned();
        policy.spec.sandbox.build_dir = "/forge/work".to_owned();
        policy.spec.sandbox.source_dir = "/forge/sources".to_owned();
        policy.spec.sandbox.recipe_dir = "/forge/recipe".to_owned();
        policy.spec.sandbox.package_dir = "/forge/recipe/package".to_owned();
        policy.spec.sandbox.install_dir = "/forge/destination".to_owned();
        {
            let cache = &mut policy.spec.build_root.compiler_cache;
            cache.ccache_dir = "/forge/cache-cc".to_owned();
            cache.sccache_dir = "/forge/cache-rust".to_owned();
            cache.go_cache_dir = "/forge/cache-go".to_owned();
            cache.go_mod_cache_dir = "/forge/cache-go-mod".to_owned();
            cache.cargo_cache_dir = "/forge/cache-cargo".to_owned();
            cache.zig_cache_dir = "/forge/cache-zig".to_owned();
        }
        policy.spec.validate().unwrap();
        let layout = BuilderLayout::from_policy(&policy.spec.sandbox, &policy.spec.build_root.compiler_cache);
        let runtime = crate::private_tempdir();
        let paths = Paths::new(&recipe, layout, runtime.path(), runtime.path()).unwrap();
        let target = policy.target("x86_64").unwrap();

        let job = Job::new(
            target,
            None,
            &recipe,
            &paths,
            &policy,
            false,
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();

        assert_eq!(job.build_dir, Path::new("/forge/work/x86_64"));
        assert_eq!(job.work_dir, Path::new("/forge/work/x86_64"));
    }
}
