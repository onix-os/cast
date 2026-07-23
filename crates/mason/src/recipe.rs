// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs_err as fs;
use gluon_config::{EvaluationFingerprint, Evaluator, SourceRoot};
use stone_recipe::build_policy::{TargetEmulationSpec, TargetPolicySpec};
use stone_recipe::package::{BuilderSpec, HooksSpec, PackageSpec, PhasesSpec, ProfileSpec, evaluate_gluon_with_inputs};
use thiserror::Error;

use crate::{
    generated_lock,
    source_lock::{self, SOURCE_LOCK_FILE_NAME, SourceLock},
};

#[derive(Debug)]
pub struct Recipe {
    pub path: PathBuf,
    pub source: String,
    /// Concrete package-v3 declaration produced by the authored factory.
    pub declaration: PackageSpec,
    pub source_lock: Option<SourceLock>,
    pub fingerprint: EvaluationFingerprint,
    pub build_time: DateTime<Utc>,
}

impl Recipe {
    /// Desired recipe value invariants are checked here
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) = load_gluon(&path, SourceLockPolicy::RequireCurrent)?;

        Self::from_loaded(path, source, declaration, source_lock, fingerprint, None)
    }

    /// Load using an explicit reproducible build timestamp. No process
    /// environment, Git metadata, or clock fallback participates.
    pub(crate) fn load_at(path: impl AsRef<Path>, build_time: DateTime<Utc>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) = load_gluon(&path, SourceLockPolicy::RequireCurrent)?;
        Self::from_loaded(path, source, declaration, source_lock, fingerprint, Some(build_time))
    }

    /// Load only the authored expression, ignoring any generated source lock.
    ///
    /// This is reserved for lock regeneration: stale or malformed generated
    /// state must not prevent Cast from evaluating the authoritative source.
    pub(crate) fn load_authored(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = resolve_path(path)?;
        let (source, declaration, source_lock, fingerprint) = load_gluon(&path, SourceLockPolicy::Ignore)?;

        Self::from_loaded(path, source, declaration, source_lock, fingerprint, None)
    }

    fn from_loaded(
        path: PathBuf,
        source: String,
        declaration: PackageSpec,
        source_lock: Option<SourceLock>,
        fingerprint: EvaluationFingerprint,
        explicit_build_time: Option<DateTime<Utc>>,
    ) -> Result<Self, Error> {
        let build_time = explicit_build_time
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("the Unix epoch is a valid UTC timestamp"));

        Ok(Self {
            path,
            source,
            declaration,
            source_lock,
            fingerprint,
            build_time,
        })
    }

    /// Whether this recipe opts into one exact repository build target.
    ///
    /// An empty architecture list means every native policy target, plus
    /// explicitly enabled compatibility targets. Once a recipe declares an
    /// architecture list, only an exact target name or the typed `native` and
    /// `emul32` classes match. Target-name prefixes and host architecture are
    /// deliberately not inferred here.
    pub fn supports_target(&self, target: &TargetPolicySpec) -> bool {
        if self.declaration.architectures.is_empty() {
            matches!(&target.emulation, TargetEmulationSpec::Native) || self.declaration.emul32
        } else {
            self.declaration.architectures.iter().any(|declared| {
                declared == &target.name
                    || matches!(
                        (declared.as_str(), &target.emulation),
                        ("native", TargetEmulationSpec::Native) | ("emul32", TargetEmulationSpec::Emul32 { .. })
                    )
            })
        }
    }

    pub fn build_target_profile_key(&self, target: &TargetPolicySpec) -> Option<&str> {
        if let Some(profile) = self
            .declaration
            .profiles
            .iter()
            .find(|profile| profile.name == target.name)
        {
            Some(&profile.name)
        } else if matches!(&target.emulation, TargetEmulationSpec::Emul32 { .. }) {
            self.declaration
                .profiles
                .iter()
                .find(|profile| profile.name == "emul32")
                .map(|profile| profile.name.as_str())
        } else {
            None
        }
    }

    pub fn build_target_profile(&self, target: &TargetPolicySpec) -> Option<&ProfileSpec> {
        let key = self.build_target_profile_key(target)?;
        self.declaration.profiles.iter().find(|profile| profile.name == key)
    }

    /// Select the structural package-v3 builder for one target.
    pub fn build_target_builder(&self, target: &TargetPolicySpec) -> &BuilderSpec {
        self.declaration
            .builder_for_profile(self.build_target_profile_key(target))
    }

    /// Select the package-v3 hooks paired with the target's structural builder.
    pub fn build_target_hooks(&self, target: &TargetPolicySpec) -> &HooksSpec {
        self.declaration
            .hooks_for_profile(self.build_target_profile_key(target))
    }

    /// Select the structural package-v3 phases for one target.
    pub fn build_target_phases(&self, target: &TargetPolicySpec) -> PhasesSpec {
        self.declaration
            .phases_for_profile(self.build_target_profile_key(target))
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
) -> Result<(String, PackageSpec, Option<SourceLock>, EvaluationFingerprint), Error> {
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
        SourceLockPolicy::RequireCurrent => match generated_lock::read(&lock_path) {
            Ok(bytes) => {
                let lock = source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &bytes).map_err(|source| {
                    Error::DecodeSourceLock {
                        path: lock_path.clone(),
                        source: Box::new(source),
                    }
                })?;
                (bytes, Some(lock))
            }
            Err(error) if error.is_not_found() => (Vec::new(), None),
            Err(source) => {
                return Err(Error::LoadSourceLock {
                    path: lock_path,
                    source: Box::new(source),
                });
            }
        },
    };

    let evaluated = evaluate_gluon_with_inputs(&evaluator, &source, &explicit_inputs)?;
    if let Some(lock) = source_lock.as_ref() {
        lock.validate_against(&evaluated.package.sources)
            .map_err(|source| Error::StaleSourceLock {
                path: lock_path,
                source: Box::new(source),
            })?;
    }
    Ok((
        source.text().to_owned(),
        evaluated.package,
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
        source: Box<generated_lock::ReadError>,
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
}

#[cfg(test)]
mod tests {
    use stone_recipe::package::{BuilderSpec, HooksSpec, ProfileSpec};

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
    const MATERIALIZATION_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn gluon_recipe(source: &str) -> String {
        format!("let cast = import! cast.package.v3\ncast.mk_package (cast.meta {source})")
    }

    fn minimal_recipe() -> Recipe {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        Recipe::load(root.path()).unwrap()
    }

    fn target(name: &str) -> TargetPolicySpec {
        crate::BuildPolicy::repository_for_tests().target(name).unwrap().clone()
    }

    fn profile(name: &str) -> ProfileSpec {
        ProfileSpec {
            name: name.to_owned(),
            builder: BuilderSpec::default(),
            hooks: HooksSpec::default(),
            native_build_inputs: Vec::new(),
            build_inputs: Vec::new(),
            check_inputs: Vec::new(),
        }
    }

    fn gluon_recipe_with_upstreams() -> String {
        format!(
            r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {SOURCE_SPEC})
{{
    sources = [
        cast.source.archive "{ARCHIVE_URL}" "{ARCHIVE_HASH}",
        cast.source.git "{GIT_URL}" "{GIT_REF}",
    ],
    .. base
}}"#
        )
    }

    #[test]
    fn documented_recipe_examples_remain_loadable() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon");

        let minimal = Recipe::load(examples.join("stone.glu")).unwrap();
        assert_eq!(minimal.declaration.meta.pname, "hello");

        let composed = Recipe::load(examples.join("composed-stone.glu")).unwrap();
        assert_eq!(composed.declaration.meta.pname, "composed-hello");
        assert_eq!(
            composed.declaration.outputs[0].summary.as_deref(),
            Some("Recipe composed with an imported policy")
        );
    }

    #[test]
    fn empty_architecture_list_supports_all_native_policy_targets_and_gates_emul32() {
        let mut recipe = minimal_recipe();
        let x86_64 = target("x86_64");
        let x86_64_v3x = target("x86_64-v3x");
        let emul32 = target("emul32/x86_64");

        assert!(recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.emul32 = true;
        assert!(recipe.supports_target(&emul32));
    }

    #[test]
    fn declared_architectures_match_exact_policy_names_or_typed_classes_only() {
        let mut recipe = minimal_recipe();
        let x86_64 = target("x86_64");
        let x86_64_v3x = target("x86_64-v3x");
        let emul32 = target("emul32/x86_64");

        recipe.declaration.architectures = vec!["x86_64".to_owned()];
        assert!(recipe.supports_target(&x86_64));
        assert!(!recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["native".to_owned()];
        assert!(recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&x86_64_v3x));
        assert!(!recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["emul32".to_owned()];
        assert!(!recipe.supports_target(&x86_64));
        assert!(recipe.supports_target(&emul32));

        recipe.declaration.architectures = vec!["emul32/x86_64".to_owned()];
        assert!(recipe.supports_target(&emul32));
    }

    #[test]
    fn target_profiles_prefer_exact_policy_name_then_typed_emul32_fallback() {
        let mut recipe = minimal_recipe();
        let target = target("emul32/x86_64");
        recipe.declaration.profiles = vec![profile("emul32"), profile("emul32/x86_64")];

        assert_eq!(recipe.build_target_profile_key(&target), Some("emul32/x86_64"));
        assert_eq!(recipe.build_target_profile(&target).unwrap().name, "emul32/x86_64");

        recipe.declaration.profiles.pop();
        assert_eq!(recipe.build_target_profile_key(&target), Some("emul32"));
        assert_eq!(recipe.build_target_profile(&target).unwrap().name, "emul32");
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
                materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
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
        assert_eq!(recipe.declaration.meta.pname, "example");
        assert!(recipe.source_lock.is_none());
        let fingerprint = recipe.fingerprint;
        assert_eq!(fingerprint.root_source_sha256.len(), 64);
        assert!(
            fingerprint
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "cast.package.v3")
        );
    }

    #[test]
    fn directory_gluon_loads_contained_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.glu"), SOURCE_SPEC).unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let cast = import! cast.package.v3
let source = import! "source.glu"
cast.mk_package (cast.meta source)
"#,
        )
        .unwrap();

        let recipe = Recipe::load(root.path()).unwrap();
        let fingerprint = recipe.fingerprint;

        assert_eq!(recipe.path, root.path().join("stone.glu").canonicalize().unwrap());
        assert_eq!(recipe.declaration.meta.version, "1.2.3");
        assert_eq!(
            fingerprint
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            [
                "cast.package.v3",
                "source.glu",
                "std.array.prim",
                "std.string.prim",
                "std.types",
            ]
        );
    }

    #[test]
    fn invalid_gluon_preserves_source_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            r#"
