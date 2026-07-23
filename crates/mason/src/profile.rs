// SPDX-FileCopyrightText: 2024 AerynOS Developers

//! Language-neutral domain model and orchestration for Cast profiles.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt,
};

use config::{
    Config,
    declaration::{
        DeclarationEvaluatorSet, LoadManagedDeclarationError,
        SaveManagedDeclarationError,
    },
};
use derive_more::{Debug, Display};
use forge::{Repository, repository};
use stone_recipe::derivation::ProfileFragmentProvenance;
use thiserror::Error;

use crate::Env;

mod gluon;

pub use gluon::{GLUON_PROFILE_ABI, PROFILE_ABI_VERSION, ProfileCodec};

/// A unique [`Profile`] identifier.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Display)]
#[debug("{_0:?}")]
pub struct Id(String);

impl Id {
    pub fn new(identifier: &str) -> Self {
        Self(
            identifier
                .chars()
                .map(|character| {
                    if character.is_alphanumeric() || character == '-' {
                        character
                    } else {
                        '_'
                    }
                })
                .collect(),
        )
    }
}

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self::new(&value)
    }
}

/// Profile configuration data.
#[derive(Debug, Clone)]
pub struct Profile {
    pub repositories: repository::Map,
}

/// A map of profiles.
#[derive(Debug, Clone, Default)]
pub struct Map(BTreeMap<Id, Profile>);

impl Map {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn with(items: impl IntoIterator<Item = (Id, Profile)>) -> Self {
        Self(items.into_iter().collect())
    }

    pub fn get(&self, id: &Id) -> Option<&Profile> {
        self.0.get(id)
    }

    pub fn add(&mut self, id: Id, profile: Profile) {
        self.0.insert(id, profile);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Id, &Profile)> {
        self.0.iter()
    }

    /// Merge a higher-priority fragment into this map.
    pub fn merge(self, other: Self) -> Self {
        Self(self.0.into_iter().chain(other.0).collect())
    }
}

impl IntoIterator for Map {
    type Item = (Id, Profile);
    type IntoIter = std::collections::btree_map::IntoIter<Id, Profile>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Config for Map {
    fn domain() -> String {
        "profile".into()
    }
}

/// Semantic profile conversion failure with a stable field path.
#[derive(Debug)]
pub struct ProfileConversionError {
    path: String,
    message: String,
    source: Option<forge::system_model::spec::ConversionError>,
}

impl ProfileConversionError {
    fn from_repository(
        profile_index: usize,
        repository_index: usize,
        error: forge::system_model::spec::ConversionError,
    ) -> Self {
        let parent = format!("profiles[{profile_index}].repositories[{repository_index}]");
        let path = if error.path().is_empty() {
            parent
        } else {
            format!("{parent}.{}", error.path())
        };
        Self {
            path,
            message: error.message().to_owned(),
            source: Some(error),
        }
    }

    fn duplicate_profile(index: usize, id: &Id) -> Self {
        Self {
            path: format!("profiles[{index}].id"),
            message: format!("duplicate profile identifier `{id}`"),
            source: None,
        }
    }

    fn duplicate_repository(profile_index: usize, repository_index: usize, id: &repository::Id) -> Self {
        Self {
            path: format!("profiles[{profile_index}].repositories[{repository_index}].id"),
            message: format!("duplicate repository identifier `{id}`"),
            source: None,
        }
    }

    fn encode(path: String, error: impl fmt::Display) -> Self {
        Self {
            path,
            message: error.to_string(),
            source: None,
        }
    }
}

impl fmt::Display for ProfileConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid profile configuration at `{}`: {}",
            self.path, self.message
        )
    }
}

