//! Engine-neutral conversion between repository specs and the canonical map.
//!
//! Declaration languages differ only in how a [`RepositorySpec`] is produced.
//! Validating those specs, rejecting duplicate identifiers, and rebuilding the
//! canonical [`repository::Map`] is the same work for every adapter, so it lives
//! here rather than inside one of them.

use std::{error::Error, fmt};

use super::{Map, Repository, Source};
use crate::{
    repository,
    system_model::spec::{RepositorySourceSpec, RepositorySpec},
};

/// Version of the embedded repository configuration API.
///
/// Versions the neutral spec contract, not any one declaration language.
pub const REPOSITORY_ABI_VERSION: u32 = 1;

/// Semantic repository conversion failure with a stable field path.
#[derive(Debug)]
pub struct RepositoryConversionError {
    path: String,
    message: String,
    source: Option<crate::system_model::spec::ConversionError>,
}

impl RepositoryConversionError {
    fn from_spec(index: usize, error: crate::system_model::spec::ConversionError) -> Self {
        let path = if error.path().is_empty() {
            format!("repositories[{index}]")
        } else {
            format!("repositories[{index}].{}", error.path())
        };
        Self {
            path,
            message: error.message().to_owned(),
            source: Some(error),
        }
    }

    fn duplicate(index: usize, id: &repository::Id) -> Self {
        Self {
            path: format!("repositories[{index}].id"),
            message: format!("duplicate repository identifier `{id}`"),
            source: None,
        }
    }

    pub(super) fn encode(path: String, error: impl fmt::Display) -> Self {
        Self {
            path,
            message: error.to_string(),
            source: None,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RepositoryConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid repository configuration at `{}`: {}",
            self.path, self.message
        )
    }
}

impl Error for RepositoryConversionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|error| error as &(dyn Error + 'static))
    }
}

/// Validate decoded specs in authored order and key them by normalized id.
pub(super) fn decode_specs(specs: Vec<RepositorySpec>) -> Result<Map, RepositoryConversionError> {
    let mut repositories = Map::default();
    for (index, spec) in specs.into_iter().enumerate() {
        let (id, repository) = <(repository::Id, Repository)>::try_from(spec)
            .map_err(|error| RepositoryConversionError::from_spec(index, error))?;
        if repositories.contains_id(&id) {
            return Err(RepositoryConversionError::duplicate(index, &id));
        }
        repositories.add(id, repository);
    }
    Ok(repositories)
}

/// Lower one live repository back into its neutral spec form.
pub(super) fn repository_to_spec(
    (id, value): (&repository::Id, &Repository),
) -> Result<RepositorySpec, RepositoryConversionError> {
    let priority = i64::try_from(u64::from(value.priority))
        .map_err(|error| RepositoryConversionError::encode(format!("repositories[\"{id}\"].priority"), error))?;
    let source = match &value.source {
        Source::DirectIndex(uri) => RepositorySourceSpec::DirectIndex { uri: uri.to_string() },
        Source::RootIndex(source) => RepositorySourceSpec::RootIndex {
            base_uri: source.base_uri.to_string(),
            channel: Some(source.channel.to_string()),
            version: source.version.to_string(),
            arch: Some(source.arch.clone()),
        },
    };

    Ok(RepositorySpec {
        id: id.to_string(),
        description: Some(value.description.clone()),
        source,
        priority: Some(priority),
        enabled: Some(value.active),
    })
}
