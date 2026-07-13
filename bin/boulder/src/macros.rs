// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, SourceRoot};
use stone_recipe::{PolicyKind, PolicyOperation};
use thiserror::Error;

use crate::Env;

#[derive(Debug, Clone)]
pub struct Macros {
    pub arch: BTreeMap<String, stone_recipe::Macros>,
    pub actions: Vec<stone_recipe::Macros>,
    /// Complete fingerprint of the explicit policy root and every imported
    /// policy module. This is retained for the derivation-plan boundary.
    pub fingerprint: EvaluationFingerprint,
    /// Ordered explanation of every policy add, replacement, or modification.
    pub provenance: Vec<PolicyChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyChange {
    pub layer_name: String,
    pub layer_order: usize,
    pub entry_order: usize,
    pub operation: PolicyOperation,
    pub kind: PolicyKind,
    pub key: String,
    pub origin: String,
    pub fingerprint: EvaluationFingerprint,
}

/// The target-specific view of the repository policy used by a build target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySelection {
    pub target: String,
    pub fingerprint: EvaluationFingerprint,
    pub changes: Vec<PolicyChange>,
}

impl Macros {
    const POLICY_ROOT: &'static str = "policy.glu";

    pub fn load(env: &Env) -> Result<Self, Error> {
        let macros_dir = env.data_dir.join("macros");
        Self::load_from(&macros_dir)
    }

    fn load_from(macros_dir: &Path) -> Result<Self, Error> {
        let source_root = SourceRoot::new(macros_dir).map_err(|source| Error::SourceRoot {
            path: macros_dir.to_path_buf(),
            source: Box::new(source),
        })?;
        let evaluator = Evaluator::default().with_source_root(source_root.clone());
        let root_path = macros_dir.join(Self::POLICY_ROOT);
        let source = source_root
            .load(Self::POLICY_ROOT, evaluator.limits().max_source_bytes)
            .map_err(|source| Error::Load {
                path: root_path.clone(),
                source: Box::new(source),
            })?;
        let declared =
            stone_recipe::evaluate_policy_gluon_with(&evaluator, &source).map_err(|source| Error::Evaluate {
                path: root_path.clone(),
                source: Box::new(source),
            })?;

        let mut modules = Vec::new();
        let mut explicit_inputs = b"boulder-policy-modules-v1\0".to_vec();
        let mut layer_names = BTreeSet::new();
        for (layer_order, layer) in declared.layers.into_iter().enumerate() {
            if layer.name.trim().is_empty() {
                return Err(Error::EmptyLayerName { layer_order });
            }
            if !layer_names.insert(layer.name.clone()) {
                return Err(Error::DuplicateLayerName { name: layer.name });
            }
            append_fingerprint_input(&mut explicit_inputs, layer.name.as_bytes());
            for (entry_order, declaration) in layer.entries.into_iter().enumerate() {
                let path = macros_dir.join(&declaration.origin);
                let source = source_root
                    .load(&declaration.origin, evaluator.limits().max_source_bytes)
                    .map_err(|source| Error::LoadModule {
                        path: path.clone(),
                        source: Box::new(source),
                    })?;
                let module = stone_recipe::evaluate_macros_gluon_with(&evaluator, &source).map_err(|source| {
                    Error::EvaluateModule {
                        path,
                        source: Box::new(source),
                    }
                })?;
                append_fingerprint_input(&mut explicit_inputs, declaration.origin.as_bytes());
                append_fingerprint_input(&mut explicit_inputs, module.fingerprint.sha256.as_bytes());
                modules.push(EvaluatedModule {
                    layer_name: layer.name.clone(),
                    layer_order,
                    entry_order,
                    declaration,
                    macros: module.macros,
                    fingerprint: module.fingerprint,
                });
            }
        }

        let fingerprinted = stone_recipe::evaluate_policy_gluon_with_inputs(&evaluator, &source, &explicit_inputs)
            .map_err(|source| Error::Evaluate {
                path: root_path,
                source: Box::new(source),
            })?;

        compose(fingerprinted.fingerprint, modules)
    }

    pub fn selection(&self, target: impl ToString) -> PolicySelection {
        let target = target.to_string();
        let changes = self
            .provenance
            .iter()
            .filter(|change| {
                change.kind == PolicyKind::Actions
                    || (change.kind == PolicyKind::Architecture && (change.key == "base" || change.key == target))
            })
            .cloned()
            .collect();

        PolicySelection {
            target,
            fingerprint: self.fingerprint.clone(),
            changes,
        }
    }

