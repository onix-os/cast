// SPDX-FileCopyrightText: 2025 AerynOS Developers

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use config::declaration::{
    LoadFixedRootDeclarationError, RootDeclarationDiscoveryError,
    RootDeclarationSlot, TypedDeclarationEvaluatorSet,
    load_fixed_root_declaration,
};
use declarative_config::{DeclarationCodec, DeclarationEvaluator, Source};
use gluon_config::EvaluationFingerprint;
use thiserror::Error;

use crate::{Package, dependency, repository};

pub mod gluon;
mod rooted;
pub mod spec;

#[cfg(test)]
pub(crate) use rooted::arm_after_rooted_system_source_retained;
pub(crate) use rooted::load_rooted;

/// User-authored desired system intent, relative to an installation root.
pub const SYSTEM_INTENT_PATH: &str = "etc/cast/system.glu";

/// Cast-generated normalized state snapshot, relative to a state root.
pub const SYSTEM_SNAPSHOT_PATH: &str = "usr/lib/system-model.glu";

const SOURCE_FINGERPRINT_PREFIX: &str = "// Authored source fingerprint: ";

pub fn intent_path(root: &Path) -> PathBuf {
    root.join(SYSTEM_INTENT_PATH)
}

pub fn snapshot_path(root: &Path) -> PathBuf {
    root.join(SYSTEM_SNAPSHOT_PATH)
}

pub(super) struct SystemParts {
    pub disable_warning: bool,
    pub repositories: repository::Map,
    pub packages: BTreeSet<dependency::Provider>,
}

#[derive(Debug, Clone)]
pub struct SystemModel {
    pub disable_warning: bool,
    pub repositories: repository::Map,
    pub packages: BTreeSet<dependency::Provider>,
    generated_snapshot: String,
    fingerprint: EvaluationFingerprint,
    source_fingerprint: Option<String>,
}

impl SystemModel {
    /// Canonical generated Gluon snapshot for state storage and export.
    pub fn encoded(&self) -> &str {
        &self.generated_snapshot
    }

    pub fn fingerprint(&self) -> &EvaluationFingerprint {
        &self.fingerprint
    }

    /// Evaluation fingerprint of the authored intent from which this snapshot
    /// was derived, when the state was created from authored intent.
    pub fn source_fingerprint(&self) -> Option<&str> {
        self.source_fingerprint.as_deref()
    }

    pub(super) fn from_generated(
        parts: SystemParts,
        generated_snapshot: String,
        fingerprint: EvaluationFingerprint,
    ) -> Self {
        Self {
            disable_warning: parts.disable_warning,
            repositories: parts.repositories,
            packages: parts.packages,
            source_fingerprint: embedded_source_fingerprint(&generated_snapshot),
            generated_snapshot,
            fingerprint,
        }
    }

