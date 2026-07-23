use std::collections::BTreeSet;

use astr::AStr;
use derive_more::{Debug, Display, From, Into};
use stone::{StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag};
use thiserror::Error;

use crate::{Dependency, Provider, dependency, request};

/// A package identifier constructed from metadata fields
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Display)]
#[debug("{_0:?}")]
pub struct Id(pub(super) AStr);

/// The name of a [`super::Package`]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, From, Into, Display)]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn contains(&self, text: &str) -> bool {
        self.0.contains(text)
    }
}

/// The metadata of a [`super::Package`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// Package name
    pub name: Name,
    /// Human readable version identifier
    pub version_identifier: String,
    /// Package release as set in stone.glu
    pub source_release: u64,
    /// Build machinery specific build release
    pub build_release: u64,
    /// Architecture this was built for
    pub architecture: String,
    /// Brief one line summary of the package
    pub summary: String,
    /// Description of the package
    pub description: String,
    /// The source-grouping ID
    pub source_id: String,
    /// Where'd we find this guy..
    pub homepage: String,
    /// Licenses this is available under
    pub licenses: Vec<String>,
    /// All dependencies
    pub dependencies: BTreeSet<Dependency>,
    /// All providers, including name()
    pub providers: BTreeSet<Provider>,
    /// All providers that conflict with this package
    pub conflicts: BTreeSet<Provider>,
    /// If relevant: uri to fetch from
    pub uri: Option<String>,
    /// If relevant: hash for the download
    pub hash: Option<String>,
    /// How big is this package in the repo..?
    pub download_size: Option<u64>,
}

impl Meta {
    pub fn from_stone_payload(payload: &[StonePayloadMetaRecord]) -> Result<Self, MissingMetaFieldError> {
        let name = find_meta_string(payload, StonePayloadMetaTag::Name)?;
        let version_identifier = find_meta_string(payload, StonePayloadMetaTag::Version)?;
        let source_release = find_meta_u64(payload, StonePayloadMetaTag::Release)?;
        let build_release = find_meta_u64(payload, StonePayloadMetaTag::BuildRelease)?;
        let architecture = find_meta_string(payload, StonePayloadMetaTag::Architecture)?;
        let summary = find_meta_string(payload, StonePayloadMetaTag::Summary)?;
        let description = find_meta_string(payload, StonePayloadMetaTag::Description)?;
        let source_id = find_meta_string(payload, StonePayloadMetaTag::SourceID)?;
        let homepage = find_meta_string(payload, StonePayloadMetaTag::Homepage)?;
        let uri = find_meta_string(payload, StonePayloadMetaTag::PackageURI).ok();
        let hash = find_meta_string(payload, StonePayloadMetaTag::PackageHash).ok();
        let download_size = find_meta_u64(payload, StonePayloadMetaTag::PackageSize).ok();

        let licenses = payload
            .iter()
            .filter_map(|meta| meta_string(meta, StonePayloadMetaTag::License))
            .collect();
        let dependencies = payload.iter().filter_map(meta_dependency).collect();
        let providers = payload
            .iter()
            .filter_map(meta_provider)
            // Add package name as provider
            .chain(Some(Provider {
                kind: dependency::Kind::PackageName,
                name: name.clone(),
            }))
            .collect();
        let conflicts = payload.iter().filter_map(meta_conflict).collect();

        Ok(Meta {
            name: Name::from(name),
            version_identifier,
            source_release,
            build_release,
            architecture,
            summary,
            description,
            source_id,
            homepage,
            licenses,
            dependencies,
            providers,
            conflicts,
            uri,
            hash,
            download_size,
        })
    }