let cast = import! cast.package.v3
cast.mk_package (cast.meta {
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
    fn oversized_and_symlinked_source_locks_are_rejected_structurally() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        let limit = gluon_config::Limits::default().max_source_bytes;
        fs::File::create(&lock_path)
            .unwrap()
            .set_len(u64::try_from(limit).unwrap() + 1)
            .unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::LoadSourceLock { source, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert!(matches!(
            *source,
            generated_lock::ReadError::TooLarge { limit: found, .. } if found == limit
        ));

        fs::remove_file(&lock_path).unwrap();
        let target = root.path().join("lock-target");
        fs::write(&target, source_lock::encode_source_lock(&matching_source_lock())).unwrap();
        symlink(&target, &lock_path).unwrap();

        let error = Recipe::load(root.path()).unwrap_err();
        let Error::LoadSourceLock { source, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert!(matches!(
            *source,
            generated_lock::ReadError::NotRegular {
                kind: generated_lock::FileKind::Symlink,
                ..
            }
        ));
    }

    #[test]
    fn malformed_schema_and_commit_lock_errors_include_the_lock_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe_with_upstreams()).unwrap();
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);

        let mut wrong_schema = matching_source_lock();
        wrong_schema.schema_version = 3;
        let mut short_commit = matching_source_lock();
        let source_lock::SourceResolution::Git(git) = &mut short_commit.sources[1] else {
            unreachable!();
        };
        git.commit = "abc123d".to_owned();

        let cases = [
            ("{".to_owned(), "evaluate source lock"),
            (source_lock::encode_source_lock(&wrong_schema), "unsupported schema"),
            (
                source_lock::encode_source_lock(&short_commit),
                "exactly 40 lowercase hexadecimal",
            ),
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

        assert_eq!(recipe.declaration.meta.pname, "example");
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
            materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
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
        archive.sha256 = "b".repeat(64);

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

        assert_eq!(first.declaration, repeated.declaration);
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

    #[test]
    fn explicit_build_timestamp_is_deterministic_and_ambient_free() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), gluon_recipe(SOURCE_SPEC)).unwrap();
        let timestamp = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let first = Recipe::load_at(root.path(), timestamp).unwrap();
        let repeated = Recipe::load_at(root.path(), timestamp).unwrap();
        let defaulted = Recipe::load(root.path()).unwrap();

        assert_eq!(first.build_time, repeated.build_time);
        assert_eq!(first.build_time.timestamp(), 1_700_000_000);
        assert_eq!(defaulted.build_time.timestamp(), 0);
    }
}