    #[cfg(test)]
    pub(crate) fn repository_for_tests() -> Self {
        static REPOSITORY: std::sync::OnceLock<Macros> = std::sync::OnceLock::new();
        let macros_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/macros");
        REPOSITORY.get_or_init(|| Self::load_from(&macros_dir).unwrap()).clone()
    }
}

fn append_fingerprint_input(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

#[derive(Debug)]
struct ComposedModule {
    order: usize,
    macros: stone_recipe::Macros,
    introduced_by: String,
}

#[derive(Debug)]
struct EvaluatedModule {
    layer_name: String,
    layer_order: usize,
    entry_order: usize,
    declaration: stone_recipe::PolicyModule,
    macros: stone_recipe::Macros,
    fingerprint: EvaluationFingerprint,
}

fn compose(fingerprint: EvaluationFingerprint, modules: Vec<EvaluatedModule>) -> Result<Macros, Error> {
    let mut composed = BTreeMap::<(PolicyKind, String), ComposedModule>::new();
    let mut provenance = Vec::with_capacity(modules.len());

    for (order, evaluated) in modules.into_iter().enumerate() {
        let layer_name = evaluated.layer_name;
        let layer_order = evaluated.layer_order;
        let entry_order = evaluated.entry_order;
        let module = evaluated.declaration;
        let map_key = (module.kind, module.key.clone());
        match module.operation {
            PolicyOperation::Add => match composed.entry(map_key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ComposedModule {
                        order,
                        macros: evaluated.macros,
                        introduced_by: module.origin.clone(),
                    });
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    return Err(Error::DuplicateAdd {
                        kind: module.kind,
                        key: module.key,
                        origin: module.origin,
                        previous: entry.get().introduced_by.clone(),
                    });
                }
            },
            PolicyOperation::Replace => {
                let Some(current) = composed.get_mut(&map_key) else {
                    return Err(Error::MissingReplace {
                        kind: module.kind,
                        key: module.key,
                        origin: module.origin,
                    });
                };
                current.macros = evaluated.macros;
                current.introduced_by = module.origin.clone();
            }
            PolicyOperation::Modify => {
                let Some(current) = composed.get_mut(&map_key) else {
                    return Err(Error::MissingModify {
                        kind: module.kind,
                        key: module.key,
                        origin: module.origin,
                    });
                };
                merge(&mut current.macros, evaluated.macros);
            }
        }

        provenance.push(PolicyChange {
            layer_name,
            layer_order,
            entry_order,
            operation: module.operation,
            kind: module.kind,
            key: module.key,
            origin: module.origin,
            fingerprint: evaluated.fingerprint,
        });
    }

    let mut actions = composed
        .iter()
        .filter(|((kind, _), _)| *kind == PolicyKind::Actions)
        .map(|(_, module)| (module.order, module.macros.clone()))
        .collect::<Vec<_>>();
    actions.sort_by_key(|(order, _)| *order);

    let arch = composed
        .into_iter()
        .filter(|((kind, _), _)| *kind == PolicyKind::Architecture)
        .map(|((_, key), module)| (key, module.macros))
        .collect();

    Ok(Macros {
        arch,
        actions: actions.into_iter().map(|(_, macros)| macros).collect(),
        fingerprint,
        provenance,
    })
}

fn merge(current: &mut stone_recipe::Macros, mut incoming: stone_recipe::Macros) {
    current.actions.append(&mut incoming.actions);
    current.definitions.append(&mut incoming.definitions);
    current.flags.append(&mut incoming.flags);
    current.tuning.append(&mut incoming.tuning);
    current
        .default_tuning_groups
        .append(&mut incoming.default_tuning_groups);
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("prepare macro Gluon source root {path:?}")]
    SourceRoot {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("load macro policy root {path:?}")]
    Load {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("evaluate macro policy root {path:?}")]
    Evaluate {
        path: PathBuf,
        #[source]
        source: Box<stone_recipe::PolicyEvaluationError>,
    },
    #[error("load macro policy module {path:?}")]
    LoadModule {
        path: PathBuf,
        #[source]
        source: Box<Diagnostic>,
    },
    #[error("evaluate macro policy module {path:?}")]
    EvaluateModule {
        path: PathBuf,
        #[source]
        source: Box<stone_recipe::MacrosEvaluationError>,
    },
    #[error("policy layer at order {layer_order} must have a non-empty name")]
    EmptyLayerName { layer_order: usize },
    #[error("policy layer name `{name}` is declared more than once")]
    DuplicateLayerName { name: String },
    #[error("policy module `{origin}` cannot add duplicate {kind:?} key `{key}`; it was introduced by `{previous}`")]
    DuplicateAdd {
        kind: PolicyKind,
        key: String,
        origin: String,
        previous: String,
    },
    #[error("policy module `{origin}` cannot replace missing {kind:?} key `{key}`")]
    MissingReplace {
        kind: PolicyKind,
        key: String,
        origin: String,
    },
    #[error("policy module `{origin}` cannot modify missing {kind:?} key `{key}`")]
    MissingModify {
        kind: PolicyKind,
        key: String,
        origin: String,
    },
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;

    const EMPTY: &str = r#"let boulder = import! boulder.macros.v1
