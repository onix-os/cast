use std::{
    collections::HashSet,
    ffi::CStr,
    io::Cursor,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    },
    path::PathBuf,
    sync::Arc,
};

use fs_err as fs;
use stone::{StoneDecodeLimits, StoneDecodedPayload, StoneHeader, StoneHeaderV1FileType};
use url::Url;

use crate::{
    package,
    repository::{self, Format, OutdatedRepoIndexUri, Repository},
};

use super::{
    INDEX_CANDIDATE_NAME, Source,
    error::{Error, PackageUriError, UnsupportedRepoFormat, identify_legacy_index_uri},
    index_storage::{
        descendant_resolution, directory_owner, inspect_file, openat2_file, proc_fd_path, require_directory_owned,
        require_name_witness, require_regular_owned,
    },
};
pub(super) fn repository_index_decode_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 8_192,
        max_records_per_payload: 512,
        max_record_bytes: 64 * 1024,
        max_stored_payload_bytes: 64 * 1024,
        max_plain_payload_bytes: 256 * 1024,
        max_total_records: 262_144,
        max_total_record_bytes: 16 * 1024 * 1024,
        max_total_stored_bytes: 8 * 1024 * 1024,
        max_total_plain_bytes: 16 * 1024 * 1024,
        max_zstd_window_log: 20,
    }
}

pub(super) struct FetchedIndex {
    // Field order matters: TempDir cleanup uses the retained cache descriptor's
    // `/proc/self/fd` path, so it must be dropped before that descriptor.
    pub(super) _directory: tempfile::TempDir,
    pub(super) _cache_directory: fs::File,
    pub(super) directory: fs::File,
    pub(super) file: fs::File,
    pub(super) path: PathBuf,
    pub(super) index_uri: Url,
}

