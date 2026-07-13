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
use stone_recipe::package::{PackageSpec, evaluate_gluon_with_inputs};
use thiserror::Error;

use crate::{
    architecture::{self, BuildTarget},
    source_lock::{self, SOURCE_LOCK_FILE_NAME, SourceLock},
};

pub type Parsed = stone_recipe::Recipe;

#[derive(Debug)]
pub struct Recipe {
    pub path: PathBuf,
    pub source: String,
    pub parsed: Parsed,
    /// Concrete package-v2 declaration produced by the authored factory.
    pub declaration: PackageSpec,
    pub source_lock: Option<SourceLock>,
    pub fingerprint: EvaluationFingerprint,
    pub build_time: DateTime<Utc>,
}

impl Recipe {
    /// Desired recipe value invariants are checked here
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, parsed, source_lock, fingerprint) =
            load_gluon(&path, SourceLockPolicy::RequireCurrent)?;

        Self::from_loaded(path, source, declaration, parsed, source_lock, fingerprint)
    }

    /// Load only the authored expression, ignoring any generated source lock.
    ///
    /// This is reserved for lock regeneration: stale or malformed generated
    /// state must not prevent Boulder from evaluating the authoritative source.
    pub(crate) fn load_authored(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, parsed, source_lock, fingerprint) = load_gluon(&path, SourceLockPolicy::Ignore)?;

        Self::from_loaded(path, source, declaration, parsed, source_lock, fingerprint)
    }

    fn from_loaded(
        path: PathBuf,
        source: String,
        declaration: PackageSpec,
        parsed: Parsed,
        source_lock: Option<SourceLock>,
        fingerprint: EvaluationFingerprint,
    ) -> Result<Self, Error> {
        let build_time = resolve_build_time(&path);

        parsed.validate()?;

        Ok(Self {
            path,
            source,
            parsed,
            declaration,
            source_lock,
            fingerprint,
            build_time,
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceLockPolicy {
    RequireCurrent,
    Ignore,
}

fn load_gluon(
    path: &Path,
    source_lock_policy: SourceLockPolicy,
) -> Result<(String, PackageSpec, Parsed, Option<SourceLock>, EvaluationFingerprint), Error> {
    let parent = path.parent().ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    let file_name = path.file_name().ok_or_else(|| Error::MissingRecipe(path.to_owned()))?;
    let source_root = SourceRoot::new(parent).map_err(Error::LoadGluonSource)?;
    let evaluator = Evaluator::default();
    let source = source_root
        .load(Path::new(file_name), evaluator.limits().max_source_bytes)
        .map_err(Error::LoadGluonSource)?;
    let evaluator = evaluator.with_source_root(source_root);

    let lock_path = path.with_file_name(SOURCE_LOCK_FILE_NAME);
    let (explicit_inputs, source_lock) = match source_lock_policy {
        SourceLockPolicy::Ignore => (Vec::new(), None),
        SourceLockPolicy::RequireCurrent => match fs::read(&lock_path) {
            Ok(bytes) => {
                let lock = source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &bytes).map_err(|source| {
                    Error::DecodeSourceLock {
                        path: lock_path.clone(),
                        source: Box::new(source),
                    }
                })?;
                (bytes, Some(lock))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), None),
            Err(source) => {
                return Err(Error::LoadSourceLock {
                    path: lock_path,
                    source,
                });
            }
        },
    };

    let evaluated = evaluate_gluon_with_inputs(&evaluator, &source, &explicit_inputs)?;
    if let Some(lock) = source_lock.as_ref() {
        lock.validate_against(&evaluated.recipe)
            .map_err(|source| Error::StaleSourceLock {
                path: lock_path,
                source: Box::new(source),
            })?;
    }
    Ok((
        source.text().to_owned(),
        evaluated.package,
        evaluated.recipe,
        source_lock,
        evaluated.fingerprint,
    ))
}

pub fn resolve_path(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    let path = path.as_ref();

    let path = if path.is_dir() {
        path.join("stone.glu")
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
    #[error("load Gluon recipe source")]
    LoadGluonSource(#[source] gluon_config::Diagnostic),
    #[error("load Gluon source lock {path:?}")]
    LoadSourceLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode Gluon source lock {path:?}")]
    DecodeSourceLock {
        path: PathBuf,
        #[source]
        source: Box<source_lock::DecodeError>,
    },
    #[error("stale Gluon source lock {path:?}")]
    StaleSourceLock {
        path: PathBuf,
        #[source]
        source: Box<source_lock::ValidationError>,
    },
    #[error("evaluate Gluon recipe")]
    EvaluateGluon(#[from] stone_recipe::package::PackageEvaluationError),
    #[error("invalid recipe")]
    Validation(#[from] stone_recipe::ValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_SPEC: &str = r#"{
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}"#;

    const ARCHIVE_URL: &str = "https://example.com/source.tar.xz";
    const ARCHIVE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GIT_URL: &str = "https://example.com/source.git";
    const GIT_REF: &str = "main";
    const FULL_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn gluon_recipe(source: &str) -> String {
        format!("let boulder = import! boulder.package.v2\nboulder.mk_package (boulder.meta {source})")
    }

    fn gluon_recipe_with_upstreams() -> String {
        format!(
            r#"let boulder = import! boulder.package.v2
let base = boulder.mk_package (boulder.meta {SOURCE_SPEC})
{{
    sources = [
        boulder.source.archive "{ARCHIVE_URL}" "{ARCHIVE_HASH}",
        boulder.source.git "{GIT_URL}" "{GIT_REF}",
    ],
    .. base
}}"#
        )
    }

    #[test]
    fn documented_recipe_examples_remain_loadable() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");

        let minimal = Recipe::load(examples.join("stone.glu")).unwrap();
        assert_eq!(minimal.parsed.source.name, "hello");

        let composed = Recipe::load(examples.join("composed-stone.glu")).unwrap();
        assert_eq!(composed.parsed.source.name, "composed-hello");
        assert_eq!(
            composed.parsed.package.summary.as_deref(),
            Some("Recipe composed with an imported policy")
        );
    }

    fn matching_source_lock() -> SourceLock {
        SourceLock::new(vec![
            source_lock::SourceResolution::Archive(source_lock::ArchiveResolution {
                order: 0,
                url: ARCHIVE_URL.to_owned(),
                sha256: ARCHIVE_HASH.to_owned(),
            }),
            source_lock::SourceResolution::Git(source_lock::GitResolution {
                order: 1,
                url: GIT_URL.to_owned(),
                requested_ref: GIT_REF.to_owned(),
                commit: FULL_COMMIT.to_owned(),
            }),
        ])
    }

    #[test]
    fn recipe_directory_resolves_only_stone_glu() {
        let root = tempfile::tempdir().unwrap();

        let missing = resolve_path(root.path()).unwrap_err();
        assert!(matches!(missing, Error::MissingRecipe(path) if path.ends_with("stone.glu")));

        let gluon = root.path().join("stone.glu");
        fs::write(&gluon, "{}").unwrap();
        assert_eq!(resolve_path(root.path()).unwrap(), gluon.canonicalize().unwrap());
    }

    #[test]
    fn explicit_gluon_file_loads_and_records_provenance() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("custom.glu");
        fs::write(&path, gluon_recipe(SOURCE_SPEC)).unwrap();

        let recipe = Recipe::load(&path).unwrap();

        assert_eq!(recipe.path, path.canonicalize().unwrap());
        assert_eq!(recipe.parsed.source.name, "example");
        assert!(recipe.source_lock.is_none());
        let fingerprint = recipe.fingerprint;
        assert_eq!(fingerprint.root_source_sha256.len(), 64);
        assert!(
            fingerprint
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "boulder.package.v2")
        );
    }

    #[test]
    fn directory_gluon_loads_contained_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.glu"), SOURCE_SPEC).unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let boulder = import! boulder.package.v2
let source = import! "source.glu"
boulder.mk_package (boulder.meta source)
"#,
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();
        let fingerprint = recipe.fingerprint;

        assert_eq!(recipe.path, root.path().join("stone.glu").canonicalize().unwrap());
        assert_eq!(recipe.parsed.source.version, "1.2.3");
        assert_eq!(
            fingerprint
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["boulder.package.v2", "source.glu", "std.array.prim", "std.types",]
        );
    }

    #[test]
    fn invalid_gluon_preserves_source_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let boulder = import! boulder.package.v2
boulder.mk_package (boulder.meta {
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = 42,
    license = ["MPL-2.0"],
})
"#,
        )
        .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::EvaluateGluon(stone_recipe::package::PackageEvaluationError::Evaluation(diagnostic)) = error else {
            panic!("unexpected error: {error}");
        };

        assert_eq!(diagnostic.category, gluon_config::DiagnosticCategory::Type);
        assert_eq!(diagnostic.source_name.as_deref(), Some("stone.glu"));
        assert!(diagnostic.span.is_some());
    }

    #[test]
    fn legacy_recipe_shape_is_not_an_implicit_fallback() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            format!("let source = {SOURCE_SPEC}\n{{ source }}"),
        )
        .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::EvaluateGluon(stone_recipe::package::PackageEvaluationError::Evaluation(_))
        ));
    }

    #[test]
    fn valid_source_lock_is_decoded_and_retained() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        fs::write(
            root.path().join(SOURCE_LOCK_FILE_NAME),
            source_lock::encode_source_lock(&matching_source_lock()),
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();

        assert_eq!(recipe.source_lock, Some(matching_source_lock()));
    }

    #[test]
    fn malformed_schema_and_commit_lock_errors_include_the_lock_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);

        let mut wrong_schema = matching_source_lock();
        wrong_schema.schema_version = 2;
        let mut short_commit = matching_source_lock();
        let source_lock::SourceResolution::Git(git) = &mut short_commit.sources[1] else {
            unreachable!();
        };
        git.commit = "abc123d".to_owned();

        let cases = [
            ("{".to_owned(), "evaluate source lock"),
            (source_lock::encode_source_lock(&wrong_schema), "unsupported schema"),
            (source_lock::encode_source_lock(&short_commit), "complete 40-hex"),
        ];

        for (contents, expected) in cases {
            fs::write(&lock_path, contents).unwrap();
            let error = Recipe::load(root.path()).unwrap_err();
            let Error::DecodeSourceLock { path, source } = error else {
                panic!("unexpected error: {error}");
            };
            assert_eq!(path, lock_path);
            assert!(source.to_string().contains(expected), "{source}");
        }
    }

    #[test]
    fn authored_load_ignores_generated_lock_bytes_without_mutating_them() {
        let root = tempfile::tempdir().unwrap();
        let recipe_path = root.path().join("stone.glu");
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        fs::write(&recipe_path, gluon_recipe(SOURCE_SPEC)).unwrap();
        fs::write(&lock_path, "not valid Gluon").unwrap();

        let recipe = Recipe::load_authored(root.path()).unwrap();

        assert_eq!(recipe.parsed.source.name, "example");
        assert!(recipe.source_lock.is_none());
        assert_eq!(fs::read_to_string(lock_path).unwrap(), "not valid Gluon");
    }

    #[test]
    fn stale_source_lock_rejects_count_kind_url_hash_and_requested_ref() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);

        let mut count = matching_source_lock();
        count.sources.pop();

        let mut kind = matching_source_lock();
        kind.sources[0] = source_lock::SourceResolution::Git(source_lock::GitResolution {
            order: 0,
            url: ARCHIVE_URL.to_owned(),
            requested_ref: "archive".to_owned(),
            commit: FULL_COMMIT.to_owned(),
        });

        let mut url = matching_source_lock();
        let source_lock::SourceResolution::Archive(archive) = &mut url.sources[0] else {
            unreachable!();
        };
        archive.url = "https://example.com/changed.tar.xz".to_owned();

        let mut hash = matching_source_lock();
        let source_lock::SourceResolution::Archive(archive) = &mut hash.sources[0] else {
            unreachable!();
        };
        archive.sha256 = "changed-hash".to_owned();

        let mut requested_ref = matching_source_lock();
        let source_lock::SourceResolution::Git(git) = &mut requested_ref.sources[1] else {
            unreachable!();
        };
        git.requested_ref = "v2".to_owned();

        for (lock, expected) in [
            (count, "stale source count"),
            (kind, "stale source kind"),
            (url, "sources[0].url"),
            (hash, "sources[0].sha256"),
            (requested_ref, "sources[1].requested_ref"),
        ] {
            fs::write(&lock_path, source_lock::encode_source_lock(&lock)).unwrap();
            let error = Recipe::load(root.path()).unwrap_err();
            let Error::StaleSourceLock { path, source } = error else {
                panic!("unexpected error: {error}");
            };
            assert_eq!(path, lock_path);
            assert!(source.to_string().contains(expected), "{source}");
        }
    }

    #[test]
    fn recipe_and_lock_fingerprints_are_deterministic() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        let lock = source_lock::encode_source_lock(&SourceLock::default());
        fs::write(root.path().join(SOURCE_LOCK_FILE_NAME), &lock).unwrap();

        let first = Recipe::load(root.path()).unwrap();
        let repeated = Recipe::load(root.path()).unwrap();

        assert_eq!(format!("{:?}", first.parsed), format!("{:?}", repeated.parsed));
        assert_eq!(first.source, repeated.source);
        assert_eq!(first.fingerprint, repeated.fingerprint);

        fs::write(
            root.path().join(SOURCE_LOCK_FILE_NAME),
            format!("{lock}// semantically inert fingerprint change\n"),
        )
        .unwrap();
        let changed = Recipe::load(root.path()).unwrap();
        let first_fingerprint = &first.fingerprint;
        let changed_fingerprint = &changed.fingerprint;

        assert_ne!(first_fingerprint.sha256, changed_fingerprint.sha256);
        assert_ne!(
            first_fingerprint.explicit_inputs_sha256,
            changed_fingerprint.explicit_inputs_sha256
        );
    }
}