    /// Decode the stricter metadata contract used by a repository index.
    ///
    /// Binary package metadata intentionally permits repository-only fields to
    /// be absent. An index entry does not: it is the authority for fetching a
    /// package and must therefore contain one unambiguous value for every
    /// singleton field and only relations that Forge can understand.
    pub(crate) fn from_repository_index_payload(
        payload: &[StonePayloadMetaRecord],
    ) -> Result<Self, RepositoryMetaError> {
        let name = repository_nonempty_string(payload, StonePayloadMetaTag::Name)?;
        let version_identifier = repository_nonempty_string(payload, StonePayloadMetaTag::Version)?;
        let source_release = repository_u64(payload, StonePayloadMetaTag::Release)?;
        let build_release = repository_u64(payload, StonePayloadMetaTag::BuildRelease)?;
        let architecture = repository_nonempty_string(payload, StonePayloadMetaTag::Architecture)?;
        let summary = repository_string(payload, StonePayloadMetaTag::Summary)?;
        let description = repository_string(payload, StonePayloadMetaTag::Description)?;
        let source_id = repository_nonempty_string(payload, StonePayloadMetaTag::SourceID)?;
        let homepage = repository_string(payload, StonePayloadMetaTag::Homepage)?;
        let uri = repository_nonempty_string(payload, StonePayloadMetaTag::PackageURI)?;
        let hash = repository_string(payload, StonePayloadMetaTag::PackageHash)?;
        let download_size = repository_u64(payload, StonePayloadMetaTag::PackageSize)?;

        i32::try_from(source_release).map_err(|_| RepositoryMetaError::IntegerOutOfRange {
            tag: StonePayloadMetaTag::Release,
            value: source_release,
        })?;
        i32::try_from(build_release).map_err(|_| RepositoryMetaError::IntegerOutOfRange {
            tag: StonePayloadMetaTag::BuildRelease,
            value: build_release,
        })?;
        i64::try_from(download_size).map_err(|_| RepositoryMetaError::IntegerOutOfRange {
            tag: StonePayloadMetaTag::PackageSize,
            value: download_size,
        })?;
        if download_size > request::DEFAULT_DOWNLOAD_LIMITS.max_bytes {
            return Err(RepositoryMetaError::PackageSizeExceedsPolicy {
                limit: request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
            });
        }

        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(RepositoryMetaError::InvalidPackageHash);
        }

        let mut licenses = Vec::new();
        let mut dependencies = BTreeSet::new();
        let mut providers = BTreeSet::new();
        let mut conflicts = BTreeSet::new();

        for record in payload {
            match record.tag {
                StonePayloadMetaTag::Name
                | StonePayloadMetaTag::Architecture
                | StonePayloadMetaTag::Version
                | StonePayloadMetaTag::Summary
                | StonePayloadMetaTag::Description
                | StonePayloadMetaTag::Homepage
                | StonePayloadMetaTag::SourceID
                | StonePayloadMetaTag::Release
                | StonePayloadMetaTag::BuildRelease
                | StonePayloadMetaTag::PackageURI
                | StonePayloadMetaTag::PackageHash
                | StonePayloadMetaTag::PackageSize => {
                    // Singleton count and primitive type were checked above.
                }
                StonePayloadMetaTag::License => match &record.primitive {
                    StonePayloadMetaPrimitive::String(license) => licenses.push(license.clone()),
                    _ => {
                        return Err(RepositoryMetaError::WrongPrimitive {
                            tag: record.tag,
                            expected: "string",
                        });
                    }
                },
                StonePayloadMetaTag::Depends => match &record.primitive {
                    StonePayloadMetaPrimitive::Dependency(kind, value) => {
                        let kind = dependency::Kind::from_stone_dependency(*kind)
                            .ok_or(RepositoryMetaError::UnknownRelationKind { tag: record.tag })?;
                        let dependency = Dependency::new(kind, value.clone())
                            .map_err(|_| RepositoryMetaError::InvalidRelation { tag: record.tag })?;
                        dependencies.insert(dependency);
                    }
                    _ => {
                        return Err(RepositoryMetaError::WrongPrimitive {
                            tag: record.tag,
                            expected: "dependency",
                        });
                    }
                },
                StonePayloadMetaTag::Provides | StonePayloadMetaTag::Conflicts => match &record.primitive {
                    StonePayloadMetaPrimitive::Provider(kind, value) => {
                        let kind = dependency::Kind::from_stone_dependency(*kind)
                            .ok_or(RepositoryMetaError::UnknownRelationKind { tag: record.tag })?;
                        let provider = Provider::new(kind, value.clone())
                            .map_err(|_| RepositoryMetaError::InvalidRelation { tag: record.tag })?;
                        if record.tag == StonePayloadMetaTag::Provides {
                            providers.insert(provider);
                        } else {
                            conflicts.insert(provider);
                        }
                    }
                    _ => {
                        return Err(RepositoryMetaError::WrongPrimitive {
                            tag: record.tag,
                            expected: "provider",
                        });
                    }
                },
                unsupported => return Err(RepositoryMetaError::UnsupportedTag(unsupported)),
            }
        }