/// Download one candidate below an already authenticated cache descriptor.
/// The downloader receives an intentional `/proc/self/fd/<n>` capability path;
/// every trusted read and the eventual rename remain descriptor-relative.
pub(super) async fn fetch_index(
    source: &Arc<Source>,
    state: &repository::Cached,
    cache_directory: &fs::File,
) -> Result<FetchedIndex, Error> {
    let index_uri = match &state.repository.source {
        repository::Source::DirectIndex(uri) => match identify_legacy_index_uri(uri) {
            None => uri.clone(),
            Some(legacy_index) => {
                let root_source = legacy_index.compatible_root_index_source();
                if let Ok(root_index) = root_source.fetch_root_index().await
                    && let Some((_, history)) = root_index.resolve_version_to_history(&root_source.version)
                    && history.format != Format::Legacy
                {
                    return Err(Error::OutdatedRepos(
                        source.clone(),
                        vec![OutdatedRepoIndexUri {
                            repository: state.clone(),
                            legacy_index_uri: uri.clone(),
                            compatible_root_index_source: root_source,
                        }],
                    ));
                }
                uri.clone()
            }
        },
        repository::Source::RootIndex(source) => resolve_index_from_root(state, source).await?,
    };

    let cache_clone = cache_directory
        .try_clone()
        .map_err(|source| Error::OpenCacheDirectory {
            path: state.cache_dir.clone(),
            source,
        })?;
    let capability = proc_fd_path(&cache_clone);
    let directory = tempfile::Builder::new()
        .prefix(".index-candidate-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&capability)
        .map_err(|source| Error::CreateIndexCandidate {
            parent: state.cache_dir.clone(),
            source,
        })?;
    let name = directory
        .path()
        .file_name()
        .ok_or_else(|| Error::InvalidIndexPath(directory.path().to_owned()))?;
    let directory_display = state.cache_dir.join(name);
    let directory_file = openat2_file(
        cache_directory.as_raw_fd(),
        name.as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &directory_display,
    )
    .map_err(|source| Error::OpenIndexCandidateDirectory {
        path: directory_display.clone(),
        source,
    })?;
    let cache_owner = directory_owner(cache_directory, &state.cache_dir)?;
    require_directory_owned(&directory_file, &directory_display, cache_owner, Some(0o700))?;

    let path = proc_fd_path(&directory_file).join(INDEX_CANDIDATE_NAME);
    repository::fetch_index(index_uri.clone(), &path).await?;
    let display_path = directory_display.join(INDEX_CANDIDATE_NAME);
    let file = openat2_file(
        directory_file.as_raw_fd(),
        INDEX_CANDIDATE_NAME.as_bytes(),
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &display_path,
    )
    .map_err(|source| Error::OpenIndex {
        path: display_path.clone(),
        source,
    })?;
    let witness = inspect_file(&file, &display_path)?;
    require_regular_owned(&display_path, witness, cache_owner, None)?;
    require_name_witness(
        &directory_file,
        CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string"),
        witness,
        &display_path,
    )?;

    Ok(FetchedIndex {
        _directory: directory,
        _cache_directory: cache_clone,
        directory: directory_file,
        file,
        path: display_path,
        index_uri,
    })
}

pub(super) fn decode_repository_index(
    bytes: &[u8],
    source: &repository::Source,
    index_uri: &Url,
) -> Result<Vec<(package::Id, package::Meta)>, Error> {
    let mut cursor = Cursor::new(bytes);
    let mut reader = stone::read_with_limits(&mut cursor, repository_index_decode_limits())?;
    let file_type = match reader.header {
        StoneHeader::V1(header) => header.file_type,
    };
    if file_type != StoneHeaderV1FileType::Repository {
        return Err(Error::UnexpectedIndexFileType(file_type));
    }

    let package_count = usize::from(reader.header.num_payloads());
    let mut packages = Vec::new();
    packages
        .try_reserve_exact(package_count)
        .map_err(Error::ReserveIndexPackages)?;
    let mut package_ids = HashSet::new();
    package_ids
        .try_reserve(package_count)
        .map_err(Error::ReserveIndexPackageIds)?;

    for (index, payload) in reader.payloads()?.enumerate() {
        let payload = payload?;
        let StoneDecodedPayload::Meta(payload) = payload else {
            return Err(Error::UnexpectedIndexPayload {
                index,
                kind: payload.header().kind,
            });
        };
        let mut meta = package::Meta::from_repository_index_payload(&payload.body)
            .map_err(|source| Error::InvalidRepositoryMeta { index, source })?;
        let hash = meta.hash.clone().ok_or(Error::RepositoryMetaInvariant { index })?;
        let id = package::Id::from(hash);
        if !package_ids.insert(id.clone()) {
            return Err(Error::DuplicateIndexPackage { index });
        }
        let raw_uri = meta.uri.as_deref().ok_or(Error::RepositoryMetaInvariant { index })?;
        let uri = normalize_repository_package_uri(source, index_uri, raw_uri)
            .map_err(|source| Error::InvalidRepositoryPackageUri { index, source })?;
        meta.uri = Some(uri.into());
        packages.push((id, meta));
    }

    Ok(packages)
}

pub(super) fn validate_repository_source(repository: &Repository) -> Result<(), Error> {
    match &repository.source {
        repository::Source::DirectIndex(uri) => validate_repository_transport(uri),
        repository::Source::RootIndex(source) => {
            validate_repository_transport(&source.base_uri)?;
            let arch = source.arch.as_bytes();
            if arch.is_empty()
                || arch.len() > 64
                || matches!(arch, b"." | b"..")
                || !arch
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(Error::InvalidRootArchitecture(source.arch.clone()));
            }
            Ok(())
        }
    }
}

pub(super) fn validate_repository_transport(uri: &Url) -> Result<(), Error> {
    if !uri.username().is_empty() || uri.password().is_some() || uri.fragment().is_some() {
        return Err(Error::RepositoryTransportPolicy {
            reason: "credentials and fragments are forbidden",
        });
    }
    match uri.scheme() {
        "https" if uri.host_str().is_some() => Ok(()),
        "file" if uri.host_str().is_none() && uri.query().is_none() && uri.to_file_path().is_ok() => Ok(()),
        "http" => Err(Error::RepositoryTransportPolicy {
            reason: "plaintext HTTP repositories are forbidden; use HTTPS or a local file repository",
        }),
        _ => Err(Error::RepositoryTransportPolicy {
            reason: "repository transport must be HTTPS or a local file URL without authority/query",
        }),
    }
}

