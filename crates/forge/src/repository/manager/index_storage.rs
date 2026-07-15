use std::{
    ffi::{CStr, CString, OsStr},
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::Arc,
};

use fs_err as fs;
use sha2::{Digest as _, Sha256};

use crate::{
    Installation,
    db::meta,
    repository::{self, Repository},
};

use super::{
    IMMUTABLE_INDEX_DIRECTORY, IMMUTABLE_INDEX_EXTENSION, INDEX_IDENTITY_BUFFER_SIZE, MAX_INDEX_GENERATION_BYTES,
    MAX_INDEX_GENERATIONS,
    error::Error,
    snapshot::{RepositoryMutationLock, verify_mutation_boundary},
    source_validation::FetchedIndex,
};
/// Directory for the repo cached data (db & stone index), hashed by identifier & repo URI
pub(super) fn cache_dir(
    identifier: &str,
    id: &repository::Id,
    repo: &Repository,
    installation: &Installation,
) -> PathBuf {
    // Repository identity is part of the namespace. Two authored IDs may use
    // the same source intentionally, but must never share DB state, mutation
    // locks, immutable generations, or removal lifetime.
    let mut hasher = Sha256::new();
    for component in ["repository-cache-v1", identifier, id.as_ref()] {
        hasher.update(component.len().to_be_bytes());
        hasher.update(component.as_bytes());
    }
    match &repo.source {
        repository::Source::DirectIndex(uri) => {
            hasher.update(b"direct");
            hasher.update(uri.as_str().len().to_be_bytes());
            hasher.update(uri.as_str().as_bytes());
        }
        repository::Source::RootIndex(repository::RootIndexSource {
            base_uri,
            channel,
            version,
            arch,
        }) => {
            hasher.update(b"root");
            for component in [base_uri.as_str(), channel.as_ref(), &version.to_string(), arch.as_str()] {
                hasher.update(component.len().to_be_bytes());
                hasher.update(component.as_bytes());
            }
        }
    }
    installation.repo_path(hex::encode(hasher.finalize()))
}

/// Open the meta db file, ensuring it's
/// directory exists
pub(super) fn open_meta_db(
    identifier: &str,
    id: &repository::Id,
    repo: &Repository,
    installation: &Installation,
) -> Result<(meta::Database, PathBuf), Error> {
    let dir = cache_dir(identifier, id, repo, installation);

    let created = match fs::create_dir(&dir) {
        Ok(()) => true,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => return Err(Error::CreateDir(source)),
    };
    let directory = open_cache_path(&dir)?;
    if created {
        directory
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::PrepareCacheDirectory {
                path: dir.clone(),
                source,
            })?;
        sync_directory_file(&directory, &dir)?;
        let parent = dir.parent().ok_or_else(|| Error::InvalidIndexPath(dir.clone()))?;
        let parent_directory = open_directory_path(parent).map_err(|source| Error::OpenCacheDirectory {
            path: parent.to_owned(),
            source,
        })?;
        sync_directory_file(&parent_directory, parent)?;
    }
    let owner = directory_owner(&directory, &dir)?;
    let db_path = dir.join("db");
    let (db_file, db_created) = open_or_create_repository_db(&directory, &db_path)?;
    let mut db_witness = inspect_file(&db_file, &db_path)?;
    require_regular_owned(&db_path, db_witness, owner, None)?;
    if db_witness.mode & 0o022 != 0 {
        return Err(Error::IndexMetadataPolicy {
            path: db_path,
            reason: "repository metadata database is writable by group or other users",
        });
    }
    if db_created {
        db_file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| Error::PrepareRepositoryDatabase {
                path: dir.join("db"),
                source,
            })?;
        db_file.sync_all().map_err(|source| Error::SyncIndexFile {
            path: dir.join("db"),
            source,
        })?;
        sync_directory_file(&directory, &dir)?;
        db_witness = inspect_file(&db_file, &dir.join("db"))?;
    }
    let db_identity = (db_witness.device, db_witness.inode);
    let directory = Arc::new(directory);
    let anchored_db_path = proc_fd_path(&directory).join("db");
    let db = meta::Database::new_anchored(anchored_db_path.to_str().unwrap_or_default(), directory.clone())?;
    let current = witness_at(
        &directory,
        CStr::from_bytes_with_nul(b"db\0").expect("static C string"),
        &dir.join("db"),
    )?
    .ok_or_else(|| Error::IndexPathChanged(dir.join("db")))?;
    if (current.device, current.inode) != db_identity {
        return Err(Error::IndexPathChanged(dir.join("db")));
    }

    Ok((db, dir))
}

