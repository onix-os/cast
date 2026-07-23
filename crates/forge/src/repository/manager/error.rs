use std::{collections::TryReserveError, io, path::PathBuf, sync::Arc};

use config::declaration::{
    LoadManagedDeclarationError, SaveManagedDeclarationError,
};
use stone::{StoneHeaderV1FileType, StonePayloadKind, StonePayloadMetaTag, StoneReadError};
use thiserror::Error;
use url::Url;

use crate::{
    db::meta,
    package,
    repository::{self, Format, OutdatedRepoIndexUri, format},
};

use super::Source;

#[derive(Debug, Error)]
pub enum PackageUriError {
    #[error("package URI is empty")]
    Empty,
    #[error("package URI must be a relative reference")]
    AbsoluteReference,
    #[error("package URI cannot be resolved against the repository index")]
    Resolve(#[source] url::ParseError),
    #[error("resolved package URI contains credentials")]
    Credentials,
    #[error("resolved package URI contains a fragment")]
    Fragment,
    #[error("resolved package URI contains a query")]
    Query,
    #[error("resolved package URI changes repository origin")]
    CrossOrigin,
    #[error("package URI contains percent-encoded path traversal or separator bytes")]
    EncodedTraversal,
    #[error("derive package URI capability root")]
    Capability(#[source] url::ParseError),
    #[error("package URI escapes its configured repository capability root")]
    CapabilityEscape,
    #[error("file package URI has a non-local authority")]
    NonLocalFileAuthority,
    #[error("file package URI is not an absolute local path")]
    InvalidFilePath,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Can't modify repos when using explicit configs or authored system intent")]
    ExplicitUnsupported,
    #[error("invalid root-index architecture {0:?}")]
    InvalidRootArchitecture(String),
    #[error("repository URI violates transport policy: {reason}")]
    RepositoryTransportPolicy { reason: &'static str },
    #[error(
        "repository metadata queries for read-only installation {0:?} are unsupported; use an owner-authorized writable cache"
    )]
    ReadOnlyRepositoryCacheUnsupported(PathBuf),
    #[error("Missing metadata field: {0:?}")]
    MissingMetaField(StonePayloadMetaTag),
    #[error("create directory")]
    CreateDir(#[source] io::Error),
    #[error("remove directory")]
    RemoveDir(#[source] io::Error),
    #[error("prepare owned repository cache directory {path:?}")]
    PrepareCacheDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open descriptor-rooted repository metadata database {path:?}")]
    OpenRepositoryDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare descriptor-rooted repository metadata database {path:?}")]
    PrepareRepositoryDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("fetch index file")]
    FetchIndex(#[from] repository::FetchError),
    #[error("create private repository index candidate directory in {parent:?}")]
    CreateIndexCandidate {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open authenticated repository cache directory {path:?}")]
    OpenCacheDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open private repository index candidate directory {path:?}")]
    OpenIndexCandidateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("create immutable repository index directory {path:?}")]
    CreateIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open immutable repository index directory {path:?}")]
    OpenIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open repository mutation lock {path:?}")]
    OpenRepositoryMutationLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("lock repository mutation boundary {path:?}")]
    LockRepositoryMutation {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("lock stable repository snapshot view {path:?}")]
    LockRepositorySnapshot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid repository index path {0:?}")]
    InvalidIndexPath(PathBuf),
    #[error("invalid immutable repository index name for SHA-256 {0:?}")]
    InvalidImmutableIndexName(String),
    #[error("unexpected entry in immutable repository index directory: {0:?}")]
    InvalidIndexDirectoryEntry(PathBuf),
    #[error("open repository index {path:?}")]
    OpenIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect repository index {path:?}")]
    InspectIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository index is not a regular file: {0:?}")]
    IndexNotRegular(PathBuf),
    #[error("repository index directory {path:?} violates policy: {reason}")]
    IndexDirectoryPolicy { path: PathBuf, reason: &'static str },
    #[error("repository index {path:?} violates metadata policy: {reason}")]
    IndexMetadataPolicy { path: PathBuf, reason: &'static str },
    #[error("repository index changed while retained: {0:?}")]
    IndexChanged(PathBuf),
    #[error("repository index name no longer identifies the retained inode: {0:?}")]
    IndexPathChanged(PathBuf),
    #[error("read repository index {path:?}")]
    ReadIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository index {path:?} exceeds the {limit}-byte limit")]
    IndexTooLarge { path: PathBuf, limit: u64 },
    #[error("repository index {path:?} size mismatch: expected {expected}, got {actual}")]
    IndexSizeMismatch { path: PathBuf, expected: u64, actual: u64 },
    #[error("repository index {path:?} SHA-256 mismatch: expected {expected}, got {actual}")]
    IndexHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("reserve bounded repository index bytes")]
    ReserveIndexBytes(#[source] TryReserveError),
    #[error("prepare repository index candidate {path:?} for immutable publication")]
    PrepareIndexCandidate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync repository index file {path:?}")]
    SyncIndexFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("publish repository index candidate {source_path:?} as {target:?}")]
    PublishIndex {
        source_path: PathBuf,
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare published immutable repository index {path:?}")]
    PrepareImmutableIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync immutable repository index directory {path:?}")]
    SyncIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read immutable repository index directory {path:?}")]
    ReadIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("immutable repository index directory cannot exceed {limit} generations")]
    IndexGenerationLimit { limit: usize },
    #[error("immutable repository index directory cannot exceed {limit} bytes")]
    IndexGenerationByteLimit { limit: u64 },
    #[error("remove inactive immutable repository index generation {path:?}")]
    RemoveIndexGeneration {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository snapshot verification cache is poisoned for `{0}`")]
    VerificationCachePoisoned(repository::Id),
    #[error("read index file")]
    ReadStone(#[from] StoneReadError),
    #[error("repository index has file type {0:?}; expected Repository")]
    UnexpectedIndexFileType(StoneHeaderV1FileType),
    #[error("repository index payload {index} has kind {kind:?}; expected Meta")]
    UnexpectedIndexPayload { index: usize, kind: StonePayloadKind },
    #[error("reserve bounded repository package metadata")]
    ReserveIndexPackages(#[source] TryReserveError),
    #[error("reserve bounded repository package identities")]
    ReserveIndexPackageIds(#[source] TryReserveError),
    #[error("repository index metadata entry {index} is invalid")]
    InvalidRepositoryMeta {
        index: usize,
        #[source]
        source: package::RepositoryMetaError,
    },
    #[error("repository index metadata entry {index} violated a validated-field invariant")]
    RepositoryMetaInvariant { index: usize },
    #[error("repository index contains a duplicate package identity at entry {index}")]
    DuplicateIndexPackage { index: usize },
    #[error("repository index package URI at entry {index} is invalid")]
    InvalidRepositoryPackageUri {
        index: usize,
        #[source]
        source: PackageUriError,
    },
    #[error("meta db")]
    Database(#[from] meta::Error),
    #[error("save repository declaration")]
    SaveConfig(
        #[source]
        Box<SaveManagedDeclarationError<repository::RepositoryConversionError>>,
    ),
    #[error("load repository declarations")]
    LoadConfig(
        #[source]
        Box<LoadManagedDeclarationError<repository::RepositoryConversionError>>,
    ),
    #[error("unknown repo")]
    UnknownRepo(repository::Id),
    #[error("resolve history index uri from root index")]
    ResolveHistoryIndexUri(#[from] repository::ResolveHistoryIndexUriError),
    #[error("root index doesn't have version identifier {0}")]
    MissingRootIndexVersion(format::ScopedIdentifier),
    #[error("repository `{0}` has no active index snapshot; initialize or refresh it before resolution")]
    MissingActiveSnapshot(repository::Id),
    #[error("one or more repositories has an unsupported format")]
    UnsupportedRepos(Vec<UnsupportedRepoFormat>),
    #[error("one or more repositories with a legacy URI need to be upgraded to the new configuration format")]
    OutdatedRepos(Arc<Source>, Vec<OutdatedRepoIndexUri>),
}

impl From<SaveManagedDeclarationError<repository::RepositoryConversionError>>
    for Error
{
    fn from(
        error: SaveManagedDeclarationError<repository::RepositoryConversionError>,
    ) -> Self {
        Self::SaveConfig(Box::new(error))
    }
}

impl From<LoadManagedDeclarationError<repository::RepositoryConversionError>>
    for Error
{
    fn from(
        error: LoadManagedDeclarationError<repository::RepositoryConversionError>,
    ) -> Self {
        Self::LoadConfig(Box::new(error))
    }
}

impl From<package::MissingMetaFieldError> for Error {
    fn from(error: package::MissingMetaFieldError) -> Self {
        Self::MissingMetaField(error.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Removal {
    NotFound,
    ConfigDeleted(bool),
}

#[derive(Debug, Clone)]
pub struct UnsupportedRepoFormat {
    pub repository: repository::Cached,
    pub root_index_uri: Url,
    pub upgrade_via_index_uri: Option<Url>,
    pub version: format::ScopedIdentifier,
    pub format: Format,
}

pub(super) struct LegacyIndexUri {
    base_uri: Url,
    stream: LegacyIndexUriStream,
}

enum LegacyIndexUriStream {
    Volatile,
    Unstable,
}

impl LegacyIndexUri {
    pub(super) fn compatible_root_index_source(self) -> repository::RootIndexSource {
        repository::RootIndexSource {
            version: self.stream.version(),
            base_uri: self.base_uri,
            channel: repository::DEFAULT_CHANNEL.try_into().expect("valid identifier"),
            arch: repository::DEFAULT_ARCH.to_owned(),
        }
    }
}

impl LegacyIndexUriStream {
    fn version(&self) -> format::ScopedIdentifier {
        format::ScopedIdentifier::Stream(
            format::Identifier::new(match self {
                LegacyIndexUriStream::Volatile => "volatile",
                LegacyIndexUriStream::Unstable => "unstable",
            })
            .expect("valid ident"),
        )
    }
}

pub(super) fn identify_legacy_index_uri(uri: &Url) -> Option<LegacyIndexUri> {
    const DOMAINS: &[&str] = &[
        "dev.serpentos.com",
        "packages.aerynos.com",
        "cdn.aerynos.dev",
        "packages.aerynos.dev",
        "build.aerynos.dev",
        "infratest.aerynos.dev",
    ];
    const VOLATILE_PATHS: &[&str] = &["/volatile/x86_64/stone.index", "/stream/volatile/x86_64/stone.index"];
    const UNSTABLE_PATHS: &[&str] = &["/unstable/x86_64/stone.index", "/stream/unstable/x86_64/stone.index"];

    let mut stream = None;

    if uri.domain().is_some_and(|domain| DOMAINS.contains(&domain)) {
        if VOLATILE_PATHS.contains(&uri.path()) {
            stream = Some(LegacyIndexUriStream::Volatile);
        }

        if UNSTABLE_PATHS.contains(&uri.path()) {
            stream = Some(LegacyIndexUriStream::Unstable);
        }
    }

    if let Some(stream) = stream {
        let mut base_uri = uri.clone();
        base_uri.set_path("");

        Some(LegacyIndexUri { base_uri, stream })
    } else {
        None
    }
}