impl StdError for ProfileConversionError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_ref().map(|error| error as &(dyn StdError + 'static))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileSpec {
    id: String,
    repositories: Vec<RepositorySpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositorySpec {
    id: String,
    description: Option<String>,
    source: RepositorySourceSpec,
    priority: Option<i64>,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RepositorySourceSpec {
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

impl From<RepositorySpec> for forge::system_model::spec::RepositorySpec {
    fn from(value: RepositorySpec) -> Self {
        let source = match value.source {
            RepositorySourceSpec::DirectIndex { uri } => {
                forge::system_model::spec::RepositorySourceSpec::DirectIndex { uri }
            }
            RepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => forge::system_model::spec::RepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            },
        };

        Self {
            id: value.id,
            description: value.description,
            source,
            priority: value.priority,
            enabled: value.enabled,
        }
    }
}

fn decode_specs(specs: Vec<ProfileSpec>) -> Result<Map, ProfileConversionError> {
    let mut ids = BTreeSet::new();
    let mut profiles = Map::default();

    for (profile_index, spec) in specs.into_iter().enumerate() {
        let id = Id::new(&spec.id);
        if !ids.insert(id.clone()) {
            return Err(ProfileConversionError::duplicate_profile(profile_index, &id));
        }

        let mut repository_ids = BTreeSet::new();
        let mut repositories = Vec::with_capacity(spec.repositories.len());
        for (repository_index, spec) in spec.repositories.into_iter().enumerate() {
            let converted =
                <(repository::Id, Repository)>::try_from(forge::system_model::spec::RepositorySpec::from(spec))
                    .map_err(|error| ProfileConversionError::from_repository(profile_index, repository_index, error))?;
            if !repository_ids.insert(converted.0.clone()) {
                return Err(ProfileConversionError::duplicate_repository(
                    profile_index,
                    repository_index,
                    &converted.0,
                ));
            }
            repositories.push(converted);
        }

        profiles.add(
            id,
            Profile {
                repositories: repository::Map::with(repositories),
            },
        );
    }

    Ok(profiles)
}

fn profile_to_spec((id, profile): (&Id, &Profile)) -> Result<ProfileSpec, ProfileConversionError> {
    let repositories = profile
        .repositories
        .iter()
        .map(|(repository_id, repository)| repository_to_spec(id, repository_id, repository))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProfileSpec {
        id: id.to_string(),
        repositories,
    })
}

fn repository_to_spec(
    profile_id: &Id,
    id: &repository::Id,
    repository: &Repository,
) -> Result<RepositorySpec, ProfileConversionError> {
    let priority = i64::try_from(u64::from(repository.priority)).map_err(|error| {
        ProfileConversionError::encode(
            format!("profiles[\"{profile_id}\"].repositories[\"{id}\"].priority"),
            error,
        )
    })?;
    let source = match &repository.source {
        repository::Source::DirectIndex(uri) => RepositorySourceSpec::DirectIndex { uri: uri.to_string() },
        repository::Source::RootIndex(source) => RepositorySourceSpec::RootIndex {
            base_uri: source.base_uri.to_string(),
            channel: Some(source.channel.to_string()),
            version: source.version.to_string(),
            arch: Some(source.arch.clone()),
        },
    };

    Ok(RepositorySpec {
        id: id.to_string(),
        description: Some(repository.description.clone()),
        source,
        priority: Some(priority),
        enabled: Some(repository.active),
    })
}

pub struct Manager<'a> {
    pub profiles: Map,
    /// Ordered, portable provenance for every profile fragment before values
    /// are merged according to configuration precedence.
    pub fragments: Vec<ProfileFragmentProvenance>,
    env: &'a Env,
}

impl<'a> Manager<'a> {
    pub fn new(env: &'a Env) -> Result<Manager<'a>, Error> {
        let evaluators = DeclarationEvaluatorSet::new([ProfileCodec::default()])
            .expect("one validated profile adapter has no extension collision");
        let loaded = env.config.load_declarations(&evaluators)?;
        let fragments = loaded
            .iter()
            .map(|loaded| ProfileFragmentProvenance {
                logical_name: loaded.logical_name.clone(),
                evaluation: loaded.identity.clone(),
            })
            .collect();
        let profiles = loaded
            .into_iter()
            .map(|loaded| loaded.value)
            .reduce(Map::merge)
            .unwrap_or_default();

        Ok(Self {
            env,
            profiles,
            fragments,
        })
    }

    pub fn repositories(&self, profile: &Id) -> Result<&repository::Map, Error> {
        self.profiles
            .get(profile)
            .map(|profile| &profile.repositories)
            .ok_or_else(|| Error::MissingProfile(profile.clone()))
    }

    /// Return the selected repositories only when every active root index was
    /// authored for the build platform that will execute the frozen plan.
    ///
    /// Direct indexes are already concrete index locations and therefore have
    /// no separate architecture declaration to validate here. Disabled root
    /// indexes cannot participate in resolution and are ignored.
    pub fn repositories_for_architecture(&self, profile: &Id, architecture: &str) -> Result<&repository::Map, Error> {
        let repositories = self.repositories(profile)?;
        for (id, repository) in repositories.iter() {
            if !repository.active {
                continue;
            }
            if let repository::Source::RootIndex(source) = &repository.source
                && source.arch != architecture
            {
                return Err(Error::RepositoryArchitectureMismatch {
                    profile: profile.clone(),
                    repository: id.clone(),
                    configured: source.arch.clone(),
                    requested: architecture.to_owned(),
                });
            }
        }
        Ok(repositories)
    }

    pub fn save_profile(&mut self, id: Id, profile: Profile) -> Result<(), Error> {
        let map = Map::with([(id.clone(), profile)]);
        let codec = ProfileCodec::default();
        self.env
            .config
            .save_declaration(id, &map, &codec)?;

        // Saving changes both the resolved profile map and the ordered
        // evaluation provenance used by planning. Reload them atomically so a
        // reused manager cannot pair new repository values with stale
        // fingerprints.
        let reloaded = Self::new(self.env)?;
        self.profiles = reloaded.profiles;
        self.fragments = reloaded.fragments;

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot find the provided profile: {0}")]
    MissingProfile(Id),
    #[error(
        "profile {profile} repository {repository} targets architecture {configured}, not selected build architecture {requested}"
    )]
    RepositoryArchitectureMismatch {
        profile: Id,
        repository: repository::Id,
        configured: String,
        requested: String,
    },
    #[error("load profiles")]
    LoadProfiles(
        #[source] Box<LoadManagedDeclarationError<ProfileConversionError>>,
    ),
    #[error("save profile")]
    SaveProfile(
        #[source] Box<SaveManagedDeclarationError<ProfileConversionError>>,
    ),
}

impl From<LoadManagedDeclarationError<ProfileConversionError>> for Error {
    fn from(error: LoadManagedDeclarationError<ProfileConversionError>) -> Self {
        Self::LoadProfiles(Box::new(error))
    }
}

impl From<SaveManagedDeclarationError<ProfileConversionError>> for Error {
    fn from(error: SaveManagedDeclarationError<ProfileConversionError>) -> Self {
        Self::SaveProfile(Box::new(error))
    }
}

#[cfg(test)]
#[path = "profile/tests.rs"]
mod tests;