pub(super) fn normalize_repository_package_uri(
    source: &repository::Source,
    index_uri: &Url,
    raw_uri: &str,
) -> Result<Url, PackageUriError> {
    if raw_uri.is_empty() {
        return Err(PackageUriError::Empty);
    }
    if raw_uri.as_bytes().contains(&b'\\') || contains_encoded_path_control(raw_uri) {
        return Err(PackageUriError::EncodedTraversal);
    }
    if Url::parse(raw_uri).is_ok() {
        return Err(PackageUriError::AbsoluteReference);
    }

    let resolved = index_uri.join(raw_uri).map_err(PackageUriError::Resolve)?;
    if !resolved.username().is_empty() || resolved.password().is_some() {
        return Err(PackageUriError::Credentials);
    }
    if resolved.fragment().is_some() {
        return Err(PackageUriError::Fragment);
    }
    if resolved.query().is_some() {
        return Err(PackageUriError::Query);
    }
    if resolved.scheme() != index_uri.scheme()
        || resolved.host_str() != index_uri.host_str()
        || resolved.port_or_known_default() != index_uri.port_or_known_default()
    {
        return Err(PackageUriError::CrossOrigin);
    }

    let capability = match source {
        repository::Source::RootIndex(root) => Some(root_package_capability(root)),
        repository::Source::DirectIndex(_) if index_uri.scheme() == "file" => {
            Some(index_uri.join("./").map_err(PackageUriError::Capability)?)
        }
        repository::Source::DirectIndex(_) => None,
    };
    if let Some(capability) = capability {
        if !url_within_capability(index_uri, &capability) || !url_within_capability(&resolved, &capability) {
            return Err(PackageUriError::CapabilityEscape);
        }
    }
    if resolved.scheme() == "file" {
        if resolved.host_str().is_some() {
            return Err(PackageUriError::NonLocalFileAuthority);
        }
        let path = resolved.to_file_path().map_err(|_| PackageUriError::InvalidFilePath)?;
        let base = index_uri
            .join("./")
            .map_err(PackageUriError::Capability)?
            .to_file_path()
            .map_err(|_| PackageUriError::InvalidFilePath)?;
        let required_base = match source {
            repository::Source::RootIndex(root) => root_package_capability(root)
                .to_file_path()
                .map_err(|_| PackageUriError::InvalidFilePath)?,
            repository::Source::DirectIndex(_) => base,
        };
        if !path.starts_with(&required_base) {
            return Err(PackageUriError::CapabilityEscape);
        }
    }

    Ok(resolved)
}

/// Build the package capability with the same directory-style base semantics
/// as [`repository::RootIndexSource::uri`] and `history_index_uri`. `Url::join`
/// would treat a base without a trailing slash as a file and drop its final
/// path component.
fn root_package_capability(root: &repository::RootIndexSource) -> Url {
    let mut capability = root.base_uri.clone();
    let mut path = capability.path().to_owned();
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(root.channel.as_ref());
    path.push('/');
    capability.set_path(&path);
    capability
}

fn contains_encoded_path_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let Some(high) = (window[1] as char).to_digit(16) else {
            return false;
        };
        let Some(low) = (window[2] as char).to_digit(16) else {
            return false;
        };
        matches!(((high << 4) | low) as u8, b'.' | b'/' | b'\\' | 0)
    })
}

fn url_within_capability(url: &Url, capability: &Url) -> bool {
    url.scheme() == capability.scheme()
        && url.host_str() == capability.host_str()
        && url.port_or_known_default() == capability.port_or_known_default()
        && url.path().starts_with(capability.path())
}

async fn resolve_index_from_root(
    state: &repository::Cached,
    source: &repository::RootIndexSource,
) -> Result<Url, Error> {
    let index_uri = match source.resolve_history_index_uri().await? {
        repository::ResolvedHistoryIndexUri::Supported(uri) => uri,
        repository::ResolvedHistoryIndexUri::Unsupported {
            format,
            version,
            root_index_uri,
            upgrade_via_index_uri,
        } => {
            return Err(Error::UnsupportedRepos(vec![UnsupportedRepoFormat {
                repository: state.clone(),
                root_index_uri,
                upgrade_via_index_uri,
                version,
                format,
            }]));
        }
    };

    Ok(index_uri)
}
