// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use fs_err as fs;
use gluon_config::{EvaluationFingerprint, Evaluator, SourceRoot};
use stone_recipe::{control_file, evaluate_gluon_with_inputs};
use thiserror::Error;
use tui::Styled;

use crate::{
    architecture::{self, BuildTarget},
    source_lock::SOURCE_LOCK_FILE_NAME,
};

pub type Parsed = stone_recipe::Recipe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Gluon,
    YamlCompatibility,
}

impl Format {
    fn from_path(path: &Path) -> Self {
        if path.extension().and_then(|extension| extension.to_str()) == Some("glu") {
            Self::Gluon
        } else {
            Self::YamlCompatibility
        }
    }
}

#[derive(Debug)]
pub struct Recipe {
    pub path: PathBuf,
    pub source: String,
    pub parsed: Parsed,
    pub fingerprint: Option<EvaluationFingerprint>,
    pub build_time: DateTime<Utc>,
    format: Format,
}

impl Recipe {
    /// Desired recipe value invariants are checked here
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let format = Format::from_path(&path);

        let (source, parsed, fingerprint) = match format {
            Format::Gluon => {
                let (source, parsed, fingerprint) = load_gluon(&path)?;
                (source, parsed, Some(fingerprint))
            }
            Format::YamlCompatibility => {
                eprintln!(
                    "{} | YAML recipe compatibility is deprecated and read-only; migrate {} to stone.glu",
                    "Warning".yellow(),
                    path.display()
                );
                let source = fs::read_to_string(&path).map_err(Error::LoadRecipe)?;
                let mut parsed = stone_recipe::from_str(&source)?;
                apply_legacy_control_file(&path, &mut parsed)?;
                (source, parsed, None)
            }
        };

        let build_time = resolve_build_time(&path);

        parsed.validate()?;

        Ok(Self {
            path,
            source,
            parsed,
            fingerprint,
            build_time,
            format,
        })
    }

    /// Whether this recipe came through the temporary, read-only YAML loader.
    #[must_use]
    pub fn is_yaml_compatibility(&self) -> bool {
        self.format == Format::YamlCompatibility
    }

    pub fn build_targets(&self) -> Vec<BuildTarget> {
        let host = architecture::host();
        let host_string = host.to_string();

        let mut targets = vec![];
        if self.parsed.architectures.is_empty() {
            if self.parsed.emul32 {
                targets.push(BuildTarget::Emul32(host));
            }

            targets.push(BuildTarget::Native(host));
        } else {
            let emul32 = BuildTarget::Emul32(host);
            let emul32_string = emul32.to_string();

            if self.parsed.architectures.contains(&emul32_string)
                || self.parsed.architectures.contains(&"emul32".into())
            {
                targets.push(emul32);
            }

            if self.parsed.architectures.contains(&host_string) || self.parsed.architectures.contains(&"native".into())
            {
                targets.push(BuildTarget::Native(host));
            }
        }

        targets
    }

    pub fn build_target_profile_key(&self, target: BuildTarget) -> Option<String> {
        let target_string = target.to_string();

        if self.parsed.profiles.iter().any(|kv| kv.key == target_string) {
            Some(target_string)
        } else if target.emul32() && self.parsed.profiles.iter().any(|kv| &kv.key == "emul32") {
            Some("emul32".to_owned())
        } else {
            None
        }
    }

    pub fn build_target_definition(&self, target: BuildTarget) -> &stone_recipe::Build {
        let key = self.build_target_profile_key(target);

        if let Some(profile) = self.parsed.profiles.iter().find(|kv| Some(&kv.key) == key.as_ref()) {
            &profile.value
        } else {
            &self.parsed.build
        }
    }
}

fn load_gluon(path: &Path) -> Result<(String, Parsed, EvaluationFingerprint), Error> {
    let parent = path.parent().ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    let file_name = path.file_name().ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    let source_root = SourceRoot::new(parent).map_err(Error::LoadGluonSource)?;
    let evaluator = Evaluator::default();
    let source = source_root
        .load(Path::new(file_name), evaluator.limits().max_source_bytes)
        .map_err(Error::LoadGluonSource)?;
    let evaluator = evaluator.with_source_root(source_root);

    let lock_path = path.with_file_name(SOURCE_LOCK_FILE_NAME);
    let explicit_inputs = match fs::read(&lock_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(Error::LoadSourceLock {
                path: lock_path,
                source,
            });
        }
    };

    let evaluated = evaluate_gluon_with_inputs(&evaluator, &source, &explicit_inputs)?;
    Ok((source.text().to_owned(), evaluated.recipe, evaluated.fingerprint))
}