boulder.macros
"#;

    fn action(key: &str) -> String {
        format!(
            r#"let boulder = import! boulder.macros.v1
{{
    actions = [boulder.named {key:?} (boulder.action.new {key:?} {key:?})],
    .. boulder.macros
}}
"#,
        )
    }

    fn layout() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("actions")).unwrap();
        fs::create_dir_all(root.path().join("arch/emul32")).unwrap();
        root
    }

    fn policy_root(declarations: &str, entries: &str) -> String {
        format!(
            r#"let policy = import! boulder.policy.v1
{declarations}
policy.policy [
    policy.layer "test" [
{entries}
    ],
]
"#
        )
    }

    #[test]
    fn loads_only_modules_declared_by_the_policy_root_in_authored_order() {
        let root = layout();
        fs::write(root.path().join("actions/z.glu"), action("z")).unwrap();
        fs::write(root.path().join("actions/a.glu"), action("a")).unwrap();
        fs::write(root.path().join("actions/ignored.glu"), "not valid gluon").unwrap();
        fs::write(root.path().join("arch/base.glu"), EMPTY).unwrap();
        fs::write(root.path().join("arch/emul32/x86_64.glu"), EMPTY).unwrap();
        fs::write(
            root.path().join("policy.glu"),
            policy_root(
                "",
                r#"    policy.add (policy.actions "z" "actions/z.glu"),
    policy.add (policy.actions "a" "actions/a.glu"),
    policy.add (policy.architecture "base" "arch/base.glu"),
    policy.add (policy.architecture "emul32/x86_64" "arch/emul32/x86_64.glu"),"#,
            ),
        )
        .unwrap();

        let macros = Macros::load_from(root.path()).unwrap();

        assert_eq!(
            macros
                .actions
                .iter()
                .map(|macros| macros.actions[0].key.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert_eq!(
            macros.arch.keys().map(String::as_str).collect::<Vec<_>>(),
            ["base", "emul32/x86_64"]
        );
        assert_eq!(macros.provenance.len(), 4);
        assert_eq!(macros.provenance[0].layer_name, "test");
        assert_eq!(macros.provenance[0].layer_order, 0);
        assert_eq!(macros.provenance[0].entry_order, 0);
        assert_eq!(macros.provenance[0].origin, "actions/z.glu");
        assert_ne!(
            macros.fingerprint.explicit_inputs_sha256,
            macros.fingerprint.root_source_sha256
        );
        assert!(
            macros
                .fingerprint
                .imported_modules
                .iter()
                .all(|module| module.logical_name != "actions/ignored.glu")
        );

        fs::write(root.path().join("actions/z.glu"), action("changed-z")).unwrap();
        let changed = Macros::load_from(root.path()).unwrap();
        assert_ne!(changed.fingerprint.sha256, macros.fingerprint.sha256);
        assert_ne!(changed.provenance[0].fingerprint, macros.provenance[0].fingerprint);
    }

    #[test]
    fn evaluates_contained_relative_imports() {
        let root = layout();
        fs::write(root.path().join("actions/shared.glu"), action("shared")).unwrap();
        fs::write(
            root.path().join("actions/wrapper.glu"),
            "let shared = import! \"shared.glu\"\nshared\n",
        )
        .unwrap();
        fs::write(
            root.path().join("policy.glu"),
            policy_root(
                "",
                "    policy.add (policy.actions \"wrapper\" \"actions/wrapper.glu\"),",
            ),
        )
        .unwrap();

        let macros = Macros::load_from(root.path()).unwrap();

        assert_eq!(macros.actions.len(), 1);
        assert_eq!(macros.actions[0].actions[0].key, "shared");
        assert!(
            macros.provenance[0]
                .fingerprint
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "actions/shared.glu")
        );
    }

    #[test]
    fn reports_the_path_of_invalid_gluon() {
        let root = layout();
        let invalid = root.path().join("actions/bad.glu");
        fs::write(&invalid, "this is not gluon").unwrap();
        fs::write(
            root.path().join("policy.glu"),
            policy_root("", "    policy.add (policy.actions \"bad\" \"actions/bad.glu\"),"),
        )
        .unwrap();

        let error = Macros::load_from(root.path()).unwrap_err();

        let Error::EvaluateModule { path, source } = error else {
            panic!("unexpected error")
        };
        assert_eq!(path, invalid);
        assert!(source.to_string().contains("evaluate macro Gluon"));
    }

    #[test]
    fn policy_operations_are_strict_and_modify_is_ordered() {
        let root = layout();
        fs::write(root.path().join("actions/first.glu"), action("first")).unwrap();
        fs::write(root.path().join("actions/second.glu"), action("second")).unwrap();
        fs::write(
            root.path().join("policy.glu"),
            policy_root(
                "",
                r#"    policy.add (policy.actions "build" "actions/first.glu"),
    policy.modify (policy.actions "build" "actions/second.glu"),"#,
            ),
        )
        .unwrap();

        let macros = Macros::load_from(root.path()).unwrap();
        assert_eq!(
            macros.actions[0]
                .actions
                .iter()
                .map(|action| action.key.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(macros.provenance[1].operation, PolicyOperation::Modify);

        fs::write(
            root.path().join("policy.glu"),
            policy_root(
                "",
                r#"    policy.add (policy.actions "build" "actions/first.glu"),
    policy.add (policy.actions "build" "actions/first.glu"),"#,
            ),
        )
        .unwrap();
        assert!(matches!(
            Macros::load_from(root.path()),
            Err(Error::DuplicateAdd { ref key, .. }) if key == "build"
        ));

        fs::write(
            root.path().join("policy.glu"),
            policy_root(
                "",
                "    policy.replace (policy.actions \"missing\" \"actions/first.glu\"),",
            ),
        )
        .unwrap();
        assert!(matches!(
            Macros::load_from(root.path()),
            Err(Error::MissingReplace { ref key, .. }) if key == "missing"
        ));
    }

    #[test]
    fn named_layers_compose_once_in_authored_order() {
        let root = layout();
        fs::write(root.path().join("actions/first.glu"), action("first")).unwrap();
        fs::write(root.path().join("actions/second.glu"), action("second")).unwrap();
        fs::write(
            root.path().join("policy.glu"),
            r#"let policy = import! boulder.policy.v1
policy.policy [
    policy.layer "foundation" [
        policy.add (policy.actions "build" "actions/first.glu"),
    ],
    policy.layer "repository-overrides" [
        policy.modify (policy.actions "build" "actions/second.glu"),
    ],
]
"#,
        )
        .unwrap();

        let macros = Macros::load_from(root.path()).unwrap();

        assert_eq!(macros.provenance[0].layer_name, "foundation");
        assert_eq!(macros.provenance[0].layer_order, 0);
        assert_eq!(macros.provenance[0].entry_order, 0);
        assert_eq!(macros.provenance[1].layer_name, "repository-overrides");
        assert_eq!(macros.provenance[1].layer_order, 1);
        assert_eq!(macros.provenance[1].entry_order, 0);
        assert_eq!(macros.provenance[1].origin, "actions/second.glu");

        fs::write(
            root.path().join("policy.glu"),
            r#"let policy = import! boulder.policy.v1
policy.policy [
    policy.layer "same" [],
    policy.layer "same" [],
]
"#,
        )
        .unwrap();
        assert!(matches!(
            Macros::load_from(root.path()),
            Err(Error::DuplicateLayerName { ref name }) if name == "same"
        ));
    }

    #[test]
    fn repository_macro_modules_all_evaluate() {
        let macros = Macros::repository_for_tests();

        assert_eq!(macros.actions.len(), 1);
        assert_eq!(
            macros.arch.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "aarch64",
                "base",
                "emul32/x86_64",
                "riscv64",
                "x86",
                "x86_64",
                "x86_64-stage1",
                "x86_64-v3x",
            ]
        );
        assert!(
            macros
                .actions
                .iter()
                .all(|macros| { !macros.actions.is_empty() || !macros.flags.is_empty() || !macros.tuning.is_empty() })
        );
        assert!(macros.arch.values().all(|macros| {
            !macros.actions.is_empty()
                || !macros.definitions.is_empty()
                || !macros.flags.is_empty()
                || !macros.tuning.is_empty()
        }));
        assert_eq!(macros.provenance.len(), 9);
        assert!(
            macros.provenance[..1]
                .iter()
                .all(|change| change.layer_name == "actions" && change.layer_order == 0)
        );
        assert!(
            macros.provenance[1..]
                .iter()
                .all(|change| change.layer_name == "architectures" && change.layer_order == 1)
        );
        assert_eq!(macros.selection("x86_64").changes.len(), 3);
        assert_eq!(macros.selection("x86_64").fingerprint, macros.fingerprint);
    }
}
