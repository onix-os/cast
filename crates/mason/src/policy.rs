//! Explicit, ordered repository build policy.
//!
//! Cast evaluates one authored Gluon manifest and applies exactly the
//! modules named by that manifest. Directory contents and filesystem order
//! never participate in composition.

use std::path::{Path, PathBuf};

use gluon_config::{Diagnostic, GluonEngine, SourceRoot};
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildPolicyEvaluationError, BuildPolicySpec, TargetPolicySpec,
    layers::{BuildPolicyOperation, BuildPolicyRootEvaluationError},
};
use stone_recipe::derivation::{
    PolicyLayerProvenance, PolicyProvenance, PolicyTransitionProvenance, policy_composition_identity,
};
use thiserror::Error;

use crate::Env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicy {
    pub spec: BuildPolicySpec,
    pub provenance: PolicyProvenance,
}

impl BuildPolicy {
    const ROOT: &'static str = "policy.glu";

    pub fn load(env: &Env) -> Result<Self, Error> {
        Self::load_from(&env.data_dir.join("policy"))
    }

    fn load_from(policy_dir: &Path) -> Result<Self, Error> {
        let source_root = SourceRoot::new(policy_dir).map_err(|source| Error::SourceRoot {
            path: policy_dir.to_path_buf(),
            source: Box::new(source),
        })?;
        let evaluator = GluonEngine::default().with_source_root(source_root.clone());
        let root_path = policy_dir.join(Self::ROOT);
        let root_source = source_root
            .load(Self::ROOT, evaluator.limits().max_source_bytes)
            .map_err(|source| Error::LoadRoot {
                path: root_path.clone(),
                source: Box::new(source),
            })?;
        let evaluated_root = stone_recipe::build_policy::layers::evaluate_gluon_with(&evaluator, &root_source)
            .map_err(|source| Error::EvaluateRoot {
                path: root_path.clone(),
                source: Box::new(source),
            })?;

        let manifest = evaluated_root.root;
        let mut layers = Vec::with_capacity(manifest.layers.len());
        let mut state = None;
        let mut operation_order = 0;

        for (layer_index, layer) in manifest.layers.iter().enumerate() {
            let mut transitions = Vec::with_capacity(layer.entries.len());
            for (entry_index, entry) in layer.entries.iter().enumerate() {
                transitions.push(apply_entry(
                    &source_root,
                    &evaluator,
                    &manifest.name,
                    &layer.name,
                    layer_index,
                    entry_index,
                    operation_order,
                    entry.operation,
                    &entry.origin,
                    &mut state,
                )?);
                operation_order += 1;
            }
            layers.push(PolicyLayerProvenance {
                name: layer.name.clone(),
                transitions,
            });
        }

        let spec = state.ok_or_else(|| Error::MissingPolicy {
            policy: manifest.name.clone(),
        })?;
        let identity_inputs = policy_composition_identity(&manifest.name, &layers);
        let finalized_root =
            stone_recipe::build_policy::layers::evaluate_gluon_with_inputs(&evaluator, &root_source, &identity_inputs)
                .map_err(|source| Error::FinalizeRoot {
                    path: root_path,
                    source: Box::new(source),
                })?;
        if finalized_root.root != manifest {
            return Err(Error::ManifestChanged { policy: manifest.name });
        }

        Ok(Self {
            spec,
            provenance: PolicyProvenance {
                name: manifest.name,
                root: finalized_root.fingerprint,
                layers,
            },
        })
    }

    pub fn target(&self, name: &str) -> Result<&TargetPolicySpec, Error> {
        if let Some(target) = self.spec.targets.iter().find(|target| target.name == name) {
            return Ok(target);
        }

        if let Some(target) = self.spec.retired_targets.iter().find(|target| target.name == name) {
            return Err(Error::RetiredTarget {
                requested: name.to_owned(),
                reason: target.reason.clone(),
            });
        }

        Err(Error::UnknownTarget {
            requested: name.to_owned(),
            available: self.spec.targets.iter().map(|target| target.name.clone()).collect(),
        })
    }

    #[cfg(test)]
    pub(crate) fn repository_for_tests() -> Self {
        static REPOSITORY: std::sync::OnceLock<BuildPolicy> = std::sync::OnceLock::new();
        let policy_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/policy");
        REPOSITORY.get_or_init(|| Self::load_from(&policy_dir).unwrap()).clone()
    }
}