    pub(super) fn regenerate(parts: SystemParts) -> Result<Self, gluon::EvaluationError> {
        let normalized = spec::from_domain(parts.disable_warning, &parts.repositories, &parts.packages)?;
        let generated = spec::encode_generated_gluon(&normalized);
        gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", generated))
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSystemModel {
    pub disable_warning: bool,
    pub repositories: repository::Map,
    pub packages: BTreeSet<dependency::Provider>,
    provenance: Box<LoadedProvenance>,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct LoadedProvenance {
    authored_source: String,
    authored_fingerprint: EvaluationFingerprint,
    generated_snapshot: String,
    generated_fingerprint: EvaluationFingerprint,
    source_fingerprint: Option<String>,
}

impl LoadedSystemModel {
    /// Original authored source retained byte-for-byte for diagnostics and
    /// source-change suggestions.
    pub fn encoded(&self) -> &str {
        &self.provenance.authored_source
    }

    pub fn authored_source(&self) -> &str {
        &self.provenance.authored_source
    }

    pub fn fingerprint(&self) -> &EvaluationFingerprint {
        &self.provenance.authored_fingerprint
    }

    pub fn generated_snapshot(&self) -> &str {
        &self.provenance.generated_snapshot
    }

    pub fn generated_fingerprint(&self) -> &EvaluationFingerprint {
        &self.provenance.generated_fingerprint
    }

    pub fn source_fingerprint(&self) -> Option<&str> {
        self.provenance.source_fingerprint.as_deref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TryFrom<LoadedSystemModel> for SystemModel {
    type Error = gluon::EvaluationError;

    fn try_from(system_model: LoadedSystemModel) -> Result<Self, Self::Error> {
        let provenance = *system_model.provenance;

        let model = SystemModel {
            disable_warning: system_model.disable_warning,
            repositories: system_model.repositories,
            packages: system_model.packages,
            generated_snapshot: provenance.generated_snapshot,
            fingerprint: provenance.generated_fingerprint,
            source_fingerprint: None,
        };
        match provenance.source_fingerprint {
            Some(fingerprint) => model.with_source_fingerprint(fingerprint),
            None => Ok(model),
        }
    }
}

/// Load and evaluate authored intent or a generated Gluon snapshot.
pub fn load(path: &Path) -> Result<Option<LoadedSystemModel>, LoadError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| LoadError::InvalidPath(path.to_owned()))?;
    let basename = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| LoadError::InvalidPath(path.to_owned()))?;
    let evaluator = gluon::SystemIntentEvaluator::default();
    if path.extension().and_then(OsStr::to_str)
        != Some(evaluator.language_spec().extension())
    {
        return Err(LoadError::InvalidPath(path.to_owned()));
    }
    let slot = RootDeclarationSlot::new(basename, file_name)
        .map_err(|_| LoadError::InvalidPath(path.to_owned()))?;
    let evaluators = TypedDeclarationEvaluatorSet::new([evaluator])
        .expect("one validated system-intent adapter has no extension collision");
    let loaded = load_fixed_root_declaration(parent, &slot, &evaluators)?;

    Ok(loaded.map(|loaded| {
        loaded_from_declaration(path, loaded.value, loaded.identity)
    }))
}

fn load_source(
    path: &Path,
    source: Source,
    evaluator: &gluon::SystemIntentEvaluator,
) -> Result<LoadedSystemModel, LoadError> {
    let evaluated = evaluator.evaluate(&source).map_err(gluon::EvaluationError::from)?;
    Ok(loaded_from_declaration(
        path,
        evaluated.value,
        evaluated.identity,
    ))
}

fn loaded_from_declaration(
    path: &Path,
    declaration: gluon::SystemIntentDeclaration,
    authored_fingerprint: EvaluationFingerprint,
) -> LoadedSystemModel {
    let authored_source = declaration.authored_source;
    let SystemModel {
        disable_warning,
        repositories,
        packages,
        generated_snapshot,
        fingerprint: generated_fingerprint,
        ..
    } = declaration.model;
    let source_fingerprint = if authored_source.starts_with(spec::GENERATED_GLUON_MARKER) {
        embedded_source_fingerprint(&authored_source)
    } else {
        Some(authored_fingerprint.sha256.clone())
    };

    LoadedSystemModel {
        disable_warning,
        repositories,
        packages,
        provenance: Box::new(LoadedProvenance {
            authored_source,
            authored_fingerprint,
            generated_snapshot,
            generated_fingerprint,
            source_fingerprint,
        }),
        path: path.to_owned(),
    }
}

pub(crate) fn encode_snapshot(
    model: &SystemModel,
) -> Result<String, spec::ConversionError> {
    <gluon::SystemSnapshotCodec as DeclarationCodec<SystemModel>>::encode(
        &gluon::SystemSnapshotCodec::default(),
        model,
    )
}

/// Create a canonical generated system model.
pub fn create(repositories: repository::Map, packages: BTreeSet<dependency::Provider>) -> SystemModel {
    create_with_options(false, repositories, packages)
}

pub(super) fn create_with_options(
    disable_warning: bool,
    repositories: repository::Map,
    packages: BTreeSet<dependency::Provider>,
) -> SystemModel {
    SystemModel::regenerate(SystemParts {
        disable_warning,
        repositories,
        packages,
    })
    .expect("Cast-generated system snapshots must evaluate")
}

impl SystemModel {
    fn with_source_fingerprint(self, source_fingerprint: String) -> Result<Self, gluon::EvaluationError> {
        let generated = self
            .generated_snapshot
            .strip_prefix(spec::GENERATED_GLUON_MARKER)
            .expect("Cast-generated system snapshots always carry the generated marker");
        let snapshot = format!(
            "{}{SOURCE_FINGERPRINT_PREFIX}{source_fingerprint}\n{generated}",
            spec::GENERATED_GLUON_MARKER
        );
        gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", snapshot))
    }

    fn regenerate_with_source(
        parts: SystemParts,
        source_fingerprint: Option<String>,
    ) -> Result<Self, gluon::EvaluationError> {
        let model = Self::regenerate(parts)?;
        match source_fingerprint {
            Some(fingerprint) => model.with_source_fingerprint(fingerprint),
            None => Ok(model),
        }
    }

    /// Sync package selections through domain values and regenerate a
    /// canonical snapshot. No authored Gluon source is modified.
    pub fn sync_packages(self, packages: &[Package]) -> Result<SystemModel, UpdateError> {
        let source_fingerprint = self.source_fingerprint;
        let selected = self.packages;
        let mut updated = selected
            .iter()
            .filter(|provider| packages.iter().any(|package| package.meta.providers.contains(provider)))
            .cloned()
            .collect::<BTreeSet<_>>();

        for package in packages {
            if !package
                .meta
                .providers
                .iter()
                .any(|provider| selected.contains(provider))
            {
                updated.insert(dependency::Provider::package_name(package.meta.name.as_str()));
            }
        }

        Ok(Self::regenerate_with_source(
            SystemParts {
                disable_warning: self.disable_warning,
                repositories: self.repositories,
                packages: updated,
            },
            source_fingerprint,
        )?)
    }

    /// Replace matching repository domain values and regenerate a canonical
    /// snapshot. Repositories absent from this model are not added.
    pub fn update_repositories(mut self, repositories: &repository::Map) -> Result<SystemModel, UpdateError> {
        let source_fingerprint = self.source_fingerprint.take();
        for (id, repository) in repositories {
            if self.repositories.contains_id(id) {
                self.repositories.add(id.clone(), repository.clone());
            }
        }

        Ok(Self::regenerate_with_source(
            SystemParts {
                disable_warning: self.disable_warning,
                repositories: self.repositories,
                packages: self.packages,
            },
            source_fingerprint,
        )?)
    }
}

fn embedded_source_fingerprint(source: &str) -> Option<String> {
    source
        .lines()
        .find_map(|line| line.strip_prefix(SOURCE_FINGERPRINT_PREFIX))
        .filter(|fingerprint| fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(ToOwned::to_owned)
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("invalid system model path {0}")]
    InvalidPath(PathBuf),
    #[error("evaluate system model")]
    Evaluation(#[from] gluon::EvaluationError),
    #[error("load fixed system declaration")]
    FixedDeclaration(
        #[from]
        LoadFixedRootDeclarationError<spec::ConversionError>,
    ),
    #[error("discover descriptor-rooted system declaration")]
    RootedDiscovery(#[source] RootDeclarationDiscoveryError),
    #[error("descriptor-rooted system declaration slot changed beneath {0}")]
    RootedSlotChanged(PathBuf),
    #[error("retain descriptor-rooted system model source {path}")]
    RetainRootedSource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("descriptor-rooted system model source changed during evaluation: {0}")]
    RootedSourceChanged(PathBuf),
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("regenerate system model")]
    Evaluation(#[from] gluon::EvaluationError),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use fs_err as fs;

    use super::*;
    use crate::{Provider, Repository, package};

    fn authored_source() -> String {
        r#"// Authored intent is retained exactly.
let cast = import! cast.system.v1

{
    repositories = [
        cast.repository.direct "local" "file:///var/cache/local.index",
    ],
    packages = ["alpha"],
    .. cast.system
}
"#
        .to_owned()
    }

    fn package(name: &str, providers: impl IntoIterator<Item = Provider>) -> Package {
        Package {
            id: package::Id::from(name.to_owned()),
            meta: package::Meta {
                name: name.to_owned().into(),
                version_identifier: String::new(),
                source_release: 0,
                build_release: 0,
                architecture: String::new(),
                summary: String::new(),
                description: String::new(),
                source_id: String::new(),
                homepage: String::new(),
                licenses: Vec::new(),
                dependencies: Default::default(),
                providers: providers.into_iter().collect(),
                conflicts: Default::default(),
                uri: None,
                hash: None,
                download_size: None,
            },
            flags: package::Flags::default(),
        }
    }

    fn repository(uri: &str, priority: u64) -> Repository {
        Repository {
            description: "repository".to_owned(),
            source: repository::Source::DirectIndex(uri.parse().unwrap()),
            priority: repository::Priority::new(priority),
            active: true,
        }
    }

    #[test]
    fn canonical_paths_separate_authored_intent_from_generated_state() {
        let root = Path::new("/target");

        assert_eq!(intent_path(root), root.join("etc/cast/system.glu"));
        assert_eq!(snapshot_path(root), root.join("usr/lib/system-model.glu"));
        assert_ne!(intent_path(root), snapshot_path(root));
    }

    #[test]
    fn rooted_load_uses_retained_directory_during_public_ancestor_absence() {
        let temporary = tempfile::tempdir().unwrap();
        let public_root = temporary.path().join("public");
        let public_etc = public_root.join("etc");
        let directory = public_etc.join("cast");
        fs::create_dir_all(&directory).unwrap();
        let source_path = directory.join("system.glu");
        fs::write(&source_path, authored_source()).unwrap();
        fs::set_permissions(
            &source_path,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let retained_directory = std::fs::File::open(&directory).unwrap();

        let detached_etc = temporary.path().join("detached-etc");
        let replacement_etc = temporary.path().join("replacement-etc");
        let evacuated_etc = temporary.path().join("evacuated-etc");
        fs::create_dir_all(replacement_etc.join("cast")).unwrap();
        fs::rename(&public_etc, &detached_etc).unwrap();
        fs::rename(&replacement_etc, &public_etc).unwrap();

        let hook_public_etc = public_etc.clone();
        let hook_detached_etc = detached_etc.clone();
        let hook_evacuated_etc = evacuated_etc.clone();
        arm_after_rooted_system_source_retained(move || {
            fs::rename(&hook_public_etc, &hook_evacuated_etc).unwrap();
            fs::rename(&hook_detached_etc, &hook_public_etc).unwrap();
        });

        let loaded = load_rooted(&directory, &retained_directory)
            .unwrap()
            .unwrap();

        assert_eq!(loaded.path(), source_path);
        assert!(loaded.packages.contains(&Provider::package_name("alpha")));
        assert!(!evacuated_etc.join("cast/system.glu").exists());
    }

    #[test]
    fn load_retains_authored_source_and_records_both_fingerprints() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system.glu");
        let authored = authored_source();
        fs::write(&path, &authored).unwrap();

        let loaded = load(&path).unwrap().unwrap();

        assert_eq!(loaded.authored_source(), authored);
        assert_eq!(loaded.encoded(), authored);
        assert!(loaded.generated_snapshot().starts_with(spec::GENERATED_GLUON_MARKER));
        assert_eq!(
            loaded
                .fingerprint()
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["cast.system.v1"]
        );
        assert!(loaded.generated_fingerprint().imported_modules.is_empty());
        assert_ne!(loaded.fingerprint().sha256, loaded.generated_fingerprint().sha256);
    }

    #[test]
    fn generated_snapshot_loads_and_round_trips_canonically() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system-model.glu");
        let model = create(
            repository::Map::with([(
                repository::Id::new("local"),
                repository("file:///var/cache/local.index", 5),
            )]),
            BTreeSet::from([Provider::package_name("alpha")]),
        );
        fs::write(&path, model.encoded()).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        let round_trip = SystemModel::try_from(loaded.clone()).unwrap();

        assert_eq!(loaded.authored_source(), model.encoded());
        assert_eq!(round_trip.encoded(), model.encoded());
        assert_eq!(round_trip.fingerprint(), model.fingerprint());
        assert!(round_trip.packages.contains(&Provider::package_name("alpha")));
    }

    #[test]
    fn fixed_loader_keeps_engine_and_conversion_errors_typed_with_exact_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system.glu");
        fs::write(
            &path,
            "let cast = import! cast.system.v1\n{ packages = [1], .. cast.system }",
        )
        .unwrap();

        let engine_error = load(&path).unwrap_err();
        assert!(matches!(
            engine_error,
            LoadError::FixedDeclaration(
                LoadFixedRootDeclarationError::Evaluation {
                    path: ref error_path,
                    ..
                }
            ) if error_path == &path
        ));

        fs::write(
            &path,
            r#"let cast = import! cast.system.v1
{
    repositories = [cast.repository.direct_with {
        id = "bad",
        description = cast.optional.none,
        uri = "https://example.test/index.stone",
        priority = cast.optional.some (-1),
        enabled = cast.optional.none,
    }],
    .. cast.system
}
"#,
        )
        .unwrap();

        let conversion_error = load(&path).unwrap_err();
        assert!(matches!(
            conversion_error,
            LoadError::FixedDeclaration(
                LoadFixedRootDeclarationError::Conversion {
                    path: ref error_path,
                    source: ref error,
                }
            ) if error_path == &path && error.path() == "repositories[0].priority"
        ));
    }

    #[test]
    fn fixed_loader_accepts_only_the_registered_gluon_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system.lua");
        fs::write(&path, authored_source()).unwrap();

        assert!(matches!(load(&path), Err(LoadError::InvalidPath(found)) if found == path));
    }