fn open_or_create_repository_db(directory: &fs::File, path: &Path) -> Result<(fs::File, bool), Error> {
    let flags =
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CREAT;
    match openat2_file(
        directory.as_raw_fd(),
        b"db",
        flags | nix::libc::O_EXCL,
        0o600,
        descendant_resolution(),
        path,
    ) {
        Ok(file) => Ok((file, true)),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => openat2_file(
            directory.as_raw_fd(),
            b"db",
            flags & !nix::libc::O_CREAT,
            0,
            descendant_resolution(),
            path,
        )
        .map(|file| (file, false))
        .map_err(|source| Error::OpenRepositoryDatabase {
            path: path.to_owned(),
            source,
        }),
        Err(source) => Err(Error::OpenRepositoryDatabase {
            path: path.to_owned(),
            source,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            length: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn from_stat(stat: &nix::libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            links: stat.st_nlink,
            uid: stat.st_uid,
            gid: stat.st_gid,
            length: stat.st_size.try_into().unwrap_or(u64::MAX),
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexIdentity {
    pub(super) sha256: String,
    pub(super) byte_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DirectoryOwner {
    device: u64,
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

pub(super) fn directory_identity(directory: &fs::File, path: &Path) -> Result<DirectoryIdentity, Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.mode() & 0o022 != 0
        || metadata.uid() != nix::unistd::Uid::effective().as_raw()
    {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache must be a directory not writable by group or other users",
        });
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
    })
}

pub(super) fn directory_owner(directory: &fs::File, path: &Path) -> Result<DirectoryOwner, Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "not a directory",
        });
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache directory is writable by group or other users",
        });
    }
    // Never adopt an attacker-precreated cache namespace, including when Cast
    // is privileged. Group may legitimately be inherited from a setgid parent,
    // but ownership must be the effective caller.
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache directory is not owned by the effective user",
        });
    }
    Ok(DirectoryOwner {
        device: metadata.dev(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

pub(super) fn require_directory_owned(
    directory: &fs::File,
    path: &Path,
    owner: DirectoryOwner,
    exact_mode: Option<u32>,
) -> Result<(), Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != owner.device
        || metadata.uid() != owner.uid
        || metadata.gid() != owner.gid
        || exact_mode.is_some_and(|mode| metadata.mode() & 0o7777 != mode)
    {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "directory type, filesystem, ownership, or mode does not match the repository cache",
        });
    }
    Ok(())
}

pub(super) fn inspect_file(file: &fs::File, path: &Path) -> Result<FileWitness, Error> {
    file.metadata()
        .map(|metadata| FileWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectIndex {
            path: path.to_owned(),
            source,
        })
}

pub(super) fn require_regular_owned(
    path: &Path,
    witness: FileWitness,
    owner: DirectoryOwner,
    exact_mode: Option<u32>,
) -> Result<(), Error> {
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(Error::IndexNotRegular(path.to_owned()));
    }
    if witness.device != owner.device {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file is not on the repository cache filesystem",
        });
    }
    if witness.links != 1 {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file must have exactly one hard link",
        });
    }
    if witness.uid != owner.uid || witness.gid != owner.gid {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file ownership does not match the repository cache",
        });
    }
    if exact_mode.is_some_and(|mode| witness.mode & 0o7777 != mode) {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file mode does not match the required immutable mode",
        });
    }
    Ok(())
}