fn apply_entry(
    source_root: &SourceRoot,
    evaluator: &GluonEngine,
    policy: &str,
    layer: &str,
    layer_index: usize,
    entry_index: usize,
    order: usize,
    operation: BuildPolicyOperation,
    origin: &str,
    state: &mut Option<BuildPolicySpec>,
) -> Result<PolicyTransitionProvenance, Error> {
    match operation {
        BuildPolicyOperation::Add if state.is_some() => {
            return Err(Error::InvalidTransition {
                policy: policy.to_owned(),
                layer: layer.to_owned(),
                layer_index,
                entry_index,
                order,
                operation,
                origin: origin.to_owned(),
                reason: "add requires an absent policy",
            });
        }
        BuildPolicyOperation::Replace | BuildPolicyOperation::Modify if state.is_none() => {
            return Err(Error::InvalidTransition {
                policy: policy.to_owned(),
                layer: layer.to_owned(),
                layer_index,
                entry_index,
                order,
                operation,
                origin: origin.to_owned(),
                reason: "replace and modify require an existing policy",
            });
        }
        BuildPolicyOperation::Add | BuildPolicyOperation::Replace | BuildPolicyOperation::Modify => {}
    }

    let path = source_root.path().join(origin);
    let source = source_root
        .load(origin, evaluator.limits().max_source_bytes)
        .map_err(|source| Error::LoadEntry {
            policy: policy.to_owned(),
            layer: layer.to_owned(),
            layer_index,
            entry_index,
            order,
            operation,
            origin: origin.to_owned(),
            path,
            source: Box::new(source),
        })?;

    let fingerprint = match operation {
        BuildPolicyOperation::Add | BuildPolicyOperation::Replace => {
            let evaluated = stone_recipe::build_policy::evaluate_gluon_with(evaluator, &source).map_err(|source| {
                Error::EvaluateEntry {
                    policy: policy.to_owned(),
                    layer: layer.to_owned(),
                    layer_index,
                    entry_index,
                    order,
                    operation,
                    origin: origin.to_owned(),
                    source: Box::new(source),
                }
            })?;
            *state = Some(evaluated.policy);
            evaluated.fingerprint
        }
        BuildPolicyOperation::Modify => {
            let evaluated =
                stone_recipe::build_policy::evaluate_patch_gluon_with(evaluator, &source).map_err(|source| {
                    Error::EvaluateEntry {
                        policy: policy.to_owned(),
                        layer: layer.to_owned(),
                        layer_index,
                        entry_index,
                        order,
                        operation,
                        origin: origin.to_owned(),
                        source: Box::new(source),
                    }
                })?;
            let current = state.take().expect("modify precondition checked");
            let next = evaluated
                .patch
                .apply_validated(current)
                .map_err(|source| Error::ApplyPatch {
                    policy: policy.to_owned(),
                    layer: layer.to_owned(),
                    layer_index,
                    entry_index,
                    order,
                    operation,
                    origin: origin.to_owned(),
                    source: Box::new(source),
                })?;
            *state = Some(next);
            evaluated.fingerprint
        }
    };

    Ok(PolicyTransitionProvenance {
        operation,
        origin: origin.to_owned(),
        evaluation: fingerprint,
    })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("prepare build-policy Gluon source root {path:?}")]
    SourceRoot {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("load build-policy manifest {path:?}")]
    LoadRoot {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("evaluate build-policy manifest {path:?}")]
    EvaluateRoot {
        path: PathBuf,
        #[source]
        source: Box<BuildPolicyRootEvaluationError>,
    },
    #[error("finalize build-policy manifest identity {path:?}")]
    FinalizeRoot {
        path: PathBuf,
        #[source]
        source: Box<BuildPolicyRootEvaluationError>,
    },
    #[error(
        "policy `{policy}` operation {order}, layer {layer_index} `{layer}` entry {entry_index} ({operation:?}) from `{origin}` is invalid: {reason}"
    )]
    InvalidTransition {
        policy: String,
        layer: String,
        layer_index: usize,
        entry_index: usize,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        reason: &'static str,
    },
    #[error(
        "policy `{policy}` operation {order}, layer {layer_index} `{layer}` entry {entry_index} ({operation:?}) cannot load `{origin}` at {path:?}"
    )]
    LoadEntry {
        policy: String,
        layer: String,
        layer_index: usize,
        entry_index: usize,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error(
        "policy `{policy}` operation {order}, layer {layer_index} `{layer}` entry {entry_index} ({operation:?}) cannot evaluate `{origin}`"
    )]
    EvaluateEntry {
        policy: String,
        layer: String,
        layer_index: usize,
        entry_index: usize,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        #[source]
        source: Box<BuildPolicyEvaluationError>,
    },
    #[error(
        "policy `{policy}` operation {order}, layer {layer_index} `{layer}` entry {entry_index} ({operation:?}) cannot apply `{origin}`"
    )]
    ApplyPatch {
        policy: String,
        layer: String,
        layer_index: usize,
        entry_index: usize,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        #[source]
        source: Box<BuildPolicyConversionError>,
    },
    #[error("policy `{policy}` has no complete value after its configured layers")]
    MissingPolicy { policy: String },
    #[error("policy manifest `{policy}` changed while finalizing its composed identity")]
    ManifestChanged { policy: String },
    #[error("build-policy target `{requested}` is retired: {reason}")]
    RetiredTarget { requested: String, reason: String },
    #[error("unknown build-policy target `{requested}`; available targets: {}", available.join(", "))]
    UnknownTarget { requested: String, available: Vec<String> },
}