    #[test]
    fn fixed_loader_keeps_relative_imports_beneath_its_retained_root() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system.glu");
        fs::write(&path, "import! \"./selection.glu\"").unwrap();
        fs::write(
            temporary.path().join("selection.glu"),
            r#"let cast = import! cast.system.v1
{
    packages = ["alpha"],
    .. cast.system
}
"#,
        )
        .unwrap();

        let loaded = load(&path).unwrap().unwrap();

        assert!(loaded.packages.contains(&Provider::package_name("alpha")));
        assert_eq!(
            loaded
                .fingerprint()
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["cast.system.v1", "selection.glu"]
        );
    }

    #[test]
    fn authored_fingerprint_is_embedded_and_preserved_across_updates() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("system.glu");
        fs::write(&path, authored_source()).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        let authored_fingerprint = loaded.fingerprint().sha256.clone();

        let snapshot = SystemModel::try_from(loaded)
            .unwrap()
            .sync_packages(&[package("alpha", [Provider::package_name("alpha")])])
            .unwrap();

        assert_eq!(snapshot.source_fingerprint(), Some(authored_fingerprint.as_str()));
        assert!(
            snapshot
                .encoded()
                .contains(&format!("{SOURCE_FINGERPRINT_PREFIX}{authored_fingerprint}"))
        );

        fs::write(&path, snapshot.encoded()).unwrap();
        let reloaded = load(&path).unwrap().unwrap();
        assert_eq!(reloaded.source_fingerprint(), Some(authored_fingerprint.as_str()));
        let round_trip = SystemModel::try_from(reloaded).unwrap();
        assert_eq!(round_trip.encoded(), snapshot.encoded());
        assert_eq!(round_trip.fingerprint(), snapshot.fingerprint());
    }

    #[test]
    fn package_and_repository_updates_are_functional_and_regenerate() {
        let original = create(
            repository::Map::with([(
                repository::Id::new("local"),
                repository("https://old.example.test/index.stone", 1),
            )]),
            BTreeSet::from([
                Provider::package_name("alpha"),
                Provider::from_name("binary(beta)").unwrap(),
            ]),
        );
        let original_snapshot = original.encoded().to_owned();
        let updated = original
            .sync_packages(&[
                package("alpha", [Provider::package_name("alpha")]),
                package("gamma", [Provider::package_name("gamma")]),
            ])
            .unwrap();

        assert!(updated.packages.contains(&Provider::package_name("alpha")));
        assert!(updated.packages.contains(&Provider::package_name("gamma")));
        assert!(!updated.packages.contains(&Provider::from_name("binary(beta)").unwrap()));
        assert_ne!(updated.encoded(), original_snapshot);

        let repositories = repository::Map::with([
            (
                repository::Id::new("local"),
                repository("https://new.example.test/index.stone", 9),
            ),
            (
                repository::Id::new("not-added"),
                repository("https://ignored.example.test/index.stone", 10),
            ),
        ]);
        let updated = updated.update_repositories(&repositories).unwrap();
        let local = updated.repositories.get(&repository::Id::new("local")).unwrap();

        assert_eq!(u64::from(local.priority), 9);
        assert_eq!(
            local.source.direct_index().map(|url| url.as_str()),
            Some("https://new.example.test/index.stone")
        );
        assert!(!updated.repositories.contains_id(&repository::Id::new("not-added")));
        let evaluated =
            gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", updated.encoded())).unwrap();
        assert_eq!(evaluated.encoded(), updated.encoded());
        assert_eq!(evaluated.fingerprint(), updated.fingerprint());
    }
}
