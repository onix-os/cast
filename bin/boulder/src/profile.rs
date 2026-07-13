// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Versioned Gluon boundary and domain model for Boulder profiles.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt::{self, Write as _},
};

use config::{Config, DecodedGluon, GluonCodec, GluonCodecError};
use derive_more::{Debug, Display};
use gluon_config::{Evaluator, Source as GluonSource};
use moss::{Repository, repository};
use stone_recipe::derivation::ProfileFragmentProvenance;
use thiserror::Error;

use crate::Env;

/// Version of the embedded profile configuration API.
pub const PROFILE_ABI_VERSION: u32 = 1;

/// Pure definitions imported by authored fragments as `boulder.profile.v1`.
pub const GLUON_PROFILE_ABI: &str = include_str!("../gluon/profile.glu");

const STANDALONE_GLUON_TYPES: &str = r#"type Optional a =
    | None
    | Some a

type Boolean =
    | False
    | True

type RepositorySourceSpec =
    | DirectIndex { uri : String }
    | RootIndex {
        base_uri : String,
        channel : Optional String,
        version : String,
        arch : Optional String,
    }

type RepositorySpec = {
    id : String,
    description : Optional String,
    source : RepositorySourceSpec,
    priority : Optional Int,
    enabled : Optional Boolean,
}

type ProfileSpec = {
    id : String,
    repositories : Array RepositorySpec,
}

"#;

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

/// Stateless profile configuration codec used by [`config::Manager`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfileCodec;

/// Semantic profile conversion failure with a stable field path.
#[derive(Debug)]
pub struct ProfileConversionError {
    path: String,
    message: String,
    source: Option<moss::system_model::spec::ConversionError>,
}