/// Read one retained inode into a bounded buffer. The complete metadata
/// witness must be unchanged across the read; decoding later consumes only
/// this buffer, never a replaceable path or a second file.
pub(super) fn read_index_bytes(file: &fs::File, path: &Path) -> Result<(Vec<u8>, FileWitness), Error> {
    let before = inspect_file(file, path)?;
    let limit = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS.max_bytes;
    if before.length > limit {
        return Err(Error::IndexTooLarge {
            path: path.to_owned(),
            limit,
        });
    }
    let initial = usize::try_from(before.length).map_err(|_| Error::IndexTooLarge {
        path: path.to_owned(),
        limit,
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(initial).map_err(Error::ReserveIndexBytes)?;
    let mut buffer = [0_u8; INDEX_IDENTITY_BUFFER_SIZE];
    let mut offset = 0_u64;
    loop {
        let remaining = limit.saturating_sub(offset);
        let requested = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = loop {
            // SAFETY: buffer and descriptor remain live; offset is bounded by
            // the download limit and therefore representable by off_t here.
            let result = unsafe {
                nix::libc::pread(
                    file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    requested,
                    offset as nix::libc::off_t,
                )
            };
            if result >= 0 {
                break result as usize;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::ReadIndex {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(Error::IndexTooLarge {
                path: path.to_owned(),
                limit,
            });
        }
        bytes.try_reserve(read).map_err(Error::ReserveIndexBytes)?;
        bytes.extend_from_slice(&buffer[..read]);
        offset += read as u64;
    }
    let after = inspect_file(file, path)?;
    if before != after || after.length != offset {
        return Err(Error::IndexChanged(path.to_owned()));
    }
    Ok((bytes, after))
}

pub(super) fn index_identity(bytes: &[u8]) -> IndexIdentity {
    IndexIdentity {
        sha256: hex::encode(Sha256::digest(bytes)),
        byte_size: bytes.len() as u64,
    }
}

pub(super) fn verify_identity(path: &Path, bytes: &[u8], expected: &IndexIdentity) -> Result<(), Error> {
    let actual = index_identity(bytes);
    if actual.byte_size != expected.byte_size {
        return Err(Error::IndexSizeMismatch {
            path: path.to_owned(),
            expected: expected.byte_size,
            actual: actual.byte_size,
        });
    }
    if actual.sha256 != expected.sha256 {
        return Err(Error::IndexHashMismatch {
            path: path.to_owned(),
            expected: expected.sha256.clone(),
            actual: actual.sha256,
        });
    }
    Ok(())
}

pub(super) fn immutable_index_path(state: &repository::Cached, sha256: &str) -> PathBuf {
    state
        .cache_dir
        .join(IMMUTABLE_INDEX_DIRECTORY)
        .join(format!("{sha256}.{IMMUTABLE_INDEX_EXTENSION}"))
}

pub(super) fn immutable_index_name(sha256: &str) -> Result<CString, Error> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::InvalidImmutableIndexName(sha256.to_owned()));
    }
    CString::new(format!("{sha256}.{IMMUTABLE_INDEX_EXTENSION}"))
        .map_err(|_| Error::InvalidImmutableIndexName(sha256.to_owned()))
}

pub(super) fn sync_directory_file(directory: &fs::File, path: &Path) -> Result<(), Error> {
    directory.sync_all().map_err(|source| Error::SyncIndexDirectory {
        path: path.to_owned(),
        source,
    })
}

pub(super) fn open_cache_directory(state: &repository::Cached) -> Result<fs::File, Error> {
    open_cache_path(&state.cache_dir)
}

fn open_cache_path(path: &Path) -> Result<fs::File, Error> {
    open_directory_path(path).map_err(|source| Error::OpenCacheDirectory {
        path: path.to_owned(),
        source,
    })
}

fn open_directory_path(path: &Path) -> io::Result<fs::File> {
    openat2_file(
        nix::libc::AT_FDCWD,
        path.as_os_str().as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS,
        path,
    )
}

pub(super) fn open_indexes_directory(
    state: &repository::Cached,
    cache_directory: &fs::File,
    create: bool,
) -> Result<fs::File, Error> {
    let path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
    if create {
        let name = CStr::from_bytes_with_nul(b"indexes\0").expect("static C string");
        // SAFETY: cache descriptor and static single-component name are live.
        if unsafe { nix::libc::mkdirat(cache_directory.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::AlreadyExists {
                return Err(Error::CreateIndexDirectory { path, source });
            }
        }
    }
    let directory = openat2_file(
        cache_directory.as_raw_fd(),
        IMMUTABLE_INDEX_DIRECTORY.as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &path,
    )
    .map_err(|source| Error::OpenIndexDirectory {
        path: path.clone(),
        source,
    })?;
    let owner = directory_owner(cache_directory, &state.cache_dir)?;
    require_directory_owned(&directory, &path, owner, Some(0o700))?;
    Ok(directory)
}

struct DirectoryStream(NonNull<nix::libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe {
            nix::libc::closedir(self.0.as_ptr());
        }
    }
}

fn immutable_index_entry_names(directory: &fs::File, path: &Path) -> Result<Vec<CString>, Error> {
    // Open a fresh directory description rather than dup'ing `directory`:
    // dup would share its enumeration offset with earlier calls.
    let cursor = openat2_file(
        directory.as_raw_fd(),
        b".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        path,
    )
    .map_err(|source| Error::ReadIndexDirectory {
        path: path.to_owned(),
        source,
    })?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh owned directory descriptor on
    // success; on failure it remains ours and is closed below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadIndexDirectory {
            path: path.to_owned(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local on Linux and readdir uses null for
        // both end-of-directory and failure.
        unsafe {
            *nix::libc::__errno_location() = 0;
        }
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::ReadIndexDirectory {
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream; copy it before advancing.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if names.len() > MAX_INDEX_GENERATIONS {
            return Err(Error::IndexGenerationLimit {
                limit: MAX_INDEX_GENERATIONS,
            });
        }
        names.push(CString::new(bytes).expect("readdir names contain no interior NUL"));
    }
    Ok(names)
}

/// Refuse publication before it can grow the immutable generation directory
/// beyond a fixed count or byte budget. Existing content hashes remain usable
/// at the limit, so an idempotent refresh cannot be turned into a failure. We
/// deliberately do not delete old generations here: cross-process readers may
/// still hold them as the authority for an in-flight resolution.
pub(super) fn enforce_index_generation_budget(
    state: &repository::Cached,
    indexes_directory: &fs::File,
    owner: DirectoryOwner,
    target_name: &CStr,
    identity: &IndexIdentity,
) -> Result<(), Error> {
    let target_path = immutable_index_path(state, &identity.sha256);
    if witness_at(indexes_directory, target_name, &target_path)?.is_some() {
        return Ok(());
    }

    let indexes_path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
    let names = immutable_index_entry_names(indexes_directory, &indexes_path)?;
    if names.len() >= MAX_INDEX_GENERATIONS {
        return Err(Error::IndexGenerationLimit {
            limit: MAX_INDEX_GENERATIONS,
        });
    }

    let mut bytes = 0_u64;
    for name in names {
        let raw_name = name.to_bytes();
        let Some(hash) = raw_name.strip_suffix(b".stone") else {
            return Err(Error::InvalidIndexDirectoryEntry(
                indexes_path.join(OsStr::from_bytes(raw_name)),
            ));
        };
        if hash.len() != 64
            || !hash
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(Error::InvalidIndexDirectoryEntry(
                indexes_path.join(OsStr::from_bytes(raw_name)),
            ));
        }
        let path = indexes_path.join(OsStr::from_bytes(raw_name));
        let file = openat2_file(
            indexes_directory.as_raw_fd(),
            raw_name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &path,
        )
        .map_err(|source| Error::OpenIndex {
            path: path.clone(),
            source,
        })?;
        let witness = inspect_file(&file, &path)?;
        require_regular_owned(&path, witness, owner, Some(0o444))?;
        bytes = bytes
            .checked_add(witness.length)
            .ok_or(Error::IndexGenerationByteLimit {
                limit: MAX_INDEX_GENERATION_BYTES,
            })?;
    }
    if !matches!(
        bytes.checked_add(identity.byte_size),
        Some(total) if total <= MAX_INDEX_GENERATION_BYTES
    ) {
        return Err(Error::IndexGenerationByteLimit {
            limit: MAX_INDEX_GENERATION_BYTES,
        });
    }
    Ok(())
}

pub(super) struct PublishedIndex {
    pub(super) file: Arc<fs::File>,
    pub(super) witness: FileWitness,
}

/// Seal and atomically rename the exact retained candidate inode. Hard links
/// are deliberately forbidden: the active object has nlink=1 before, during,
/// and after the DB commit. An EEXIST race converges only after byte-for-byte
/// and metadata verification of the independently retained final descriptor.
pub(super) fn publish_index_candidate(
    state: &repository::Cached,
    mutation: &RepositoryMutationLock,
    candidate: &FetchedIndex,
    bytes: &[u8],
    identity: &IndexIdentity,
) -> Result<PublishedIndex, Error> {
    verify_mutation_boundary(state, mutation)?;
    let cache_owner = directory_owner(&mutation.cache_directory, &state.cache_dir)?;
    require_directory_owned(
        &candidate.directory,
        candidate
            .path
            .parent()
            .ok_or_else(|| Error::InvalidIndexPath(candidate.path.clone()))?,
        cache_owner,
        Some(0o700),
    )?;
    let before = inspect_file(&candidate.file, &candidate.path)?;
    require_regular_owned(&candidate.path, before, cache_owner, None)?;
    let (confirmed, read_witness) = read_index_bytes(&candidate.file, &candidate.path)?;
    if confirmed != bytes || read_witness != before {
        return Err(Error::IndexChanged(candidate.path.clone()));
    }

    candidate
        .file
        .set_permissions(std::fs::Permissions::from_mode(0o444))
        .map_err(|source| Error::PrepareIndexCandidate {
            path: candidate.path.clone(),
            source,
        })?;
    candidate.file.sync_all().map_err(|source| Error::SyncIndexFile {
        path: candidate.path.clone(),
        source,
    })?;
    let sealed = inspect_file(&candidate.file, &candidate.path)?;
    require_regular_owned(&candidate.path, sealed, cache_owner, Some(0o444))?;
    let (sealed_bytes, sealed_after_read) = read_index_bytes(&candidate.file, &candidate.path)?;
    if sealed_bytes != bytes || sealed_after_read != sealed {
        return Err(Error::IndexChanged(candidate.path.clone()));
    }
    require_name_witness(
        &candidate.directory,
        CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string"),
        sealed,
        &candidate.path,
    )?;

    let indexes_directory = open_indexes_directory(state, &mutation.cache_directory, true)?;
    let target_path = immutable_index_path(state, &identity.sha256);
    let target_name = immutable_index_name(&identity.sha256)?;
    enforce_index_generation_budget(state, &indexes_directory, cache_owner, &target_name, identity)?;
    let source_name = CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string");
    // SAFETY: both retained directory descriptors and names remain live.
    // RENAME_NOREPLACE either moves the authenticated inode or changes nothing.
    let renamed = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            candidate.directory.as_raw_fd(),
            source_name.as_ptr(),
            indexes_directory.as_raw_fd(),
            target_name.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    let created = if renamed == 0 {
        true
    } else {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::AlreadyExists {
            false
        } else {
            return Err(Error::PublishIndex {
                source_path: candidate.path.clone(),
                target: target_path,
                source,
            });
        }
    };

    if !created {
        require_name_witness(&candidate.directory, source_name, sealed, &candidate.path)?;
    }
    let final_file = openat2_file(
        indexes_directory.as_raw_fd(),
        target_name.to_bytes(),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &target_path,
    )
    .map_err(|source| Error::OpenIndex {
        path: target_path.clone(),
        source,
    })?;
    let final_before = inspect_file(&final_file, &target_path)?;
    require_regular_owned(&target_path, final_before, cache_owner, Some(0o444))?;
    if created && (final_before.device != sealed.device || final_before.inode != sealed.inode) {
        return Err(Error::IndexPathChanged(target_path));
    }
    let (final_bytes, final_after) = read_index_bytes(&final_file, &target_path)?;
    require_regular_owned(&target_path, final_after, cache_owner, Some(0o444))?;
    if final_bytes != bytes || final_after != final_before {
        return Err(Error::IndexChanged(target_path));
    }
    verify_identity(&target_path, &final_bytes, identity)?;
    final_file.sync_all().map_err(|source| Error::SyncIndexFile {
        path: target_path.clone(),
        source,
    })?;
    sync_directory_file(
        &candidate.directory,
        candidate
            .path
            .parent()
            .ok_or_else(|| Error::InvalidIndexPath(candidate.path.clone()))?,
    )?;
    sync_directory_file(&indexes_directory, &state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY))?;
    sync_directory_file(&mutation.cache_directory, &state.cache_dir)?;
    require_name_witness(&indexes_directory, &target_name, final_after, &target_path)?;

    Ok(PublishedIndex {
        file: Arc::new(final_file),
        witness: final_after,
    })
}

pub(super) fn require_name_witness(
    directory: &fs::File,
    name: &CStr,
    expected: FileWitness,
    path: &Path,
) -> Result<(), Error> {
    match witness_at(directory, name, path)? {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(Error::IndexPathChanged(path.to_owned())),
    }
}

fn witness_at(directory: &fs::File, name: &CStr, path: &Path) -> Result<Option<FileWitness>, Error> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: directory/name are live and stat points to writable storage.
    if unsafe {
        nix::libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(Error::InspectIndex {
                path: path.to_owned(),
                source,
            })
        };
    }
    // SAFETY: successful fstatat initialized stat.
    let stat = unsafe { stat.assume_init() };
    Ok(Some(FileWitness::from_stat(&stat)))
}

pub(super) fn descendant_resolution() -> u64 {
    nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV
}

pub(super) fn openat2_file(
    dirfd: RawFd,
    path: &[u8],
    flags: i32,
    mode: u32,
    resolve: u64,
    display_path: &Path,
) -> io::Result<fs::File> {
    let path = CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: zero is valid for every open_how field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: arguments remain live and a successful call returns a fresh fd.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let raw = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    Ok(fs::File::from_parts(descriptor.into(), display_path))
}

pub(super) fn proc_fd_path(file: &fs::File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
