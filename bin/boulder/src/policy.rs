// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Explicit, ordered repository build policy.
//!
//! Boulder evaluates one authored Gluon manifest and applies exactly the
//! modules named by that manifest. Directory contents and filesystem order
//! never participate in composition.

use std::path::{Path, PathBuf};

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, SourceRoot};
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildPolicyEvaluationError, BuildPolicySpec, TargetPolicySpec,
    layers::{BuildPolicyOperation, BuildPolicyRootEvaluationError},
};
use thiserror::Error;

use crate::Env;

const COMPOSITION_IDENTITY_DOMAIN: &str = "boulder-build-policy-composition-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicy {
    pub spec: BuildPolicySpec,
    pub fingerprint: EvaluationFingerprint,
    pub origin: String,
    sources: Vec<PolicySource>,
    changes: Vec<PolicyChange>,
}

/// One root or imported source which contributed to policy evaluation.
///
/// A configured layer module is an evaluation root too. Consequently more
/// than one item may have `root = true`; authored operation context lives in
/// [`PolicyChange`]. Entries remain in manifest/evaluation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySource {
    pub origin: String,
    pub fingerprint: String,
    pub root: bool,
}

/// One successfully applied, ordered policy state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyChange {
    pub policy: String,
    pub layer: String,
    pub layer_order: usize,
    pub entry_order: usize,
    pub order: usize,
    pub operation: BuildPolicyOperation,
    pub origin: String,
    /// Complete evaluation identity, including every imported module.
    pub fingerprint: EvaluationFingerprint,
}