        providers.insert(Provider::new(dependency::Kind::PackageName, name.clone()).map_err(|_| {
            RepositoryMetaError::InvalidRelation {
                tag: StonePayloadMetaTag::Name,
            }
        })?);

        Ok(Meta {
            name: Name::from(name),
            version_identifier,
            source_release,
            build_release,
            architecture,
            summary,
            description,
            source_id,
            homepage,
            licenses,
            dependencies,
            providers,
            conflicts,
            uri: Some(uri),
            hash: Some(hash),
            download_size: Some(download_size),
        })
    }

    pub fn to_stone_payload(self) -> Vec<StonePayloadMetaRecord> {
        vec![
            (
                StonePayloadMetaTag::Name,
                StonePayloadMetaPrimitive::String(self.name.to_string()),
            ),
            (
                StonePayloadMetaTag::Version,
                StonePayloadMetaPrimitive::String(self.version_identifier),
            ),
            (
                StonePayloadMetaTag::Release,
                StonePayloadMetaPrimitive::Uint64(self.source_release),
            ),
            (
                StonePayloadMetaTag::BuildRelease,
                StonePayloadMetaPrimitive::Uint64(self.build_release),
            ),
            (
                StonePayloadMetaTag::Architecture,
                StonePayloadMetaPrimitive::String(self.architecture),
            ),
            (
                StonePayloadMetaTag::Summary,
                StonePayloadMetaPrimitive::String(self.summary),
            ),
            (
                StonePayloadMetaTag::Description,
                StonePayloadMetaPrimitive::String(self.description),
            ),
            (
                StonePayloadMetaTag::SourceID,
                StonePayloadMetaPrimitive::String(self.source_id),
            ),
            (
                StonePayloadMetaTag::Homepage,
                StonePayloadMetaPrimitive::String(self.homepage),
            ),
        ]
        .into_iter()
        .chain(
            self.uri
                .map(|uri| (StonePayloadMetaTag::PackageURI, StonePayloadMetaPrimitive::String(uri))),
        )
        .chain(self.hash.map(|hash| {
            (
                StonePayloadMetaTag::PackageHash,
                StonePayloadMetaPrimitive::String(hash),
            )
        }))
        .chain(self.download_size.map(|size| {
            (
                StonePayloadMetaTag::PackageSize,
                StonePayloadMetaPrimitive::Uint64(size),
            )
        }))
        .chain(
            self.licenses
                .into_iter()
                .map(|license| (StonePayloadMetaTag::License, StonePayloadMetaPrimitive::String(license))),
        )
        .chain(self.dependencies.into_iter().map(|dep| {
            (
                StonePayloadMetaTag::Depends,
                StonePayloadMetaPrimitive::Dependency(dep.kind.into(), dep.name),
            )
        }))
        .chain(
            self.providers
                .into_iter()
                // We re-add this on ingestion / it's implied
                .filter(|provider| provider.kind != dependency::Kind::PackageName)
                .map(|provider| {
                    (
                        StonePayloadMetaTag::Provides,
                        StonePayloadMetaPrimitive::Provider(provider.kind.into(), provider.name),
                    )
                }),
        )
        .chain(
            self.conflicts
                .into_iter()
                // We re-add this on ingestion / it's implied
                .map(|conflict| {
                    (
                        StonePayloadMetaTag::Conflicts,
                        StonePayloadMetaPrimitive::Provider(conflict.kind.into(), conflict.name),
                    )
                }),
        )
        .map(|(tag, kind)| StonePayloadMetaRecord { tag, primitive: kind })
        .collect()
    }

    /// Return a reusable ID
    pub fn id(&self) -> Id {
        Id(format!(
            "{}-{}-{}.{}",
            self.name.0, self.version_identifier, self.source_release, self.architecture
        )
        .into())
    }
}

fn find_meta_string(
    meta: &[StonePayloadMetaRecord],
    tag: StonePayloadMetaTag,
) -> Result<String, MissingMetaFieldError> {
    meta.iter()
        .find_map(|meta| meta_string(meta, tag))
        .ok_or(MissingMetaFieldError(tag))
}

fn repository_record(
    payload: &[StonePayloadMetaRecord],
    tag: StonePayloadMetaTag,
) -> Result<&StonePayloadMetaRecord, RepositoryMetaError> {
    let mut records = payload.iter().filter(|record| record.tag == tag);
    let record = records.next().ok_or(RepositoryMetaError::MissingField(tag))?;
    let additional = records.count();
    if additional != 0 {
        return Err(RepositoryMetaError::DuplicateField {
            tag,
            count: additional + 1,
        });
    }
    Ok(record)
}

