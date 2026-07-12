// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::{
    io,
    path::{Path, PathBuf},
};

use gluon_config::{Diagnostic, Evaluator, SourceRoot};
use moss::util;
use thiserror::Error;

use crate::Env;

#[derive(Debug)]
pub struct Macros {
    pub arch: BTreeMap<String, stone_recipe::Macros>,
    pub actions: Vec<stone_recipe::Macros>,
}

impl Macros {
    pub fn load(env: &Env) -> Result<Self, Error> {
        let macros_dir = env.data_dir.join("macros");
        Self::load_from(&macros_dir)
    }

    fn load_from(macros_dir: &Path) -> Result<Self, Error> {
        let actions_dir = macros_dir.join("actions");
        let arch_dir = macros_dir.join("arch");

        let matcher = |p: &Path| p.extension().and_then(|s| s.to_str()) == Some("glu");

        let mut arch_files = util::enumerate_files(&arch_dir, matcher).map_err(|source| Error::ArchFiles {
            path: arch_dir.clone(),
            source,
        })?;
        let mut action_files = util::enumerate_files(&actions_dir, matcher).map_err(|source| Error::ActionFiles {
            path: actions_dir.clone(),
            source,
        })?;
        arch_files.sort();
        action_files.sort();

        let source_root = SourceRoot::new(macros_dir).map_err(|source| Error::SourceRoot {
            path: macros_dir.to_path_buf(),
            source,
        })?;
        let evaluator = Evaluator::default().with_source_root(source_root.clone());

        let mut arch = BTreeMap::new();
        let mut actions = vec![];

        for file in arch_files {
            let relative = file.strip_prefix(&arch_dir).unwrap_or_else(|_| unreachable!());

            let identifier = relative.with_extension("").to_string_lossy().replace('\\', "/");

            let macros = evaluate_file(&evaluator, &source_root, macros_dir, &file)?;

            arch.insert(identifier, macros);
        }

        for file in action_files {
            let macros = evaluate_file(&evaluator, &source_root, macros_dir, &file)?;

            actions.push(macros);
        }

        Ok(Self { arch, actions })
    }
}

fn evaluate_file(
    evaluator: &Evaluator,
    source_root: &SourceRoot,
    macros_dir: &Path,
    path: &Path,
) -> Result<stone_recipe::Macros, Error> {
    let relative = path.strip_prefix(macros_dir).map_err(|_| Error::OutsideSourceRoot {
        path: path.to_path_buf(),
        root: macros_dir.to_path_buf(),
    })?;
    let source = source_root
        .load(relative, evaluator.limits().max_source_bytes)
        .map_err(|source| Error::Load {
            path: path.to_path_buf(),
            source,
        })?;
    stone_recipe::evaluate_macros_gluon_with(evaluator, &source)
        .map(|evaluated| evaluated.macros)
        .map_err(|source| Error::Evaluate {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("enumerate architecture macro Gluon modules in {path:?}")]
    ArchFiles {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("enumerate action macro Gluon modules in {path:?}")]
    ActionFiles {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare macro Gluon source root {path:?}")]
    SourceRoot {
        path: PathBuf,
        #[source]
        source: Diagnostic,
    },
    #[error("macro Gluon module {path:?} is outside source root {root:?}")]
    OutsideSourceRoot { path: PathBuf, root: PathBuf },
    #[error("load macro Gluon module {path:?}")]
    Load {
        path: PathBuf,
        #[source]
        source: Diagnostic,
    },
    #[error("evaluate macro Gluon module {path:?}")]
    Evaluate {
        path: PathBuf,
        #[source]
        source: stone_recipe::MacrosEvaluationError,
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

    #[test]
    fn loads_only_gluon_in_sorted_merge_order() {
        let root = layout();
        fs::write(root.path().join("actions/z.glu"), action("z")).unwrap();
        fs::write(root.path().join("actions/a.glu"), action("a")).unwrap();
        fs::write(root.path().join("actions/ignored.yaml"), "not: [valid").unwrap();
        fs::write(root.path().join("arch/base.glu"), EMPTY).unwrap();
        fs::write(root.path().join("arch/emul32/x86_64.glu"), EMPTY).unwrap();

        let macros = Macros::load_from(root.path()).unwrap();

        assert_eq!(
            macros
                .actions
                .iter()
                .map(|macros| macros.actions[0].key.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            macros.arch.keys().map(String::as_str).collect::<Vec<_>>(),
            ["base", "emul32/x86_64"]
        );
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
        fs::write(root.path().join("arch/base.glu"), EMPTY).unwrap();

        let macros = Macros::load_from(root.path()).unwrap();

        assert_eq!(macros.actions.len(), 2);
        assert!(macros.actions.iter().all(|macros| macros.actions[0].key == "shared"));
    }

    #[test]
    fn reports_the_path_of_invalid_gluon() {
        let root = layout();
        let invalid = root.path().join("actions/bad.glu");
        fs::write(&invalid, "this is not gluon").unwrap();
        fs::write(root.path().join("arch/base.glu"), EMPTY).unwrap();

        let error = Macros::load_from(root.path()).unwrap_err();

        assert!(matches!(error, Error::Evaluate { ref path, .. } if path == &invalid));
        assert!(error.to_string().contains("actions/bad.glu"));
    }

    #[test]
    fn repository_macro_modules_all_evaluate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/macros");

        let macros = Macros::load_from(&root).unwrap();

        assert_eq!(macros.actions.len(), 14);
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
        assert!(macros.actions.iter().all(|macros| !macros.actions.is_empty()));
        assert!(macros.arch.values().all(|macros| {
            !macros.actions.is_empty()
                || !macros.definitions.is_empty()
                || !macros.flags.is_empty()
                || !macros.tuning.is_empty()
                || !macros.packages.is_empty()
        }));
    }
}
