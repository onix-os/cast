//! Explicit, ordered repository build policy.
//!
//! Cast evaluates one authored manifest and applies exactly the
//! modules named by that manifest. Directory contents and filesystem order
//! never participate in composition.

use std::path::{Path, PathBuf};

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, SourceRoot,
};
use declarative_config::Diagnostic;
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildPolicyEvaluator, BuildPolicyPatchSpec, BuildPolicySpec,
    LuaBuildPolicyEvaluator, TargetPolicySpec,
    layers::{
        BuildPolicyOperation, BuildPolicyRootConversionError,
        BuildPolicyRootSpec, LuaBuildPolicyRootEvaluator,
    },
};
use stone_recipe::derivation::{
    PolicyLayerProvenance, PolicyProvenance, PolicyTransitionProvenance, policy_composition_identity,
};
use thiserror::Error;

use crate::Env;

mod root_declaration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicy {
    pub spec: BuildPolicySpec,
    pub provenance: PolicyProvenance,
}

impl BuildPolicy {
    pub fn load(env: &Env) -> Result<Self, Error> {
        Self::load_from(&env.data_dir.join("policy"))
    }

    fn load_from(policy_dir: &Path) -> Result<Self, Error> {
        let root_declaration::LoadedPolicyRoot {
            path: root_path,
            source_root,
            source: root_source,
            manifest,
        } = root_declaration::load(policy_dir)?;
        let root_evaluator =
            <LuaBuildPolicyRootEvaluator as DeclarationEvaluator<BuildPolicyRootSpec>>::with_source_root(
                &LuaBuildPolicyRootEvaluator::default(),
                source_root.clone(),
            );
        let mut layers = Vec::with_capacity(manifest.layers.len());
        let mut state = None;
        let mut operation_order = 0;

        for (layer_index, layer) in manifest.layers.iter().enumerate() {
            let mut transitions = Vec::with_capacity(layer.entries.len());
            for (entry_index, entry) in layer.entries.iter().enumerate() {
                transitions.push(apply_entry(
                    &source_root,
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
            <LuaBuildPolicyRootEvaluator as DeclarationInputEvaluator<BuildPolicyRootSpec>>::evaluate_with_inputs(
                &root_evaluator,
                &root_source,
                &identity_inputs,
            )
                .map_err(|source| Error::FinalizeRoot {
                    path: root_path,
                    source: Box::new(source),
                })?;
        if finalized_root.value != manifest {
            return Err(Error::ManifestChanged { policy: manifest.name });
        }

        Ok(Self {
            spec,
            provenance: PolicyProvenance {
                name: manifest.name,
                root: finalized_root.identity,
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

/// Build the layer evaluator, source-rooted so its imports resolve beneath the
/// policy directory.
///
/// `origin` names the layer file whose language this selects; with one
/// registered language every layer decodes through the Lua adapter.
fn build_policy_evaluator_for(_origin: &str, source_root: &SourceRoot) -> BuildPolicyEvaluator {
    let evaluator = BuildPolicyEvaluator::Lua(LuaBuildPolicyEvaluator::default());
    <BuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::with_source_root(
        &evaluator,
        source_root.clone(),
    )
}

fn apply_entry(
    source_root: &SourceRoot,
    policy: &str,
    layer: &str,
    layer_index: usize,
    entry_index: usize,
    order: usize,
    operation: BuildPolicyOperation,
    origin: &str,
    state: &mut Option<BuildPolicySpec>,
) -> Result<PolicyTransitionProvenance, Error> {
    let evaluator = build_policy_evaluator_for(origin, source_root);
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
        .load(
            origin,
            <BuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::limits(&evaluator)
                .max_source_bytes,
        )
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
            let evaluated =
                <BuildPolicyEvaluator as DeclarationEvaluator<BuildPolicySpec>>::evaluate(
                    &evaluator,
                    &source,
                )
                .map_err(|source| Error::EvaluateEntry {
                    policy: policy.to_owned(),
                    layer: layer.to_owned(),
                    layer_index,
                    entry_index,
                    order,
                    operation,
                    origin: origin.to_owned(),
                    source: Box::new(source),
                })?;
            *state = Some(evaluated.value);
            evaluated.identity
        }
        BuildPolicyOperation::Modify => {
            let evaluated =
                <BuildPolicyEvaluator as DeclarationEvaluator<BuildPolicyPatchSpec>>::evaluate(
                    &evaluator,
                    &source,
                )
                .map_err(|source| Error::EvaluateEntry {
                        policy: policy.to_owned(),
                        layer: layer.to_owned(),
                        layer_index,
                        entry_index,
                        order,
                        operation,
                        origin: origin.to_owned(),
                        source: Box::new(source),
                    })?;
            let current = state.take().expect("modify precondition checked");
            let next = evaluated
                .value
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
            evaluated.identity
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
    #[error("prepare build-policy source root {path:?}")]
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
        source: Box<DeclarationEvaluationError<BuildPolicyRootConversionError>>,
    },
    #[error("load build-policy manifest declaration")]
    LoadRootDeclaration(
        #[source]
        Box<config::declaration::LoadFixedRootDeclarationError<BuildPolicyRootConversionError>>,
    ),
    #[error("finalize build-policy manifest identity {path:?}")]
    FinalizeRoot {
        path: PathBuf,
        #[source]
        source: Box<DeclarationEvaluationError<BuildPolicyRootConversionError>>,
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
        source: Box<DeclarationEvaluationError<BuildPolicyConversionError>>,
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
    use std::error::Error as _;

    use fs_err as fs;
    use sha2::{Digest, Sha256};

    use super::*;

    /// The shipped manifest — its foundation layer is `default.lua`.
    const REPOSITORY_MANIFEST: &str = include_str!("../data/policy/policy.lua");
    /// The shipped Lua policy authority Cast loads.
    const REPOSITORY_DEFAULT_LUA: &str = include_str!("../data/policy/default.lua");

    fn fixture(manifest: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("policy.lua"), manifest).unwrap();
        fs::write(root.path().join("default.lua"), REPOSITORY_DEFAULT_LUA).unwrap();
        root
    }

    /// A layer manifest naming one operation per entry.
    fn manifest(name: &str, layers: &[(&str, &[(&str, &str)])]) -> String {
        let mut output = format!("return {{\n    name = {name:?},\n    layers = {{\n");
        for (layer, entries) in layers {
            output.push_str(&format!("        {{ name = {layer:?}, entries = {{ "));
            for (operation, origin) in *entries {
                output.push_str(&format!(
                    "{{ operation = {{ kind = {operation:?} }}, origin = {origin:?} }}, "
                ));
            }
            output.push_str("} },\n");
        }
        output.push_str("    },\n}\n");
        output
    }

    /// A sparse policy patch: every field keeps its base value except
    /// `build_subdir`. Patch fields are total, so each one is named.
    fn build_subdir_patch(value: &str) -> String {
        format!(
            r#"return {{
    build_subdir = {{ kind = "set", value = {value:?} }},
    layout = {{ kind = "keep" }},
    toolchains = {{ kind = "keep" }},
    targets = {{ kind = "keep" }},
    retired_targets = {{ kind = "keep" }},
    sandbox = {{ kind = "keep" }},
    build_root = {{ kind = "keep" }},
    sources = {{ kind = "keep" }},
    tuning = {{ kind = "keep" }},
    environment = {{ kind = "keep" }},
    builders = {{ kind = "keep" }},
    analyzers = {{ kind = "keep" }},
    pgo = {{ kind = "keep" }},
}}
"#
        )
    }

    fn composition_digest(provenance: &PolicyProvenance) -> String {
        format!(
            "{:x}",
            Sha256::digest(policy_composition_identity(&provenance.name, &provenance.layers))
        )
    }

    /// The real shipped repository policy — `default.glu` plus the two large
    /// tuning catalogs it imports (`tuning/flags.glu`, `tuning/groups.glu`) —
    /// re-encodes to generated Lua and decodes back to an equal spec. This
    /// pairs those authored files with the build-policy write path,
    /// proving a generated-slot switch could reproduce them as `policy.lua`.
    #[test]
    fn the_repository_policy_round_trips_through_the_lua_emitter() {
        let policy = BuildPolicy::repository_for_tests();

        let emitted = stone_recipe::build_policy::encode_lua_policy(&policy.spec);
        assert!(emitted.starts_with(lua_config::GENERATED_LUA_MARKER));

        let redecoded = LuaBuildPolicyEvaluator::default()
            .evaluate(&declarative_config::Source::new("policy.lua", &emitted))
            .expect("emitted repository policy re-decodes");
        assert_eq!(policy.spec, redecoded);
    }

    /// One-shot: emit the shipped policy's `default.lua` from the currently
    /// composed spec, verifying it re-decodes to the same policy before writing.
    #[test]
    #[ignore = "one-shot shipped-data conversion tool"]
    fn generate_lua_policy_default() {
        let policy_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/policy");
        let policy = BuildPolicy::load_from(&policy_dir).expect("current policy loads");

        let lua = stone_recipe::build_policy::encode_lua_policy(&policy.spec);
        let redecoded = LuaBuildPolicyEvaluator::default()
            .evaluate(&declarative_config::Source::new("default.lua", &lua))
            .expect("emitted default.lua re-decodes");
        assert_eq!(policy.spec, redecoded);

        fs::write(policy_dir.join("default.lua"), lua).unwrap();
    }

    fn assert_same_diagnostic(actual: &Diagnostic, expected: &Diagnostic) {
        assert_eq!(actual.category, expected.category);
        assert_eq!(actual.limit, expected.limit);
        assert_eq!(actual.source_name, expected.source_name);
        assert_eq!(actual.span, expected.span);
        assert_eq!(actual.message, expected.message);
        assert_eq!(
            actual.source().map(ToString::to_string),
            expected.source().map(ToString::to_string)
        );
        assert_eq!(
            actual
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .map(std::io::Error::kind),
            expected
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .map(std::io::Error::kind)
        );
    }

    #[test]
    fn loads_the_explicit_repository_policy_layers() {
        let policy = BuildPolicy::repository_for_tests();

        assert_eq!(policy.provenance.name, "aerynos");
        assert_eq!(
            policy.provenance.root.root_logical_name,
            root_declaration::POLICY_ROOT_LOGICAL_NAME
        );
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
        assert_eq!(transition.origin, "default.lua");
        assert_eq!(transition.evaluation.root_logical_name, "default.lua");
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
    fn registered_root_discovery_preserves_complete_identity_golden() {
        let policy = BuildPolicy::repository_for_tests();
        let root = &policy.provenance.root;

        assert_eq!(root.root_logical_name, root_declaration::POLICY_ROOT_LOGICAL_NAME);
        assert_eq!(
            root.root_source_sha256,
            "e909010ddbbc85a0676cb4e35e73f143441aeb873ed83fe2557fd3e5b01df7ff"
        );
        assert!(
            root.modules.is_empty(),
            "the Lua policy manifest resolves no modules"
        );
        assert_eq!(root.language.as_str(), "lua");
        assert_eq!(root.engine.implementation(), "lua");
        assert_eq!(root.engine.version(), lua_config::LUA_VERSION);
        assert_eq!(root.configuration_abi.name(), "cast.configuration");
        assert_eq!(
            root.configuration_abi.version(),
            lua_config::CONFIGURATION_ABI_VERSION.to_string()
        );
        assert_eq!(
            root.evaluator_policy.as_str(),
            lua_config::EVALUATOR_POLICY_VERSION.to_string()
        );
        assert_eq!(root.explicit_inputs_sha256, composition_digest(&policy.provenance));
        assert_eq!(
            root.sha256,
            "8b761e7334acc2e094e6b02cc345bd99a30f914490e733989baf7c6955918d9b"
        );
        root.validate().unwrap();
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
        // Both the manifest and the foundation layer it names are
        // self-contained Lua authorities: they are evaluated by the Lua engine
        // and import nothing, so neither carries module provenance (the tuning
        // catalogs are inlined into the layer).
        assert!(
            policy.provenance.root.modules.is_empty(),
            "the Lua policy manifest imports nothing"
        );
        let transition = &policy.provenance.layers[0].transitions[0];
        assert_eq!(transition.evaluation.root_logical_name, "default.lua");
        assert_eq!(transition.evaluation.root_source_sha256.len(), 64);
        assert_eq!(transition.evaluation.explicit_inputs_sha256.len(), 64);
        assert_eq!(transition.evaluation.sha256.len(), 64);
        assert_eq!(transition.evaluation.engine.version(), lua_config::LUA_VERSION);
        assert_eq!(
            transition.evaluation.configuration_abi.version(),
            lua_config::CONFIGURATION_ABI_VERSION.to_string()
        );
        assert_eq!(
            transition.evaluation.evaluator_policy.as_str(),
            lua_config::EVALUATOR_POLICY_VERSION.to_string()
        );
        assert!(
            transition.evaluation.modules.is_empty(),
            "the Lua foundation authority imports nothing"
        );
    }

    #[test]
    fn preserves_named_empty_layers_in_manifest_order_and_v2_identity() {
        let root = fixture(&manifest(
            "empty-layer-policy",
            &[
                ("foundation", &[("add", "default.lua")]),
                ("reserved-site-layer", &[]),
            ],
        ));

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
        let root = fixture(&manifest(
            "test-policy",
            &[
                ("foundation", &[("add", "default.lua")]),
                ("site", &[("modify", "modify.lua"), ("replace", "replacement.lua")]),
            ],
        ));
        fs::write(root.path().join("modify.lua"), build_subdir_patch("modified-builddir")).unwrap();
        fs::write(
            root.path().join("replacement.lua"),
            REPOSITORY_DEFAULT_LUA.replace("aerynos-builddir", "final-builddir"),
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
        assert_eq!(policy.provenance.layers[1].transitions[0].origin, "modify.lua");
    }

    #[test]
    fn rejects_each_invalid_state_transition_with_context() {
        let cases = [
            (
                &[("add", "default.lua"), ("add", "default.lua")][..],
                BuildPolicyOperation::Add,
                1,
            ),
            (&[("replace", "default.lua")][..], BuildPolicyOperation::Replace, 0),
            (&[("modify", "modify.lua")][..], BuildPolicyOperation::Modify, 0),
        ];
        for (entries, expected_operation, expected_order) in cases {
            let root = fixture(&manifest("strict-policy", &[("strict-layer", entries)]));
            fs::write(root.path().join("modify.lua"), "not reached").unwrap();

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
        let root = fixture(&manifest(
            "validated-policy",
            &[("site", &[("add", "default.lua"), ("modify", "invalid.lua")])],
        ));
        fs::write(root.path().join("invalid.lua"), build_subdir_patch("")).unwrap();

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
            } if policy == "validated-policy" && layer == "site" && origin == "invalid.lua"
        ));
    }

    #[test]
    fn ignores_undeclared_neighbor_files() {
        let root = fixture(REPOSITORY_MANIFEST);
        fs::write(root.path().join("ignored.lua"), "not valid Lua").unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(policy.spec.targets.len(), 6);
        assert_ne!(policy.provenance.root.root_logical_name, "ignored.glu");
        assert!(
            policy
                .provenance
                .root
                .modules
                .iter()
                .all(|module| module.logical_name != "ignored.glu")
        );
        assert!(policy.provenance.layers.iter().all(|layer| {
            layer.transitions.iter().all(|transition| {
                transition.evaluation.root_logical_name != "ignored.glu"
                    && transition
                        .evaluation
                        .modules
                        .iter()
                        .all(|module| module.logical_name != "ignored.glu")
            })
        }));
    }

    #[test]
    fn registered_root_discovery_preserves_v1_name_and_exact_source_bytes() {
        let manifest = format!(
            "{REPOSITORY_MANIFEST}\n-- fixed-root discovery identity sentinel\n"
        );
        let root = fixture(&manifest);
        fs::write(
            root.path().join("policy.glu"),
            "this unregistered neighbor must never be inspected",
        )
        .unwrap();

        let policy = BuildPolicy::load_from(root.path()).unwrap();

        assert_eq!(
            policy.provenance.root.root_logical_name,
            root_declaration::POLICY_ROOT_LOGICAL_NAME
        );
        assert_eq!(
            policy.provenance.root.root_source_sha256,
            format!("{:x}", Sha256::digest(manifest.as_bytes()))
        );
        policy.provenance.root.validate().unwrap();
    }

    #[test]
    fn registered_root_discovery_preserves_symlink_diagnostics() {
        use std::os::unix::fs::symlink;

        let root = fixture(REPOSITORY_MANIFEST);
        fs::remove_file(root.path().join("policy.lua")).unwrap();
        fs::write(root.path().join("policy-target.lua"), REPOSITORY_MANIFEST)
            .unwrap();
        symlink("policy-target.lua", root.path().join("policy.lua")).unwrap();
        let expected = SourceRoot::new(root.path())
            .unwrap()
            .load(
                "policy.lua",
                <LuaBuildPolicyRootEvaluator as DeclarationEvaluator<
                    BuildPolicyRootSpec,
                >>::limits(&LuaBuildPolicyRootEvaluator::default())
                .max_source_bytes,
            )
            .unwrap_err();

        let error = BuildPolicy::load_from(root.path()).unwrap_err();

        match error {
            Error::LoadRoot { path, source } => {
                assert_eq!(path, root.path().join("policy.lua"));
                assert_same_diagnostic(&source, &expected);
            }
            error => panic!("expected legacy LoadRoot error, found {error:?}"),
        }
    }

    #[test]
    fn registered_root_discovery_preserves_missing_directory_diagnostics() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing-policy-root");
        let expected = SourceRoot::new(&missing).unwrap_err();

        let error = BuildPolicy::load_from(&missing).unwrap_err();

        match error {
            Error::SourceRoot { path, source } => {
                assert_eq!(path, missing);
                assert_same_diagnostic(&source, &expected);
            }
            error => panic!("expected legacy SourceRoot error, found {error:?}"),
        }
    }

    #[test]
    fn registered_root_discovery_preserves_missing_manifest_diagnostics() {
        let root = fixture(REPOSITORY_MANIFEST);
        fs::remove_file(root.path().join("policy.lua")).unwrap();
        let expected = SourceRoot::new(root.path())
            .unwrap()
            .load(
                "policy.lua",
                <LuaBuildPolicyRootEvaluator as DeclarationEvaluator<
                    BuildPolicyRootSpec,
                >>::limits(&LuaBuildPolicyRootEvaluator::default())
                .max_source_bytes,
            )
            .unwrap_err();

        let error = BuildPolicy::load_from(root.path()).unwrap_err();

        match error {
            Error::LoadRoot { path, source } => {
                assert_eq!(path, root.path().join("policy.lua"));
                assert_same_diagnostic(&source, &expected);
            }
            error => panic!("expected legacy LoadRoot error, found {error:?}"),
        }
    }

    #[test]
    fn registered_root_discovery_accepts_a_symlinked_policy_directory() {
        use std::os::unix::fs::symlink;

        let root = fixture(REPOSITORY_MANIFEST);
        let links = tempfile::tempdir().unwrap();
        let linked = links.path().join("policy-root");
        symlink(root.path(), &linked).unwrap();

        let direct = BuildPolicy::load_from(root.path()).unwrap();
        let through_link = BuildPolicy::load_from(&linked).unwrap();

        assert_eq!(through_link, direct);
    }

    /// A malformed manifest reaches the caller as the engine's own diagnostic,
    /// carrying the root slot's logical name and the position evidence the
    /// parser produced.
    #[test]
    fn registered_root_discovery_preserves_engine_diagnostics() {
        let root = fixture("return { name = ");

        let error = BuildPolicy::load_from(root.path()).unwrap_err();

        let Error::EvaluateRoot { path, source } = error else {
            panic!("expected an EvaluateRoot error, found {error:?}");
        };
        assert_eq!(path, root.path().join("policy.lua"));
        let DeclarationEvaluationError::Evaluation(diagnostic) = *source else {
            panic!("expected an engine diagnostic, found {source:?}");
        };
        assert_eq!(
            diagnostic.source_name.as_deref(),
            Some(root_declaration::POLICY_ROOT_LOGICAL_NAME)
        );
        assert!(diagnostic.span.is_some(), "the parser's span is preserved");
        assert!(
            diagnostic.message.contains("does not parse"),
            "unexpected diagnostic {diagnostic:?}"
        );
    }
}
