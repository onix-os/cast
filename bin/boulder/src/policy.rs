// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Explicit repository build policy.
//!
//! Boulder loads one authored Gluon root. Directory contents and filesystem
//! order never participate in policy composition.

use std::path::{Path, PathBuf};

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, SourceRoot};
use stone_recipe::build_policy::{BuildPolicySpec, TargetPolicySpec};
use thiserror::Error;

use crate::Env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicy {
    pub spec: BuildPolicySpec,
    pub fingerprint: EvaluationFingerprint,
    pub origin: String,
}

/// One source module which contributed to the evaluated typed policy.
///
/// The root is listed first. Relative and embedded imports follow in the
/// evaluator's canonical fingerprint order, so explanation output describes
/// exactly the code bound into the policy identity without pretending that an
/// import performed an unrecorded mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySource {
    pub origin: String,
    pub fingerprint: String,
    pub root: bool,
}

impl BuildPolicy {
    const ROOT: &'static str = "default.glu";

    pub fn load(env: &Env) -> Result<Self, Error> {
        Self::load_from(&env.data_dir.join("policy"))
    }

    fn load_from(policy_dir: &Path) -> Result<Self, Error> {
        let source_root = SourceRoot::new(policy_dir).map_err(|source| Error::SourceRoot {
            path: policy_dir.to_path_buf(),
            source: Box::new(source),
        })?;
        let evaluator = Evaluator::default().with_source_root(source_root.clone());
        let root_path = policy_dir.join(Self::ROOT);
        let source = source_root
            .load(Self::ROOT, evaluator.limits().max_source_bytes)
            .map_err(|source| Error::Load {
                path: root_path.clone(),
                source: Box::new(source),
            })?;
        let evaluated =
            stone_recipe::build_policy::evaluate_gluon_with(&evaluator, &source).map_err(|source| Error::Evaluate {
                path: root_path,
                source: Box::new(source),
            })?;

        Ok(Self {
            spec: evaluated.policy,
            fingerprint: evaluated.fingerprint,
            origin: source.logical_name().to_owned(),
        })
    }

    pub fn target(&self, name: &str) -> Result<&TargetPolicySpec, Error> {
        self.spec
            .targets
            .iter()
            .find(|target| target.name == name)
            .ok_or_else(|| Error::UnknownTarget {
                requested: name.to_owned(),
                available: self.spec.targets.iter().map(|target| target.name.clone()).collect(),
            })
    }

    pub fn sources(&self) -> Vec<PolicySource> {
        std::iter::once(PolicySource {
            origin: self.origin.clone(),
            fingerprint: self.fingerprint.root_source_sha256.clone(),
            root: true,
        })
        .chain(self.fingerprint.imported_modules.iter().map(|module| PolicySource {
            origin: module.logical_name.clone(),
            fingerprint: module.sha256.clone(),
            root: false,
        }))
        .collect()
    }

    #[cfg(test)]
    pub(crate) fn repository_for_tests() -> Self {
        static REPOSITORY: std::sync::OnceLock<BuildPolicy> = std::sync::OnceLock::new();
        let policy_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/policy");
        REPOSITORY.get_or_init(|| Self::load_from(&policy_dir).unwrap()).clone()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("prepare build-policy Gluon source root {path:?}")]
    SourceRoot {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("load build-policy root {path:?}")]
    Load {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("evaluate build-policy root {path:?}")]
    Evaluate {
        path: PathBuf,
        #[source]
        source: Box<stone_recipe::build_policy::BuildPolicyEvaluationError>,
    },
    #[error("unknown build-policy target `{requested}`; available targets: {}", available.join(", "))]
    UnknownTarget { requested: String, available: Vec<String> },
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;

    #[test]
    fn loads_the_single_repository_policy_root() {
        let policy = BuildPolicy::repository_for_tests();

        assert_eq!(policy.origin, "default.glu");
        assert_eq!(policy.spec.vendor_id, "aerynos-linux");
        assert_eq!(
            policy.target("x86_64").unwrap().target_triple,
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            policy.fingerprint.imported_modules[0].logical_name,
            "boulder.build_policy.v1"
        );
    }

    #[test]
    fn rejects_unknown_targets_without_fallback() {
        let policy = BuildPolicy::repository_for_tests();
        assert!(matches!(
            policy.target("host-default"),
            Err(Error::UnknownTarget { requested, .. }) if requested == "host-default"
        ));
    }

    #[test]
    fn exposes_exact_root_and_import_provenance() {
        let policy = BuildPolicy::repository_for_tests();
        let sources = policy.sources();

        assert_eq!(sources[0].origin, "default.glu");
        assert_eq!(sources[0].fingerprint, policy.fingerprint.root_source_sha256);
        assert!(sources[0].root);
        assert!(sources[1..].iter().all(|source| !source.root));
        assert!(
            sources
                .iter()
                .any(|source| source.origin == "boulder.build_policy.v1")
        );
    }

    #[test]
    fn ignores_undeclared_neighbor_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("default.glu"),
            include_str!("../data/policy/default.glu"),
        )
        .unwrap();
        fs::write(root.path().join("ignored.glu"), "not valid Gluon").unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.targets.len(), 6);
        assert!(
            policy
                .fingerprint
                .imported_modules
                .iter()
                .all(|module| module.logical_name != "ignored.glu")
        );
    }
}