fn apply_legacy_control_file(path: &Path, parsed: &mut Parsed) -> Result<(), Error> {
    let control_file_path = path.with_file_name("control.kdl");
    if !control_file_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&control_file_path).map_err(Error::LoadControlFile)?;
    let control_file =
        control_file::decode(&content).map_err(|error| Error::DecodeControlFile(error, control_file_path.clone()))?;

    control_file
        .apply_to_recipe(parsed)
        .map_err(|error| Error::ApplyControlFile(error, control_file_path.clone()))?;

    println!(
        "{} | Applied modifications from {control_file_path:?}",
        "Control File".green()
    );
    Ok(())
}

pub fn resolve_path(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    let path = path.as_ref();

    // Resolve a recipe directory without silently shadowing either format
    // during the short YAML compatibility window.
    let path = if path.is_dir() {
        let gluon = path.join("stone.glu");
        let yaml = path.join("stone.yaml");

        match (gluon.exists(), yaml.exists()) {
            (true, false) => gluon,
            (false, true) => yaml,
            (true, true) => return Err(Error::AmbiguousRecipe { gluon, yaml }),
            (false, false) => gluon,
        }
    } else {
        path.to_path_buf()
    };

    // Ensure it's absolute & exists
    fs::canonicalize(&path).map_err(|_| Error::MissingRecipe(path))
}

fn resolve_build_time(path: &Path) -> DateTime<Utc> {
    // Propagate SOURCE_DATE_EPOCH if set
    if let Ok(epoch_env) = env::var("SOURCE_DATE_EPOCH")
        && let Ok(parsed) = epoch_env.parse::<i64>()
        && let Some(timestamp) = DateTime::from_timestamp(parsed, 0)
    {
        return timestamp;
    }

    // If we are building from a git repo and have the git binary available to us then use the last commit timestamp
    if let Some(recipe_dir) = path.parent()
        && let Ok(git_log) = Command::new("git")
            .args(["log", "-1", "--format=\"%at\""])
            .current_dir(recipe_dir)
            .output()
        && let Ok(stdout) = String::from_utf8(git_log.stdout)
        && let Ok(parsed) = stdout.replace(['\n', '"'], "").parse::<i64>()
        && let Some(timestamp) = DateTime::from_timestamp(parsed, 0)
    {
        return timestamp;
    }

    // As a final fallback use the current time
    Utc::now()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("recipe file does not exist: {0:?}")]
    MissingRecipe(PathBuf),
    #[error("recipe directory contains both {gluon:?} and {yaml:?}")]
    AmbiguousRecipe { gluon: PathBuf, yaml: PathBuf },
    #[error("load recipe")]
    LoadRecipe(#[source] io::Error),
    #[error("load Gluon recipe source")]
    LoadGluonSource(#[source] gluon_config::Diagnostic),
    #[error("load Gluon source lock {path:?}")]
    LoadSourceLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("evaluate Gluon recipe")]
    EvaluateGluon(#[from] stone_recipe::RecipeEvaluationError),
    #[error("load control file")]
    LoadControlFile(#[source] io::Error),
    #[error("decode recipe")]
    Decode(#[from] stone_recipe::Error),
    #[error("invalid recipe")]
    Validation(#[from] stone_recipe::ValidationError),
    #[error("failed to decode control file {1:?}")]
    DecodeControlFile(#[source] control_file::decode::Error, PathBuf),
    #[error("failed to modify recipe with control file {1:?}")]
    ApplyControlFile(#[source] control_file::ModificationError, PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_SPEC: &str = r#"{
    name = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}"#;

    const YAML_RECIPE: &str = r#"
name: example
version: "1.2.3"
release: 1
homepage: https://example.com
license: MPL-2.0
"#;

    fn gluon_recipe(source: &str) -> String {
        format!("let boulder = import! boulder.recipe.v1\nboulder.recipe (boulder.source {source})")
    }

    #[test]
    fn recipe_directory_resolves_each_format_without_shadowing() {
        let root = tempfile::tempdir().unwrap();

        let missing = resolve_path(root.path()).unwrap_err();
        assert!(matches!(missing, Error::MissingRecipe(path) if path.ends_with("stone.glu")));

        let yaml = root.path().join("stone.yaml");
        fs::write(&yaml, "name: compatibility").unwrap();
        assert_eq!(resolve_path(root.path()).unwrap(), yaml.canonicalize().unwrap());

        fs::remove_file(&yaml).unwrap();
        let gluon = root.path().join("stone.glu");
        fs::write(&gluon, "{}").unwrap();
        assert_eq!(resolve_path(root.path()).unwrap(), gluon.canonicalize().unwrap());
    }

    #[test]
    fn recipe_directory_rejects_ambiguous_formats() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), "{}").unwrap();
        fs::write(root.path().join("stone.yaml"), "name: compatibility").unwrap();

        let error = resolve_path(root.path()).unwrap_err();

        assert!(matches!(error, Error::AmbiguousRecipe { .. }));
    }

    #[test]
    fn explicit_gluon_file_loads_and_records_provenance() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("custom.glu");
        fs::write(&path, gluon_recipe(SOURCE_SPEC)).unwrap();

        let recipe = Recipe::load(&path).unwrap();

        assert_eq!(recipe.path, path.canonicalize().unwrap());
        assert_eq!(recipe.parsed.source.name, "example");
        assert!(!recipe.is_yaml_compatibility());
        let fingerprint = recipe.fingerprint.unwrap();
        assert_eq!(fingerprint.root_source_sha256.len(), 64);
        assert!(
            fingerprint
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "boulder.recipe.v1")
        );
    }

    #[test]
    fn directory_gluon_loads_contained_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.glu"), SOURCE_SPEC).unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let boulder = import! boulder.recipe.v1
let source = import! "source.glu"
boulder.recipe (boulder.source source)
"#,
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();
        let fingerprint = recipe.fingerprint.unwrap();

        assert_eq!(recipe.path, root.path().join("stone.glu").canonicalize().unwrap());
        assert_eq!(recipe.parsed.source.version, "1.2.3");
        assert_eq!(
            fingerprint
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["boulder.recipe.v1", "source.glu"]
        );
    }

    #[test]
    fn yaml_directory_remains_legacy_read_only_compatibility() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.yaml"), YAML_RECIPE).unwrap();
        fs::write(
            root.path().join("control.kdl"),
            r#"
override {
    setup "controlled"
}
"#,
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();

        assert!(recipe.is_yaml_compatibility());
        assert!(recipe.fingerprint.is_none());
        assert_eq!(recipe.parsed.build.setup.as_deref(), Some("controlled"));
    }

    #[test]
    fn gluon_never_applies_legacy_control_file() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        fs::write(root.path().join("control.kdl"), "not valid kdl {").unwrap();

        let recipe = Recipe::load(root.path()).unwrap();

        assert!(recipe.parsed.build.setup.is_none());
    }

    #[test]
    fn invalid_gluon_preserves_source_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let boulder = import! boulder.recipe.v1