#[cfg(test)]
mod tests {
    use fs_err as fs;
    use sha2::{Digest, Sha256};

    use super::*;

    const REPOSITORY_MANIFEST: &str = include_str!("../data/policy/policy.glu");
    const REPOSITORY_DEFAULT: &str = include_str!("../data/policy/default.glu");
    const REPOSITORY_TUNING_FLAGS: &str = include_str!("../data/policy/tuning/flags.glu");
    const REPOSITORY_TUNING_GROUPS: &str = include_str!("../data/policy/tuning/groups.glu");

    fn fixture(manifest: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("tuning")).unwrap();
        fs::write(root.path().join("policy.glu"), manifest).unwrap();
        fs::write(root.path().join("default.glu"), REPOSITORY_DEFAULT).unwrap();
        fs::write(root.path().join("tuning/flags.glu"), REPOSITORY_TUNING_FLAGS).unwrap();
        fs::write(root.path().join("tuning/groups.glu"), REPOSITORY_TUNING_GROUPS).unwrap();
        root
    }

    fn composition_digest(provenance: &PolicyProvenance) -> String {
        format!(
            "{:x}",
            Sha256::digest(policy_composition_identity(&provenance.name, &provenance.layers))
        )
    }

    #[test]
    fn loads_the_explicit_repository_policy_layers() {
        let policy = BuildPolicy::repository_for_tests();

        assert_eq!(policy.provenance.name, "aerynos");
        assert_eq!(policy.provenance.root.root_logical_name, "policy.glu");
        assert_eq!(policy.spec.build_subdir, "aerynos-builddir");
        assert_eq!(
            policy.target("x86_64").unwrap().target_triple,
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(policy.provenance.layers.len(), 1);
        assert_eq!(policy.provenance.layers[0].name, "foundation");
        assert_eq!(policy.provenance.layers[0].transitions.len(), 1);
        let transition = &policy.provenance.layers[0].transitions[0];
        assert_eq!(transition.operation, BuildPolicyOperation::Add);
        assert_eq!(transition.origin, "default.glu");
        assert_eq!(transition.evaluation.root_logical_name, "default.glu");
        transition.evaluation.validate().unwrap();
        policy.provenance.root.validate().unwrap();
        assert_ne!(
            policy.provenance.root.explicit_inputs_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            policy.provenance.root.explicit_inputs_sha256,
            composition_digest(&policy.provenance)
        );
    }

    #[test]
    fn rejects_unknown_targets_without_fallback() {
        let policy = BuildPolicy::repository_for_tests();
        assert!(matches!(
            policy.target("host-default"),
            Err(Error::UnknownTarget {
                requested,
                available,
            }) if requested == "host-default"
                && available.contains(&"x86_64".to_owned())
                && !available.contains(&"x86_64-stage1".to_owned())
        ));
    }

    #[test]
    fn reports_repository_retired_targets_with_the_authored_reason() {
        let policy = BuildPolicy::repository_for_tests();
        assert!(matches!(
            policy.target("x86_64-stage1"),
            Err(Error::RetiredTarget { requested, reason })
                if requested == "x86_64-stage1"
                    && reason == "legacy bootstrap target was unreachable and its bootstrap_root had no consumer"
        ));
    }

    #[test]
    fn retains_complete_manifest_and_transition_evaluation_provenance() {
        let policy = BuildPolicy::repository_for_tests();
        assert!(
            policy
                .provenance
                .root
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "cast.build_policy.layers.v1")
        );
        let transition = &policy.provenance.layers[0].transitions[0];
        assert_eq!(transition.evaluation.root_logical_name, "default.glu");
        assert_eq!(transition.evaluation.root_source_sha256.len(), 64);
        assert_eq!(transition.evaluation.explicit_inputs_sha256.len(), 64);
        assert_eq!(transition.evaluation.sha256.len(), 64);
        assert_eq!(transition.evaluation.gluon_version, gluon_config::GLUON_VERSION);
        assert_eq!(
            transition.evaluation.configuration_abi_version,
            gluon_config::CONFIGURATION_ABI_VERSION
        );
        assert_eq!(
            transition.evaluation.evaluator_policy_version,
            gluon_config::EVALUATOR_POLICY_VERSION
        );
        assert!(
            transition
                .evaluation
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "cast.build_policy.v5")
        );
        for expected in ["tuning/flags.glu", "tuning/groups.glu"] {
            assert!(
                transition
                    .evaluation
                    .imported_modules
                    .iter()
                    .any(|module| module.logical_name == expected),
                "missing repository policy module {expected} from evaluation provenance"
            );
        }
    }

    #[test]
    fn preserves_named_empty_layers_in_manifest_order_and_v2_identity() {
        let root = fixture(
            r#"
let l = import! cast.build_policy.layers.v1
l.policy "empty-layer-policy" [
    l.layer "foundation" [l.add "default.glu"],
    l.layer "reserved-site-layer" [],
]
"#,
        );

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(
            policy
                .provenance
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            ["foundation", "reserved-site-layer"]
        );
        assert!(policy.provenance.layers[1].transitions.is_empty());
        assert_eq!(
            policy.provenance.root.explicit_inputs_sha256,
            composition_digest(&policy.provenance)
        );
    }

    #[test]
    fn applies_add_modify_and_replace_in_authored_order() {
        let root = fixture(
            r#"
let l = import! cast.build_policy.layers.v1
l.policy "test-policy" [
    l.layer "foundation" [l.add "default.glu"],
    l.layer "site" [
        l.modify "modify.glu",
        l.replace "replacement.glu",
    ],
]
"#,
        );
        fs::write(
            root.path().join("modify.glu"),
            r#"
let b = import! cast.build_policy.v5
b.policy_patch {
    build_subdir = b.patch.set "modified-builddir",
    .. b.defaults.policy_patch
}
"#,
        )
        .unwrap();
        fs::write(
            root.path().join("replacement.glu"),
            REPOSITORY_DEFAULT.replace("aerynos-builddir", "final-builddir"),
        )
        .unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.build_subdir, "final-builddir");
        assert_eq!(
            policy
                .provenance
                .layers
                .iter()
                .flat_map(|layer| layer.transitions.iter())
                .map(|transition| transition.operation)
                .collect::<Vec<_>>(),
            [
                BuildPolicyOperation::Add,
                BuildPolicyOperation::Modify,
                BuildPolicyOperation::Replace
            ]
        );
        assert_eq!(policy.provenance.layers[1].name, "site");
        assert_eq!(policy.provenance.layers[1].transitions[0].origin, "modify.glu");
    }

    #[test]
    fn rejects_each_invalid_state_transition_with_context() {
        let cases = [
            (
                "l.add \"default.glu\", l.add \"default.glu\"",
                BuildPolicyOperation::Add,
                1,
            ),
            ("l.replace \"default.glu\"", BuildPolicyOperation::Replace, 0),
            ("l.modify \"modify.glu\"", BuildPolicyOperation::Modify, 0),
        ];
        for (entries, expected_operation, expected_order) in cases {
            let root = fixture(&format!(
                r#"
let l = import! cast.build_policy.layers.v1
l.policy "strict-policy" [l.layer "strict-layer" [{entries}]]
"#
            ));
            fs::write(root.path().join("modify.glu"), "not reached").unwrap();

            let error = BuildPolicy::load_from(root.path()).unwrap_err();
            assert!(matches!(
                error,
                Error::InvalidTransition {
                    policy,
                    layer,
                    layer_index,
                    entry_index,
                    order,
                    operation,
                    origin: _,
                    reason: _,
                } if policy == "strict-policy"
                    && layer == "strict-layer"
                    && layer_index == 0
                    && entry_index == expected_order
                    && order == expected_order
                    && operation == expected_operation
            ));
        }
    }

    #[test]
    fn rejects_invalid_intermediate_patch_with_operation_context() {
        let root = fixture(
            r#"
let l = import! cast.build_policy.layers.v1
l.policy "validated-policy" [l.layer "site" [
    l.add "default.glu",
    l.modify "invalid.glu",
]]
"#,
        );
        fs::write(
            root.path().join("invalid.glu"),
            r#"
let b = import! cast.build_policy.v5
b.policy_patch {
    build_subdir = b.patch.set "",
    .. b.defaults.policy_patch
}
"#,
        )
        .unwrap();

        let error = BuildPolicy::load_from(root.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::ApplyPatch {
                policy,
                layer,
                layer_index: 0,
                entry_index: 1,
                order: 1,
                operation: BuildPolicyOperation::Modify,
                origin,
                ..
            } if policy == "validated-policy" && layer == "site" && origin == "invalid.glu"
        ));
    }

    #[test]
    fn composed_identity_binds_manifest_order_and_complete_module_fingerprints() {
        let first_root = fixture(REPOSITORY_MANIFEST);
        let repeated_root = fixture(REPOSITORY_MANIFEST);
        let changed_root = fixture(REPOSITORY_MANIFEST);
        fs::write(
            changed_root.path().join("default.glu"),
            format!("{REPOSITORY_DEFAULT}\n// identity-only source change\n"),
        )
        .unwrap();

        let first = BuildPolicy::load_from(first_root.path()).unwrap();
        let repeated = BuildPolicy::load_from(repeated_root.path()).unwrap();
        let changed = BuildPolicy::load_from(changed_root.path()).unwrap();

        assert_eq!(first.spec, repeated.spec);
        assert_eq!(first.provenance.root.sha256, repeated.provenance.root.sha256);
        assert_eq!(first.spec, changed.spec);
        assert_ne!(
            first.provenance.layers[0].transitions[0].evaluation.sha256,
            changed.provenance.layers[0].transitions[0].evaluation.sha256
        );
        assert_ne!(first.provenance.root.sha256, changed.provenance.root.sha256);
        assert_eq!(first.provenance.root.root_logical_name, "policy.glu");
        assert_eq!(
            first.provenance.root.explicit_inputs_sha256,
            composition_digest(&first.provenance)
        );
    }

    #[test]
    fn tuning_module_bytes_participate_in_transition_and_composed_identity() {
        for (logical_name, source) in [
            ("tuning/flags.glu", REPOSITORY_TUNING_FLAGS),
            ("tuning/groups.glu", REPOSITORY_TUNING_GROUPS),
        ] {
            let baseline_root = fixture(REPOSITORY_MANIFEST);
            let changed_root = fixture(REPOSITORY_MANIFEST);
            fs::write(
                changed_root.path().join(logical_name),
                format!("{source}\n// identity-only module change\n"),
            )
            .unwrap();

            let baseline = BuildPolicy::load_from(baseline_root.path()).unwrap();
            let changed = BuildPolicy::load_from(changed_root.path()).unwrap();
            let baseline_transition = &baseline.provenance.layers[0].transitions[0].evaluation;
            let changed_transition = &changed.provenance.layers[0].transitions[0].evaluation;
            let baseline_module = baseline_transition
                .imported_modules
                .iter()
                .find(|module| module.logical_name == logical_name)
                .unwrap();
            let changed_module = changed_transition
                .imported_modules
                .iter()
                .find(|module| module.logical_name == logical_name)
                .unwrap();

            assert_eq!(baseline.spec, changed.spec);
            assert_ne!(baseline_module.sha256, changed_module.sha256);
            assert_ne!(baseline_transition.sha256, changed_transition.sha256);
            assert_ne!(baseline.provenance.root.sha256, changed.provenance.root.sha256);
        }
    }

    #[test]
    fn ignores_undeclared_neighbor_files() {
        let root = fixture(REPOSITORY_MANIFEST);
        fs::write(root.path().join("ignored.glu"), "not valid Gluon").unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.targets.len(), 6);
        assert_ne!(policy.provenance.root.root_logical_name, "ignored.glu");
        assert!(
            policy
                .provenance
                .root
                .imported_modules
                .iter()
                .all(|module| module.logical_name != "ignored.glu")
        );
        assert!(policy.provenance.layers.iter().all(|layer| {
            layer.transitions.iter().all(|transition| {
                transition.evaluation.root_logical_name != "ignored.glu"
                    && transition
                        .evaluation
                        .imported_modules
                        .iter()
                        .all(|module| module.logical_name != "ignored.glu")
            })
        }));
    }
}
