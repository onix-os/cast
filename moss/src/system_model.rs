// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use gluon_config::{EvaluationFingerprint, Evaluator, Source, SourceRoot};
use thiserror::Error;

use crate::{Package, dependency, repository};

pub mod gluon;
pub mod spec;

/// User-authored desired system intent, relative to an installation root.
pub const SYSTEM_INTENT_PATH: &str = "etc/moss/system.glu";

/// Moss-generated normalized state snapshot, relative to a state root.
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
    if !path.exists() {
        return Ok(None);
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| LoadError::InvalidPath(path.to_owned()))?;
    let source_root = SourceRoot::new(parent).map_err(gluon::EvaluationError::from)?;
    let evaluator = Evaluator::default().with_source_root(source_root.clone());
    let source = source_root
        .load(Path::new(file_name), evaluator.limits().max_source_bytes)
        .map_err(gluon::EvaluationError::from)?;
    let authored_source = source.text().to_owned();
    let evaluated = gluon::evaluate_with(&evaluator, &source)?;
    let authored_fingerprint = evaluated.fingerprint;
    let SystemModel {
        disable_warning,
        repositories,
        packages,
        generated_snapshot,
        fingerprint: generated_fingerprint,
        ..
    } = evaluated.model;
    let source_fingerprint = if authored_source.starts_with(spec::GENERATED_GLUON_MARKER) {
        embedded_source_fingerprint(&authored_source)
    } else {
        Some(authored_fingerprint.sha256.clone())
    };

    Ok(Some(LoadedSystemModel {
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
    }))
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
    .expect("Moss-generated system snapshots must evaluate")
}

impl SystemModel {
    fn with_source_fingerprint(self, source_fingerprint: String) -> Result<Self, gluon::EvaluationError> {
        let generated = self
            .generated_snapshot
            .strip_prefix(spec::GENERATED_GLUON_MARKER)
            .expect("Moss-generated system snapshots always carry the generated marker");
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
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("regenerate system model")]
    Evaluation(#[from] gluon::EvaluationError),
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;
    use crate::{Provider, Repository, package};

    fn authored_source() -> String {
        r#"// Authored intent is retained exactly.
let moss = import! moss.system.v1

{
    repositories = [
        moss.repository.direct "local" "file:///var/cache/local.index",
    ],
    packages = ["alpha"],
    .. moss.system
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

        assert_eq!(intent_path(root), root.join("etc/moss/system.glu"));
        assert_eq!(snapshot_path(root), root.join("usr/lib/system-model.glu"));
        assert_ne!(intent_path(root), snapshot_path(root));
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
            ["moss.system.v1"]
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