impl PolicyChange {
    pub fn operation_name(&self) -> &'static str {
        operation_name(self.operation)
    }
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
        let evaluator = Evaluator::default().with_source_root(source_root.clone());
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
        let mut sources = sources_for(root_source.logical_name(), &evaluated_root.fingerprint);
        let mut changes = Vec::new();
        let mut state = None;

        for (layer_order, layer) in manifest.layers.iter().enumerate() {
            for (entry_order, entry) in layer.entries.iter().enumerate() {
                let order = changes.len();
                apply_entry(
                    &source_root,
                    &evaluator,
                    &manifest.name,
                    &layer.name,
                    layer_order,
                    entry_order,
                    order,
                    entry.operation,
                    &entry.origin,
                    &mut state,
                    &mut sources,
                    &mut changes,
                )?;
            }
        }

        let spec = state.ok_or_else(|| Error::MissingPolicy {
            policy: manifest.name.clone(),
        })?;
        let identity_inputs = composition_identity(&manifest.name, &manifest.layers, &changes);
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
            fingerprint: finalized_root.fingerprint,
            origin: root_source.logical_name().to_owned(),
            sources,
            changes,
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
        self.sources.clone()
    }

    pub fn changes(&self) -> &[PolicyChange] {
        &self.changes
    }

    #[cfg(test)]
    pub(crate) fn repository_for_tests() -> Self {
        static REPOSITORY: std::sync::OnceLock<BuildPolicy> = std::sync::OnceLock::new();
        let policy_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/policy");
        REPOSITORY.get_or_init(|| Self::load_from(&policy_dir).unwrap()).clone()
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_entry(
    source_root: &SourceRoot,
    evaluator: &Evaluator,
    policy: &str,
    layer: &str,
    layer_order: usize,
    entry_order: usize,
    order: usize,
    operation: BuildPolicyOperation,
    origin: &str,
    state: &mut Option<BuildPolicySpec>,
    sources: &mut Vec<PolicySource>,
    changes: &mut Vec<PolicyChange>,
) -> Result<(), Error> {
    match operation {
        BuildPolicyOperation::Add if state.is_some() => {
            return Err(Error::InvalidTransition {
                policy: policy.to_owned(),
                layer: layer.to_owned(),
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
                    order,
                    operation,
                    origin: origin.to_owned(),
                    source: Box::new(source),
                })?;
            *state = Some(next);
            evaluated.fingerprint
        }
    };

    sources.extend(sources_for(source.logical_name(), &fingerprint));
    changes.push(PolicyChange {
        policy: policy.to_owned(),
        layer: layer.to_owned(),
        layer_order,
        entry_order,
        order,
        operation,
        origin: origin.to_owned(),
        fingerprint,
    });
    Ok(())
}

fn sources_for(origin: &str, fingerprint: &EvaluationFingerprint) -> Vec<PolicySource> {
    std::iter::once(PolicySource {
        origin: origin.to_owned(),
        fingerprint: fingerprint.root_source_sha256.clone(),
        root: true,
    })
    .chain(fingerprint.imported_modules.iter().map(|module| PolicySource {
        origin: module.logical_name.clone(),
        fingerprint: module.sha256.clone(),
        root: false,
    }))
    .collect()
}

fn composition_identity(
    policy: &str,
    layers: &[stone_recipe::build_policy::layers::BuildPolicyLayerSpec],
    changes: &[PolicyChange],
) -> Vec<u8> {
    let mut output = Vec::new();
    encode_field(&mut output, COMPOSITION_IDENTITY_DOMAIN.as_bytes());
    encode_field(&mut output, policy.as_bytes());
    encode_count(&mut output, layers.len());

    let mut next_change = 0;
    for (layer_order, layer) in layers.iter().enumerate() {
        encode_index(&mut output, layer_order);
        encode_field(&mut output, layer.name.as_bytes());
        encode_count(&mut output, layer.entries.len());
        for (entry_order, entry) in layer.entries.iter().enumerate() {
            let change = &changes[next_change];
            next_change += 1;
            debug_assert_eq!(change.layer_order, layer_order);
            debug_assert_eq!(change.entry_order, entry_order);
            debug_assert_eq!(change.operation, entry.operation);
            debug_assert_eq!(change.origin, entry.origin);

            encode_index(&mut output, entry_order);
            encode_field(&mut output, operation_name(entry.operation).as_bytes());
            encode_field(&mut output, entry.origin.as_bytes());
            encode_fingerprint(&mut output, &change.fingerprint);
        }
    }
    debug_assert_eq!(next_change, changes.len());
    output
}

fn encode_fingerprint(output: &mut Vec<u8>, fingerprint: &EvaluationFingerprint) {
    encode_field(output, fingerprint.root_source_sha256.as_bytes());
    encode_count(output, fingerprint.imported_modules.len());
    for module in &fingerprint.imported_modules {
        encode_field(output, module.logical_name.as_bytes());
        encode_field(output, module.sha256.as_bytes());
    }
    encode_field(output, fingerprint.gluon_version.as_bytes());
    output.extend_from_slice(&fingerprint.configuration_abi_version.to_le_bytes());
    output.extend_from_slice(&fingerprint.evaluator_policy_version.to_le_bytes());
    encode_field(output, fingerprint.explicit_inputs_sha256.as_bytes());
    encode_field(output, fingerprint.sha256.as_bytes());
}

fn encode_count(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(
        &u64::try_from(count)
            .expect("supported collection length fits u64")
            .to_le_bytes(),
    );
}

fn encode_index(output: &mut Vec<u8>, index: usize) {
    output.extend_from_slice(
        &u64::try_from(index)
            .expect("supported policy index fits u64")
            .to_le_bytes(),
    );
}

fn encode_field(output: &mut Vec<u8>, value: &[u8]) {
    encode_count(output, value.len());
    output.extend_from_slice(value);
}

const fn operation_name(operation: BuildPolicyOperation) -> &'static str {
    match operation {
        BuildPolicyOperation::Add => "add",
        BuildPolicyOperation::Replace => "replace",
        BuildPolicyOperation::Modify => "modify",
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
    #[error("policy `{policy}` layer `{layer}` operation {order} ({operation:?}) from `{origin}` is invalid: {reason}")]
    InvalidTransition {
        policy: String,
        layer: String,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        reason: &'static str,
    },
    #[error("policy `{policy}` layer `{layer}` operation {order} ({operation:?}) cannot load `{origin}` at {path:?}")]
    LoadEntry {
        policy: String,
        layer: String,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("policy `{policy}` layer `{layer}` operation {order} ({operation:?}) cannot evaluate `{origin}`")]
    EvaluateEntry {
        policy: String,
        layer: String,
        order: usize,
        operation: BuildPolicyOperation,
        origin: String,
        #[source]
        source: Box<BuildPolicyEvaluationError>,
    },
    #[error("policy `{policy}` layer `{layer}` operation {order} ({operation:?}) cannot apply `{origin}`")]
    ApplyPatch {
        policy: String,
        layer: String,
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
    #[error("unknown build-policy target `{requested}`; available targets: {}", available.join(", "))]
    UnknownTarget { requested: String, available: Vec<String> },
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;

    const REPOSITORY_MANIFEST: &str = include_str!("../data/policy/policy.glu");
    const REPOSITORY_DEFAULT: &str = include_str!("../data/policy/default.glu");

    fn fixture(manifest: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("policy.glu"), manifest).unwrap();
        fs::write(root.path().join("default.glu"), REPOSITORY_DEFAULT).unwrap();
        root
    }

    #[test]
    fn loads_the_explicit_repository_policy_layers() {
        let policy = BuildPolicy::repository_for_tests();

        assert_eq!(policy.origin, "policy.glu");
        assert_eq!(policy.spec.vendor_id, "aerynos-linux");
        assert_eq!(
            policy.target("x86_64").unwrap().target_triple,
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(policy.changes.len(), 1);
        assert_eq!(policy.changes[0].policy, "aerynos");
        assert_eq!(policy.changes[0].layer, "foundation");
        assert_eq!(policy.changes[0].operation, BuildPolicyOperation::Add);
        assert_eq!(policy.changes[0].origin, "default.glu");
        assert_ne!(
            policy.fingerprint.explicit_inputs_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
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
    fn exposes_manifest_and_layer_root_import_provenance() {
        let policy = BuildPolicy::repository_for_tests();
        let sources = policy.sources();

        assert_eq!(sources[0].origin, "policy.glu");
        assert!(sources[0].root);
        assert!(
            sources
                .iter()
                .any(|source| source.origin == "boulder.build_policy.layers.v1")
        );
        assert!(
            sources
                .iter()
                .any(|source| source.origin == "default.glu" && source.root)
        );
        assert!(sources.iter().any(|source| source.origin == "boulder.build_policy.v1"));
    }

    #[test]
    fn applies_add_modify_and_replace_in_authored_order() {
        let root = fixture(
            r#"
let l = import! boulder.build_policy.layers.v1
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
let b = import! boulder.build_policy.v1
b.policy_patch {
    vendor_id = b.patch.set "modified-linux",
    .. b.defaults.policy_patch
}
"#,
        )
        .unwrap();
        fs::write(
            root.path().join("replacement.glu"),
            REPOSITORY_DEFAULT.replace("aerynos-linux", "final-linux"),
        )
        .unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.vendor_id, "final-linux");
        assert_eq!(
            policy
                .changes()
                .iter()
                .map(PolicyChange::operation_name)
                .collect::<Vec<_>>(),
            ["add", "modify", "replace"]
        );
        assert_eq!(policy.changes[1].layer_order, 1);
        assert_eq!(policy.changes[1].entry_order, 0);
        assert_eq!(policy.changes[1].order, 1);
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
let l = import! boulder.build_policy.layers.v1
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
                    order,
                    operation,
                    origin: _,
                    reason: _,
                } if policy == "strict-policy"
                    && layer == "strict-layer"
                    && order == expected_order
                    && operation == expected_operation
            ));
        }
    }

    #[test]
    fn rejects_invalid_intermediate_patch_with_operation_context() {
        let root = fixture(
            r#"
let l = import! boulder.build_policy.layers.v1
l.policy "validated-policy" [l.layer "site" [
    l.add "default.glu",
    l.modify "invalid.glu",
]]
"#,
        );
        fs::write(
            root.path().join("invalid.glu"),
            r#"
let b = import! boulder.build_policy.v1
b.policy_patch {
    vendor_id = b.patch.set "",
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
        assert_eq!(first.fingerprint.sha256, repeated.fingerprint.sha256);
        assert_eq!(first.spec, changed.spec);
        assert_ne!(
            first.changes[0].fingerprint.sha256,
            changed.changes[0].fingerprint.sha256
        );
        assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
    }

    #[test]
    fn ignores_undeclared_neighbor_files() {
        let root = fixture(REPOSITORY_MANIFEST);
        fs::write(root.path().join("ignored.glu"), "not valid Gluon").unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.targets.len(), 6);
        assert!(policy.sources.iter().all(|source| source.origin != "ignored.glu"));
    }
}
