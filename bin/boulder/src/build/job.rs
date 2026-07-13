// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeMap,
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use moss::util;
use stone_recipe::{UpstreamSpec, build_policy::ContextValue, derivation::PhasePlan, script};
use thiserror::Error;

pub use self::phase::Phase;
use crate::build::pgo;
use crate::{BuildPolicy, Macros, Paths, Recipe, architecture::BuildTarget};

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
        target: BuildTarget,
        pgo_stage: Option<pgo::Stage>,
        recipe: &Recipe,
        paths: &Paths,
        macros: &Macros,
        policy: &BuildPolicy,
        ccache: bool,
        jobs: NonZeroUsize,
    ) -> Result<Self, Error> {
        let build_dir = paths.build().guest.join(target.to_string());
        let work_dir = work_dir(&build_dir, &recipe.declaration.sources);

        let phases = phase::list(pgo_stage)
            .into_iter()
            .filter_map(|phase| {
                let result = phase
                    .plan(target, pgo_stage, recipe, paths, macros, policy, ccache, jobs)
                    .transpose()?;
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

fn work_dir(build_dir: &Path, sources: &[UpstreamSpec]) -> PathBuf {
    let mut work_dir = build_dir.to_path_buf();

    // Work dir is the first upstream that should be unpacked
    if let Some(source) = sources.iter().find(|source| match source {
        UpstreamSpec::Archive { unpack, .. } => *unpack,
        UpstreamSpec::Git { .. } => true,
    }) {
        match source {
            UpstreamSpec::Archive {
                url,
                rename,
                unpack_dir,
                ..
            } => {
                let file_name = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let rename = rename.as_deref().unwrap_or(file_name.as_str());
                let unpack_dir = unpack_dir.as_ref().cloned().unwrap_or_else(|| rename.to_owned());

                work_dir = build_dir.join(unpack_dir);
            }
            UpstreamSpec::Git { url, clone_dir, .. } => {
                let source = url
                    .parse()
                    .ok()
                    .map(|url| util::uri_file_name(&url).to_owned())
                    .unwrap_or_default();
                let target = clone_dir.as_ref().cloned().unwrap_or_else(|| source.to_owned());

                work_dir = build_dir.join(target);
            }
        }
    }

    work_dir
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing arch macros: {0}")]
    MissingArchMacros(String),
    #[error("script")]
    Script(#[from] script::Error),
    #[error("typed tuning policy")]
    Tuning(#[from] crate::build::tuning::Error),
    #[error("build policy")]
    BuildPolicy(#[from] crate::policy::Error),
    #[error("unsupported context {0:?} in a compiler tuning flag")]
    UnsupportedTuningContext(ContextValue),
    #[error("an environment phase may only contain Shell or CargoEnvironment steps")]
    UnsupportedEnvironmentStep,
    #[error("Shell and CargoEnvironment steps cannot be rendered as structural executable commands")]
    UnsupportedExecutableStep,
    #[error("io")]
    Io(#[from] io::Error),
}