boulder.recipe (boulder.source {
    name = "example",
    version = "1.2.3",
    release = 1,
    homepage = 42,
    license = ["MPL-2.0"],
})
"#,
        )
        .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::EvaluateGluon(stone_recipe::RecipeEvaluationError::Evaluation(diagnostic)) = error else {
            panic!("unexpected error: {error}");
        };

        assert_eq!(diagnostic.category, gluon_config::DiagnosticCategory::Type);
        assert_eq!(diagnostic.source_name.as_deref(), Some("stone.glu"));
        assert!(diagnostic.span.is_some());
    }

    #[test]
    fn recipe_and_lock_fingerprints_are_deterministic() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        fs::write(root.path().join(SOURCE_LOCK_FILE_NAME), "lock-v1").unwrap();

        let first = Recipe::load(root.path()).unwrap();
        let repeated = Recipe::load(root.path()).unwrap();

        assert_eq!(format!("{:?}", first.parsed), format!("{:?}", repeated.parsed));
        assert_eq!(first.source, repeated.source);
        assert_eq!(first.fingerprint, repeated.fingerprint);

        fs::write(root.path().join(SOURCE_LOCK_FILE_NAME), "lock-v2").unwrap();
        let changed = Recipe::load(root.path()).unwrap();
        let first_fingerprint = first.fingerprint.as_ref().unwrap();
        let changed_fingerprint = changed.fingerprint.as_ref().unwrap();

        assert_ne!(first_fingerprint.sha256, changed_fingerprint.sha256);
        assert_ne!(
            first_fingerprint.explicit_inputs_sha256,
            changed_fingerprint.explicit_inputs_sha256
        );
    }
}
