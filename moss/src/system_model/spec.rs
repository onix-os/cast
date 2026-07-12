// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Format-neutral data transfer objects for declarative system configuration.
//!
//! These types deliberately contain only primitive values, collections,
//! options, and explicit variants. Parsing into domain types happens at this
//! boundary so configuration frontends do not expose Moss internals as their
//! public schema.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use crate::{Repository, dependency, repository};

use super::SystemModel;

/// A declarative system configuration before domain conversion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemSpec {
    pub disable_warning: bool,
    pub repositories: Vec<RepositorySpec>,
    /// Package names and explicit provider expressions such as
    /// `soname(libc.so.6)`.
    pub packages: Vec<String>,
}

/// A repository entry before URL and repository-format validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySpec {
    pub id: String,
    pub description: Option<String>,
    pub source: RepositorySourceSpec,
    /// Signed at the DTO boundary so a negative configuration value is
    /// rejected with a field-path diagnostic instead of being coerced.
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
}

/// The supported repository source variants before semantic validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositorySourceSpec {
    DirectIndex {
        uri: String,
    },
    RootIndex {
        base_uri: String,
        channel: Option<String>,
        version: String,
        arch: Option<String>,
    },
}

/// A semantic conversion error tied to its DTO field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionError {
    path: String,
    message: String,
}

impl ConversionError {
    fn new(path: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            path: path.into(),
            message: error.to_string(),
        }
    }

    fn at(mut self, parent: impl fmt::Display) -> Self {
        self.path = if self.path.is_empty() {
            parent.to_string()
        } else {
            format!("{parent}.{}", self.path)
        };
        self
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid system specification at `{}`: {}", self.path, self.message)
    }
}

impl Error for ConversionError {}

impl TryFrom<SystemSpec> for SystemModel {
    type Error = ConversionError;