fn repository_string(
    payload: &[StonePayloadMetaRecord],
    tag: StonePayloadMetaTag,
) -> Result<String, RepositoryMetaError> {
    match &repository_record(payload, tag)?.primitive {
        StonePayloadMetaPrimitive::String(value) => Ok(value.clone()),
        _ => Err(RepositoryMetaError::WrongPrimitive {
            tag,
            expected: "string",
        }),
    }
}

fn repository_nonempty_string(
    payload: &[StonePayloadMetaRecord],
    tag: StonePayloadMetaTag,
) -> Result<String, RepositoryMetaError> {
    let value = repository_string(payload, tag)?;
    if value.is_empty() {
        return Err(RepositoryMetaError::EmptyField(tag));
    }
    Ok(value)
}

fn repository_u64(payload: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> Result<u64, RepositoryMetaError> {
    let value = match repository_record(payload, tag)?.primitive {
        StonePayloadMetaPrimitive::Int8(value) => u64::try_from(value),
        StonePayloadMetaPrimitive::Uint8(value) => Ok(u64::from(value)),
        StonePayloadMetaPrimitive::Int16(value) => u64::try_from(value),
        StonePayloadMetaPrimitive::Uint16(value) => Ok(u64::from(value)),
        StonePayloadMetaPrimitive::Int32(value) => u64::try_from(value),
        StonePayloadMetaPrimitive::Uint32(value) => Ok(u64::from(value)),
        StonePayloadMetaPrimitive::Int64(value) => u64::try_from(value),
        StonePayloadMetaPrimitive::Uint64(value) => Ok(value),
        _ => {
            return Err(RepositoryMetaError::WrongPrimitive {
                tag,
                expected: "non-negative integer",
            });
        }
    };
    value.map_err(|_| RepositoryMetaError::NegativeInteger(tag))
}

fn find_meta_u64(meta: &[StonePayloadMetaRecord], tag: StonePayloadMetaTag) -> Result<u64, MissingMetaFieldError> {
    meta.iter()
        .find_map(|meta| meta_u64(meta, tag))
        .ok_or(MissingMetaFieldError(tag))
}

fn meta_u64(meta: &StonePayloadMetaRecord, tag: StonePayloadMetaTag) -> Option<u64> {
    if meta.tag == tag {
        Some(match meta.primitive {
            StonePayloadMetaPrimitive::Int8(i) => i as _,
            StonePayloadMetaPrimitive::Uint8(i) => i as _,
            StonePayloadMetaPrimitive::Int16(i) => i as _,
            StonePayloadMetaPrimitive::Uint16(i) => i as _,
            StonePayloadMetaPrimitive::Int32(i) => i as _,
            StonePayloadMetaPrimitive::Uint32(i) => i as _,
            StonePayloadMetaPrimitive::Int64(i) => i as _,
            StonePayloadMetaPrimitive::Uint64(i) => i,
            _ => return None,
        })
    } else {
        None
    }
}

fn meta_string(meta: &StonePayloadMetaRecord, tag: StonePayloadMetaTag) -> Option<String> {
    match (meta.tag, &meta.primitive) {
        (meta_tag, StonePayloadMetaPrimitive::String(value)) if meta_tag == tag => Some(value.clone()),
        _ => None,
    }
}

fn meta_dependency(meta: &StonePayloadMetaRecord) -> Option<Dependency> {
    if let StonePayloadMetaPrimitive::Dependency(kind, name) = meta.primitive.clone() {
        Some(Dependency {
            kind: dependency::Kind::from_stone_dependency(kind)?,
            name,
        })
    } else {
        None
    }
}

fn meta_provider(meta: &StonePayloadMetaRecord) -> Option<Provider> {
    match (meta.tag, meta.primitive.clone()) {
        (StonePayloadMetaTag::Provides, StonePayloadMetaPrimitive::Provider(kind, name)) => Some(Provider {
            kind: dependency::Kind::from_stone_dependency(kind)?,
            name: name.clone(),
        }),
        _ => None,
    }
}

fn meta_conflict(meta: &StonePayloadMetaRecord) -> Option<Provider> {
    match (meta.tag, meta.primitive.clone()) {
        (StonePayloadMetaTag::Conflicts, StonePayloadMetaPrimitive::Provider(kind, name)) => Some(Provider {
            kind: dependency::Kind::from_stone_dependency(kind)?,
            name: name.clone(),
        }),
        _ => None,
    }
}

#[derive(Debug, Error)]
#[error("Missing metadata field: {0:?}")]
pub struct MissingMetaFieldError(pub StonePayloadMetaTag);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RepositoryMetaError {
    #[error("missing required repository metadata field {0:?}")]
    MissingField(StonePayloadMetaTag),
    #[error("repository metadata field {tag:?} occurs {count} times; expected exactly one")]
    DuplicateField { tag: StonePayloadMetaTag, count: usize },
    #[error("repository metadata field {tag:?} has the wrong primitive; expected {expected}")]
    WrongPrimitive {
        tag: StonePayloadMetaTag,
        expected: &'static str,
    },
    #[error("repository metadata field {0:?} is negative")]
    NegativeInteger(StonePayloadMetaTag),
    #[error("repository metadata field {0:?} must not be empty")]
    EmptyField(StonePayloadMetaTag),
    #[error("repository metadata field {tag:?} value {value} is outside the supported range")]
    IntegerOutOfRange { tag: StonePayloadMetaTag, value: u64 },
    #[error("repository PackageSize exceeds the package download limit of {limit} bytes")]
    PackageSizeExceedsPolicy { limit: u64 },
    #[error("repository PackageHash must be exactly 64 lowercase ASCII hexadecimal characters")]
    InvalidPackageHash,
    #[error("repository relation {tag:?} uses an unknown kind")]
    UnknownRelationKind { tag: StonePayloadMetaTag },
    #[error("repository relation {tag:?} is invalid")]
    InvalidRelation { tag: StonePayloadMetaTag },
    #[error("repository metadata tag {0:?} is not supported by this index format")]
    UnsupportedTag(StonePayloadMetaTag),
}

#[cfg(test)]
mod tests {
    use stone::StonePayloadMetaDependency;

    use super::*;

    fn string(tag: StonePayloadMetaTag, value: impl Into<String>) -> StonePayloadMetaRecord {
        StonePayloadMetaRecord {
            tag,
            primitive: StonePayloadMetaPrimitive::String(value.into()),
        }
    }

    fn integer(tag: StonePayloadMetaTag, value: u64) -> StonePayloadMetaRecord {
        StonePayloadMetaRecord {
            tag,
            primitive: StonePayloadMetaPrimitive::Uint64(value),
        }
    }

    fn valid_repository_payload() -> Vec<StonePayloadMetaRecord> {
        vec![
            string(StonePayloadMetaTag::Name, "example"),
            string(StonePayloadMetaTag::Architecture, "x86_64"),
            string(StonePayloadMetaTag::Version, "1.0"),
            string(StonePayloadMetaTag::Summary, "An example"),
            string(StonePayloadMetaTag::Description, "An example package"),
            string(StonePayloadMetaTag::Homepage, "https://example.test"),
            string(StonePayloadMetaTag::SourceID, "example"),
            integer(StonePayloadMetaTag::Release, 1),
            integer(StonePayloadMetaTag::BuildRelease, 1),
            string(
                StonePayloadMetaTag::PackageURI,
                "../../../pool/v0/e/example/example-1.0-1-1-x86_64.stone",
            ),
            string(StonePayloadMetaTag::PackageHash, "a".repeat(64)),
            integer(StonePayloadMetaTag::PackageSize, 4_096),
            string(StonePayloadMetaTag::License, "MPL-2.0"),
            StonePayloadMetaRecord {
                tag: StonePayloadMetaTag::Depends,
                primitive: StonePayloadMetaPrimitive::Dependency(
                    StonePayloadMetaDependency::PackageName,
                    "runtime".to_owned(),
                ),
            },
            StonePayloadMetaRecord {
                tag: StonePayloadMetaTag::Provides,
                primitive: StonePayloadMetaPrimitive::Provider(
                    StonePayloadMetaDependency::Binary,
                    "example".to_owned(),
                ),
            },
            StonePayloadMetaRecord {
                tag: StonePayloadMetaTag::Conflicts,
                primitive: StonePayloadMetaPrimitive::Provider(
                    StonePayloadMetaDependency::PackageName,
                    "old-example".to_owned(),
                ),
            },
        ]
    }

    #[test]
    fn strict_repository_meta_accepts_complete_typed_metadata() {
        let meta = Meta::from_repository_index_payload(&valid_repository_payload()).unwrap();

        assert_eq!(meta.name.as_str(), "example");
        assert_eq!(
            meta.hash.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(meta.download_size, Some(4_096));
        assert_eq!(meta.dependencies.len(), 1);
        assert_eq!(meta.providers.len(), 2);
        assert_eq!(meta.conflicts.len(), 1);
    }

    #[test]
    fn strict_repository_meta_requires_exactly_one_typed_singleton() {
        let mut missing = valid_repository_payload();
        missing.retain(|record| record.tag != StonePayloadMetaTag::PackageURI);
        assert_eq!(
            Meta::from_repository_index_payload(&missing).unwrap_err(),
            RepositoryMetaError::MissingField(StonePayloadMetaTag::PackageURI)
        );

        let mut duplicate = valid_repository_payload();
        duplicate.push(string(StonePayloadMetaTag::PackageHash, "b".repeat(64)));
        assert_eq!(
            Meta::from_repository_index_payload(&duplicate).unwrap_err(),
            RepositoryMetaError::DuplicateField {
                tag: StonePayloadMetaTag::PackageHash,
                count: 2,
            }
        );

        let mut wrong_type = valid_repository_payload();
        wrong_type
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::PackageSize)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::String("4096".to_owned());
        assert_eq!(
            Meta::from_repository_index_payload(&wrong_type).unwrap_err(),
            RepositoryMetaError::WrongPrimitive {
                tag: StonePayloadMetaTag::PackageSize,
                expected: "non-negative integer",
            }
        );
    }

    #[test]
    fn strict_repository_meta_rejects_negative_overflowing_and_noncanonical_values() {
        let mut negative = valid_repository_payload();
        negative
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::Release)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::Int64(-1);
        assert_eq!(
            Meta::from_repository_index_payload(&negative).unwrap_err(),
            RepositoryMetaError::NegativeInteger(StonePayloadMetaTag::Release)
        );

        let mut overflowing = valid_repository_payload();
        overflowing
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::BuildRelease)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::Uint64(i32::MAX as u64 + 1);
        assert_eq!(
            Meta::from_repository_index_payload(&overflowing).unwrap_err(),
            RepositoryMetaError::IntegerOutOfRange {
                tag: StonePayloadMetaTag::BuildRelease,
                value: i32::MAX as u64 + 1,
            }
        );

        let mut uppercase_hash = valid_repository_payload();
        uppercase_hash
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::PackageHash)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::String("A".repeat(64));
        assert_eq!(
            Meta::from_repository_index_payload(&uppercase_hash).unwrap_err(),
            RepositoryMetaError::InvalidPackageHash
        );

        let mut oversized_package = valid_repository_payload();
        oversized_package
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::PackageSize)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::Uint64(request::DEFAULT_DOWNLOAD_LIMITS.max_bytes + 1);
        assert_eq!(
            Meta::from_repository_index_payload(&oversized_package).unwrap_err(),
            RepositoryMetaError::PackageSizeExceedsPolicy {
                limit: request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
            }
        );

        let mut empty_version = valid_repository_payload();
        empty_version
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::Version)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::String(String::new());
        assert_eq!(
            Meta::from_repository_index_payload(&empty_version).unwrap_err(),
            RepositoryMetaError::EmptyField(StonePayloadMetaTag::Version)
        );
    }

    #[test]
    fn strict_repository_meta_rejects_wrong_or_unknown_relations() {
        let mut wrong_primitive = valid_repository_payload();
        wrong_primitive
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::Depends)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::String("name(runtime)".to_owned());
        assert_eq!(
            Meta::from_repository_index_payload(&wrong_primitive).unwrap_err(),
            RepositoryMetaError::WrongPrimitive {
                tag: StonePayloadMetaTag::Depends,
                expected: "dependency",
            }
        );

        let mut unknown_kind = valid_repository_payload();
        unknown_kind
            .iter_mut()
            .find(|record| record.tag == StonePayloadMetaTag::Provides)
            .unwrap()
            .primitive = StonePayloadMetaPrimitive::Provider(StonePayloadMetaDependency::Unknown, "example".to_owned());
        assert_eq!(
            Meta::from_repository_index_payload(&unknown_kind).unwrap_err(),
            RepositoryMetaError::UnknownRelationKind {
                tag: StonePayloadMetaTag::Provides,
            }
        );
    }
}