impl ProfileConversionError {
    fn from_repository(
        profile_index: usize,
        repository_index: usize,
        error: moss::system_model::spec::ConversionError,
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

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    None,
    Some(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBool {
    False,
    True,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonProfileSpec {
    id: String,
    repositories: Vec<GluonRepositorySpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRepositorySpec {
    id: String,
    description: GluonOptional<String>,
    source: GluonRepositorySourceSpec,
    priority: GluonOptional<i64>,
    enabled: GluonOptional<GluonBool>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonRepositorySourceSpec {
    DirectIndex {
        uri: String,
    },
    RootIndex {
        base_uri: String,
        channel: GluonOptional<String>,
        version: String,
        arch: GluonOptional<String>,
    },
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::None => None,
            GluonOptional::Some(value) => Some(value),
        }
    }
}

impl From<GluonBool> for bool {
    fn from(value: GluonBool) -> Self {
        match value {
            GluonBool::False => false,
            GluonBool::True => true,
        }
    }
}

impl From<GluonProfileSpec> for ProfileSpec {
    fn from(value: GluonProfileSpec) -> Self {
        Self {
            id: value.id,
            repositories: value.repositories.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonRepositorySpec> for RepositorySpec {
    fn from(value: GluonRepositorySpec) -> Self {
        Self {
            id: value.id,
            description: value.description.into(),
            source: value.source.into(),
            priority: value.priority.into(),
            enabled: Option::<GluonBool>::from(value.enabled).map(Into::into),
        }
    }
}

impl From<GluonRepositorySourceSpec> for RepositorySourceSpec {
    fn from(value: GluonRepositorySourceSpec) -> Self {
        match value {
            GluonRepositorySourceSpec::DirectIndex { uri } => Self::DirectIndex { uri },
            GluonRepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => Self::RootIndex {
                base_uri,
                channel: channel.into(),
                version,
                arch: arch.into(),
            },
        }
    }
}

impl From<RepositorySpec> for moss::system_model::spec::RepositorySpec {
    fn from(value: RepositorySpec) -> Self {
        let source = match value.source {
            RepositorySourceSpec::DirectIndex { uri } => {
                moss::system_model::spec::RepositorySourceSpec::DirectIndex { uri }
            }
            RepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => moss::system_model::spec::RepositorySourceSpec::RootIndex {
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

impl GluonCodec for ProfileCodec {
    type Config = Map;

    fn decode(
        &self,
        evaluator: &Evaluator,
        source: &GluonSource,
    ) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        let mut policy = evaluator.import_policy().clone();
        policy.insert_embedded_module("boulder.profile.v1", GLUON_PROFILE_ABI)?;
        let evaluator = evaluator.clone().with_import_policy(policy);
        let evaluation = evaluator.evaluate::<Vec<GluonProfileSpec>>(source)?;
        let fingerprint = evaluation.fingerprint;
        let profiles = evaluation.value.into_iter().map(Into::into).collect();
        let value = decode_specs(profiles).map_err(GluonCodecError::conversion)?;

        Ok(DecodedGluon { value, fingerprint })
    }

    fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
        let specs = config
            .iter()
            .map(profile_to_spec)
            .collect::<Result<Vec<_>, _>>()
            .map_err(GluonCodecError::conversion)?;
        Ok(encode_specs(&specs))
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
                <(repository::Id, Repository)>::try_from(moss::system_model::spec::RepositorySpec::from(spec))
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

fn encode_specs(specs: &[ProfileSpec]) -> String {
    let mut profiles = specs.iter().collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.id.cmp(&right.id));

    let mut output = format!("// Canonical standalone ProfileSpec snapshot (ABI {PROFILE_ABI_VERSION}).\n");
    output.push_str(STANDALONE_GLUON_TYPES);
    output.push_str("[\n");
    for profile in profiles {
        output.push_str("    {\n");
        writeln!(output, "        id = {},", gluon_string(&profile.id)).unwrap();
        output.push_str("        repositories = [\n");
        let mut repositories = profile.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.id.cmp(&right.id));
        for repository in repositories {
            output.push_str("            {\n");
            writeln!(output, "                id = {},", gluon_string(&repository.id)).unwrap();
            writeln!(
                output,
                "                description = {},",
                gluon_optional_string(repository.description.as_deref())
            )
            .unwrap();
            encode_source(&mut output, &repository.source);
            writeln!(
                output,
                "                priority = {},",
                gluon_optional_integer(repository.priority)
            )
            .unwrap();
            writeln!(
                output,
                "                enabled = {},",
                gluon_optional_bool(repository.enabled)
            )
            .unwrap();
            output.push_str("            },\n");
        }
        output.push_str("        ],\n");
        output.push_str("    },\n");
    }
    output.push_str("]\n");
    output
}

fn encode_source(output: &mut String, source: &RepositorySourceSpec) {
    match source {
        RepositorySourceSpec::DirectIndex { uri } => {
            output.push_str("                source = DirectIndex {\n");
            writeln!(output, "                    uri = {},", gluon_string(uri)).unwrap();
            output.push_str("                },\n");
        }
        RepositorySourceSpec::RootIndex {
            base_uri,
            channel,
            version,
            arch,
        } => {
            output.push_str("                source = RootIndex {\n");
            writeln!(output, "                    base_uri = {},", gluon_string(base_uri)).unwrap();
            writeln!(
                output,
                "                    channel = {},",
                gluon_optional_string(channel.as_deref())
            )
            .unwrap();
            writeln!(output, "                    version = {},", gluon_string(version)).unwrap();
            writeln!(
                output,
                "                    arch = {},",
                gluon_optional_string(arch.as_deref())
            )
            .unwrap();
            output.push_str("                },\n");
        }
    }
}

fn gluon_optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {}", gluon_string(value)))
}

fn gluon_optional_integer(value: Option<i64>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {value}"))
}

fn gluon_optional_bool(value: Option<bool>) -> String {
    value.map_or_else(
        || "None".to_owned(),
        |value| format!("Some {}", if value { "True" } else { "False" }),
    )
}

fn gluon_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
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
        let loaded = env.config.load_gluon(&Evaluator::default(), &ProfileCodec)?;
        let fragments = loaded
            .iter()
            .map(|loaded| ProfileFragmentProvenance {
                logical_name: loaded.logical_name.clone(),
                evaluation: loaded.fingerprint.clone(),
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
        self.env.config.save_gluon(id, &map, &ProfileCodec)?;

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
    LoadProfiles(#[source] Box<config::LoadGluonError>),
    #[error("save profile")]
    SaveProfile(#[source] Box<config::SaveGluonError>),
}

impl From<config::LoadGluonError> for Error {
    fn from(error: config::LoadGluonError) -> Self {
        Self::LoadProfiles(Box::new(error))
    }
}

impl From<config::SaveGluonError> for Error {
    fn from(error: config::SaveGluonError) -> Self {
        Self::SaveProfile(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use config::{LoadGluonError, SaveGluonError};
    use fs_err as fs;

    use super::*;

    fn assert_portable_complete_fragment(fragment: &ProfileFragmentProvenance, host_root: &Path) {
        fragment.evaluation.validate().unwrap();
        assert!(!Path::new(&fragment.logical_name).is_absolute());
        assert!(!Path::new(&fragment.evaluation.root_logical_name).is_absolute());
        assert!(
            fragment
                .evaluation
                .imported_modules
                .iter()
                .all(|module| !Path::new(&module.logical_name).is_absolute())
        );

        let host_root = host_root.to_string_lossy();
        assert!(!fragment.logical_name.contains(host_root.as_ref()));
        assert!(!fragment.evaluation.root_logical_name.contains(host_root.as_ref()));
        assert!(
            fragment
                .evaluation
                .imported_modules
                .iter()
                .all(|module| !module.logical_name.contains(host_root.as_ref()))
        );
    }

    fn write(path: &Path, source: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn authored(body: &str) -> String {
        format!("let boulder = import! boulder.profile.v1\n{body}")
    }

    fn environment(config_root: &Path) -> Env {
        Env {
            cache_dir: config_root.join("cache"),
            data_dir: config_root.join("data"),
            moss_dir: config_root.join("moss"),
            config: config::Manager::custom(config_root),
        }
    }

    fn single_profile(body: &str) -> String {
        authored(&format!("boulder.profiles [boulder.profile \"test\" [{body}]]"))
    }

    fn conversion_error(source: String) -> (PathBuf, String) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.d/invalid.glu");
        write(&path, &source);
        let error = config::Manager::custom(temporary.path())
            .load_gluon(&Evaluator::default(), &ProfileCodec)
            .expect_err("profile should be invalid");
        let LoadGluonError::Conversion {
            path: error_path,
            source,
        } = error
        else {
            panic!("expected conversion error");
        };
        (error_path, source.to_string())
    }

    #[test]
    fn manager_loads_direct_root_and_repository_defaults() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.d/authored.glu");
        write(
            &path,
            &authored(
                r#"boulder.profiles [
    boulder.profile "test" [
        boulder.repository.direct "local" "file:///var/cache/local.index",
        boulder.repository.root "volatile" "https://packages.example.test" "stream/volatile",
    ],
]"#,
            ),
        );

        let env = environment(temporary.path());
        let manager = Manager::new(&env).unwrap();
        assert_eq!(
            manager
                .fragments
                .iter()
                .map(|fragment| fragment.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["authored"]
        );
        let fragment = &manager.fragments[0];
        assert_portable_complete_fragment(fragment, temporary.path());
        assert!(
            fragment
                .evaluation
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "boulder.profile.v1")
        );
        let repositories = manager.repositories(&Id::new("test")).unwrap();

        let local = repositories.get(&repository::Id::new("local")).unwrap();
        assert_eq!(local.description, "");
        assert_eq!(u64::from(local.priority), 0);
        assert!(local.active);
        assert!(matches!(&local.source, repository::Source::DirectIndex(_)));

        let volatile = repositories.get(&repository::Id::new("volatile")).unwrap();
        let repository::Source::RootIndex(root) = &volatile.source else {
            panic!("expected root-index repository");
        };
        assert_eq!(root.channel.as_ref(), repository::DEFAULT_CHANNEL);
        assert_eq!(root.arch, repository::DEFAULT_ARCH);
        assert_eq!(root.version.to_string(), "stream/volatile");
    }

    #[test]
    fn active_root_indexes_must_match_the_selected_build_architecture() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.d/authored.glu");
        write(
            &path,
            &authored(
                r#"boulder.profiles [
    boulder.profile "test" [
        boulder.repository.direct "local" "file:///var/cache/local.index",
        boulder.repository.root_index_with {
            id = "volatile",
            description = boulder.optional.some "volatile",
            base_uri = "https://packages.example.test",
            channel = boulder.optional.some "main",
            version = "stream/volatile",
            arch = boulder.optional.some "x86_64",
            priority = boulder.optional.some 0,
            enabled = boulder.optional.some boulder.boolean.true,
        },
        boulder.repository.root_index_with {
            id = "disabled-aarch64",
            description = boulder.optional.some "disabled",
            base_uri = "https://packages.example.test",
            channel = boulder.optional.some "main",
            version = "stream/volatile",
            arch = boulder.optional.some "aarch64",
            priority = boulder.optional.some 0,
            enabled = boulder.optional.some boulder.boolean.false,
        },
    ],
]"#,
            ),
        );

        let env = environment(temporary.path());
        let manager = Manager::new(&env).unwrap();
        let profile = Id::new("test");
        assert!(manager.repositories_for_architecture(&profile, "x86_64").is_ok());
        assert!(matches!(
            manager.repositories_for_architecture(&profile, "aarch64"),
            Err(Error::RepositoryArchitectureMismatch {
                profile: error_profile,
                repository: repository_id,
                configured,
                requested,
            }) if error_profile == profile
                && repository_id == repository::Id::new("volatile")
                && configured == "x86_64"
                && requested == "aarch64"
        ));
    }

    #[test]
    fn invalid_url_version_and_priority_report_exact_fields() {
        let (path, error) = conversion_error(single_profile(r#"boulder.repository.direct "broken" "not a url""#));
        assert!(path.ends_with("profile.d/invalid.glu"));
        assert!(error.contains("profiles[0].repositories[0].source.uri"));

        let (_, error) = conversion_error(single_profile(
            r#"boulder.repository.root "broken" "https://packages.example.test" "volatile""#,
        ));
        assert!(error.contains("profiles[0].repositories[0].source.version"));

        let (_, error) = conversion_error(single_profile(
            r#"boulder.repository.direct_with {
    id = "broken",
    description = boulder.optional.none,
    uri = "file:///valid.index",
    priority = boulder.optional.some (-1),
    enabled = boulder.optional.none,
}"#,
        ));
        assert!(error.contains("profiles[0].repositories[0].priority"));
    }

    #[test]
    fn malformed_fragment_is_returned_by_the_manager_with_its_path() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.d/malformed.glu");
        write(&path, "let value = in value");
        let env = environment(temporary.path());

        let error = match Manager::new(&env) {
            Ok(_) => panic!("malformed profile should fail"),
            Err(error) => error,
        };
        let Error::LoadProfiles(error) = error else {
            panic!("expected visible evaluation error");
        };
        let LoadGluonError::Evaluation {
            path: error_path,
            source,
        } = *error
        else {
            panic!("expected visible evaluation error");
        };
        assert_eq!(error_path, path);
        assert_eq!(source.source_name.as_deref(), Some("profile.d/malformed.glu"));
        assert!(source.span.is_some());
    }

    #[test]
    fn generated_save_is_deterministic_standalone_and_loadable() {
        let evaluator = Evaluator::default();
        let decoded = ProfileCodec
            .decode(
                &evaluator,
                &GluonSource::new(
                    "authored.glu",
                    authored(
                        r#"boulder.profiles [
    boulder.profile "z-profile" [
        boulder.repository.root "z-root" "https://packages.example.test" "stream/volatile",
        boulder.repository.direct "a-direct" "file:///var/cache/local.index",
    ],
    boulder.profile "a-profile" [],
]"#,
                    ),
                ),
            )
            .unwrap();
        let first = ProfileCodec.encode(&decoded.value).unwrap();
        let repeated = ProfileCodec.encode(&decoded.value).unwrap();
        assert_eq!(first, repeated);
        assert!(first.find("id = \"a-profile\"").unwrap() < first.find("id = \"z-profile\"").unwrap());
        assert!(first.find("id = \"a-direct\"").unwrap() < first.find("id = \"z-root\"").unwrap());

        let temporary = tempfile::tempdir().unwrap();
        let manager = config::Manager::custom(temporary.path());
        let path = manager.save_gluon("generated", &decoded.value, &ProfileCodec).unwrap();
        let generated = fs::read_to_string(path).unwrap();
        assert!(generated.starts_with(config::GENERATED_GLUON_MARKER));
        assert!(generated.contains("type ProfileSpec ="));
        assert!(!generated.contains("import!"));

        let loaded = manager.load_gluon(&evaluator, &ProfileCodec).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].value.get(&Id::new("a-profile")).is_some());
        assert!(loaded[0].value.get(&Id::new("z-profile")).is_some());
    }

    #[test]
    fn generated_save_refuses_to_overwrite_an_authored_fragment() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.d/owned.glu");
        let source = authored("boulder.profiles [boulder.profile \"owned\" []]");
        write(&path, &source);
        let manager = config::Manager::custom(temporary.path());
        let loaded = manager.load_gluon(&Evaluator::default(), &ProfileCodec).unwrap();

        let error = manager
            .save_gluon("owned", &loaded[0].value, &ProfileCodec)
            .expect_err("authored fragment must be protected");
        assert!(matches!(error, SaveGluonError::AuthoredFragment { path: ref error_path } if error_path == &path));
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn fragment_merge_order_is_deterministic() {
        let temporary = tempfile::tempdir().unwrap();
        write(
            &temporary.path().join("profile.d/z.glu"),
            &single_profile(r#"boulder.repository.direct "source" "file:///z.index""#),
        );
        write(
            &temporary.path().join("profile.d/a.glu"),
            &single_profile(r#"boulder.repository.direct "source" "file:///a.index""#),
        );
        let env = environment(temporary.path());
        let mut expected_fragments = None;

        for _ in 0..3 {
            let manager = Manager::new(&env).unwrap();
            assert_eq!(
                manager
                    .fragments
                    .iter()
                    .map(|fragment| fragment.logical_name.as_str())
                    .collect::<Vec<_>>(),
                ["a", "z"]
            );
            for fragment in &manager.fragments {
                assert_portable_complete_fragment(fragment, temporary.path());
            }
            if let Some(expected) = &expected_fragments {
                assert_eq!(&manager.fragments, expected);
            } else {
                expected_fragments = Some(manager.fragments.clone());
            }
            let source = manager
                .repositories(&Id::new("test"))
                .unwrap()
                .get(&repository::Id::new("source"))
                .unwrap();
            let repository::Source::DirectIndex(uri) = &source.source else {
                panic!("expected direct-index repository");
            };
            assert_eq!(uri.as_str(), "file:///z.index");
        }
    }

    #[test]
    fn saving_a_profile_refreshes_values_and_provenance_together() {
        let temporary = tempfile::tempdir().unwrap();
        let env = environment(temporary.path());
        let mut manager = Manager::new(&env).unwrap();

        manager
            .save_profile(
                Id::new("saved"),
                Profile {
                    repositories: repository::Map::default(),
                },
            )
            .unwrap();

        assert!(manager.profiles.get(&Id::new("saved")).is_some());
        assert_eq!(
            manager
                .fragments
                .iter()
                .map(|fragment| fragment.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["saved"]
        );
        assert_portable_complete_fragment(&manager.fragments[0], temporary.path());
    }

    #[test]
    fn repository_owned_default_profile_is_valid_gluon() {
        let decoded = ProfileCodec
            .decode(
                &Evaluator::default(),
                &GluonSource::new(
                    "default-x86_64.glu",
                    include_str!("../data/profile.d/default-x86_64.glu"),
                ),
            )
            .unwrap();
        let profile = decoded.value.get(&Id::new("default-x86_64")).unwrap();
        let volatile = profile.repositories.get(&repository::Id::new("volatile")).unwrap();
        assert_eq!(volatile.description, "AerynOS volatile stream (CDN)");
        assert!(volatile.active);
    }
}