    fn try_from(spec: SystemSpec) -> Result<Self, Self::Error> {
        let mut repository_ids = BTreeSet::new();
        let repositories = spec
            .repositories
            .into_iter()
            .enumerate()
            .map(|(index, repository)| {
                let path = format!("repositories[{index}]");
                let (id, repository) = convert_repository(repository).map_err(|error| error.at(&path))?;

                if !repository_ids.insert(id.clone()) {
                    return Err(ConversionError::new(
                        format!("{path}.id"),
                        format_args!("duplicate repository identifier `{id}`"),
                    ));
                }

                Ok((id, repository))
            })
            .collect::<Result<repository::Map, _>>()?;

        let packages = spec
            .packages
            .into_iter()
            .enumerate()
            .map(|(index, package)| {
                dependency::Provider::from_name(&package)
                    .map_err(|error| ConversionError::new(format!("packages[{index}]"), error))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;

        Ok(super::create_with_options(spec.disable_warning, repositories, packages))
    }
}

impl TryFrom<RepositorySpec> for (repository::Id, Repository) {
    type Error = ConversionError;

    fn try_from(spec: RepositorySpec) -> Result<Self, Self::Error> {
        convert_repository(spec)
    }
}

fn convert_repository(spec: RepositorySpec) -> Result<(repository::Id, Repository), ConversionError> {
    let id = repository::Id::new(&spec.id);
    let source = repository::Source::try_from(spec.source).map_err(|error| error.at("source"))?;
    let priority = spec
        .priority
        .unwrap_or_default()
        .try_into()
        .map(repository::Priority::new)
        .map_err(|error| ConversionError::new("priority", error))?;

    Ok((
        id,
        Repository {
            description: spec.description.unwrap_or_default(),
            source,
            priority,
            active: spec.enabled.unwrap_or(true),
        },
    ))
}

impl TryFrom<RepositorySourceSpec> for repository::Source {
    type Error = ConversionError;

    fn try_from(spec: RepositorySourceSpec) -> Result<Self, Self::Error> {
        match spec {
            RepositorySourceSpec::DirectIndex { uri } => uri
                .parse()
                .map(Self::DirectIndex)
                .map_err(|error| ConversionError::new("uri", error)),
            RepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => {
                let base_uri = base_uri
                    .parse()
                    .map_err(|error| ConversionError::new("base_uri", error))?;
                let channel = channel
                    .unwrap_or_else(|| repository::DEFAULT_CHANNEL.to_owned())
                    .try_into()
                    .map_err(|error| ConversionError::new("channel", error))?;
                let version = version
                    .parse()
                    .map_err(|error| ConversionError::new("version", error))?;

                Ok(Self::RootIndex(repository::RootIndexSource {
                    base_uri,
                    channel,
                    version,
                    arch: arch.unwrap_or_else(|| repository::DEFAULT_ARCH.to_owned()),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_repository(uri: &str) -> RepositorySpec {
        RepositorySpec {
            id: "local".to_owned(),
            description: None,
            source: RepositorySourceSpec::DirectIndex { uri: uri.to_owned() },
            priority: None,
            enabled: None,
        }
    }

    fn root_repository() -> RepositorySpec {
        RepositorySpec {
            id: "volatile".to_owned(),
            description: None,
            source: RepositorySourceSpec::RootIndex {
                base_uri: "https://packages.example.test".to_owned(),
                channel: None,
                version: "stream/volatile".to_owned(),
                arch: None,
            },
            priority: None,
            enabled: None,
        }
    }

    fn conversion_error(spec: SystemSpec) -> ConversionError {
        SystemModel::try_from(spec).expect_err("spec should be invalid")
    }

    #[test]
    fn empty_spec_uses_system_defaults() {
        let model = SystemModel::try_from(SystemSpec::default()).expect("convert empty spec");

        assert!(!model.disable_warning);
        assert_eq!(model.repositories.iter().count(), 0);
        assert!(model.packages.is_empty());
    }

    #[test]
    fn converts_repository_defaults_and_package_provider_selections() {
        let model = SystemModel::try_from(SystemSpec {
            disable_warning: true,
            repositories: vec![root_repository(), direct_repository("file:///var/cache/local.index")],
            packages: vec!["moss".to_owned(), "soname(libc.so.6)".to_owned()],
        })
        .expect("convert populated spec");

        assert!(model.disable_warning);
        assert!(model.encoded().starts_with("disable_warning #true\n"));

        let root = model
            .repositories
            .get(&repository::Id::new("volatile"))
            .expect("root repository");
        assert_eq!(root.description, "");
        assert_eq!(u64::from(root.priority), 0);
        assert!(root.active);
        let repository::Source::RootIndex(root_source) = &root.source else {
            panic!("expected root-index source");
        };
        assert_eq!(root_source.base_uri.as_str(), "https://packages.example.test/");
        assert_eq!(root_source.channel.as_ref(), repository::DEFAULT_CHANNEL);
        assert_eq!(root_source.version.to_string(), "stream/volatile");
        assert_eq!(root_source.arch, repository::DEFAULT_ARCH);

        let direct = model
            .repositories
            .get(&repository::Id::new("local"))
            .expect("direct repository");
        assert_eq!(u64::from(direct.priority), 0);
        assert!(direct.active);
        assert_eq!(
            direct.source.direct_index().map(|uri| uri.as_str()),
            Some("file:///var/cache/local.index")
        );

        assert_eq!(
            model.packages,
            BTreeSet::from([
                dependency::Provider::package_name("moss"),
                dependency::Provider::from_name("soname(libc.so.6)").unwrap(),
            ])
        );
    }

    #[test]
    fn converts_explicit_repository_fields() {
        let (id, repository) = <(repository::Id, Repository)>::try_from(RepositorySpec {
            id: "testing".to_owned(),
            description: Some("testing packages".to_owned()),
            source: RepositorySourceSpec::RootIndex {
                base_uri: "https://packages.example.test/base".to_owned(),
                channel: Some("testing".to_owned()),
                version: "tag/2026-07".to_owned(),
                arch: Some("aarch64".to_owned()),
            },
            priority: Some(50),
            enabled: Some(false),
        })
        .expect("convert repository");

        assert_eq!(id.to_string(), "testing");
        assert_eq!(repository.description, "testing packages");
        assert_eq!(u64::from(repository.priority), 50);
        assert!(!repository.active);
        let repository::Source::RootIndex(source) = repository.source else {
            panic!("expected root-index source");
        };
        assert_eq!(source.channel.as_ref(), "testing");
        assert_eq!(source.version.to_string(), "tag/2026-07");
        assert_eq!(source.arch, "aarch64");
    }

    #[test]
    fn reports_url_field_paths() {
        let direct = conversion_error(SystemSpec {
            repositories: vec![direct_repository("not a url")],
            ..SystemSpec::default()
        });
        assert_eq!(direct.path(), "repositories[0].source.uri");

        let mut root = root_repository();
        let RepositorySourceSpec::RootIndex { base_uri, .. } = &mut root.source else {
            unreachable!();
        };
        *base_uri = "not a url".to_owned();
        let root = conversion_error(SystemSpec {
            repositories: vec![root],
            ..SystemSpec::default()
        });
        assert_eq!(root.path(), "repositories[0].source.base_uri");
    }

    #[test]
    fn reports_channel_and_version_field_paths() {
        let mut invalid_channel = root_repository();
        let RepositorySourceSpec::RootIndex { channel, .. } = &mut invalid_channel.source else {
            unreachable!();
        };
        *channel = Some("-testing".to_owned());
        let error = conversion_error(SystemSpec {
            repositories: vec![invalid_channel],
            ..SystemSpec::default()
        });
        assert_eq!(error.path(), "repositories[0].source.channel");

        let mut invalid_version = root_repository();
        let RepositorySourceSpec::RootIndex { version, .. } = &mut invalid_version.source else {
            unreachable!();
        };
        *version = "volatile".to_owned();
        let error = conversion_error(SystemSpec {
            repositories: vec![invalid_version],
            ..SystemSpec::default()
        });
        assert_eq!(error.path(), "repositories[0].source.version");
    }

    #[test]
    fn reports_priority_and_provider_field_paths() {
        let mut repository = direct_repository("https://packages.example.test/index.stone");
        repository.priority = Some(-1);
        let error = conversion_error(SystemSpec {
            repositories: vec![repository],
            ..SystemSpec::default()
        });
        assert_eq!(error.path(), "repositories[0].priority");

        let error = conversion_error(SystemSpec {
            packages: vec!["soname(incomplete".to_owned()],
            ..SystemSpec::default()
        });
        assert_eq!(error.path(), "packages[0]");
    }

    #[test]
    fn rejects_duplicate_normalized_repository_ids() {
        let mut original = direct_repository("https://packages.example.test/one.index");
        original.id = "local?".to_owned();
        let mut duplicate = direct_repository("https://packages.example.test/two.index");
        duplicate.id = "local!".to_owned();
        let error = conversion_error(SystemSpec {
            repositories: vec![original, duplicate],
            ..SystemSpec::default()
        });

        assert_eq!(error.path(), "repositories[1].id");
        assert!(error.message().contains("duplicate repository identifier"));
    }
}
