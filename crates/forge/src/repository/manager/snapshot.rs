use std::{ffi::CStr, io, os::fd::AsRawFd as _, sync::Arc};

use fs_err as fs;

use crate::{db::meta, repository};

use super::{
    REPOSITORY_MUTATION_LOCK_NAME,
    error::Error,
    index_storage::{
        DirectoryIdentity, FileWitness, IndexIdentity, descendant_resolution, directory_identity, directory_owner,
        immutable_index_name, immutable_index_path, inspect_file, open_cache_directory, open_indexes_directory,
        openat2_file, read_index_bytes, require_name_witness, require_regular_owned, sync_directory_file,
        verify_identity,
    },
};
/// Cached proof for one DB-selected generation. It never selects a generation:
/// callers must first read the complete [`meta::Snapshot`] from SQLite and the
/// key must match it. Retaining the descriptor also keeps an inode alive if GC
/// unlinks an older generation while a bounded operation is finishing.
#[derive(Debug)]
pub(crate) struct VerifiedSnapshot {
    pub(super) snapshot: meta::Snapshot,
    pub(super) file: Arc<fs::File>,
    pub(super) witness: FileWitness,
}

pub(super) struct RepositoryMutationLock {
    pub(super) cache_directory: fs::File,
    pub(super) cache_identity: DirectoryIdentity,
    pub(super) lock_file: fs::File,
    pub(super) lock_witness: FileWitness,
}

pub(crate) struct StableSnapshotView {
    pub(super) snapshots: Vec<repository::IndexSnapshot>,
    // Drop only after every query using this view and its snapshot copy has
    // completed. Each file owns one process-wide advisory shared lock.
    pub(super) _locks: Vec<RepositorySnapshotReadLock>,
}

impl StableSnapshotView {
    pub(crate) fn snapshots(&self) -> &[repository::IndexSnapshot] {
        &self.snapshots
    }
}

pub(super) struct RepositorySnapshotReadLock {
    _cache_directory: fs::File,
    _lock_file: fs::File,
}

impl RepositorySnapshotReadLock {
    pub(super) fn acquire(state: &repository::Cached) -> Result<Self, Error> {
        let cache_directory = open_cache_directory(state)?;
        let owner = directory_owner(&cache_directory, &state.cache_dir)?;
        let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
        let lock_file = openat2_file(
            cache_directory.as_raw_fd(),
            REPOSITORY_MUTATION_LOCK_NAME.as_bytes(),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
            &lock_path,
        )
        .map_err(|source| Error::OpenRepositoryMutationLock {
            path: lock_path.clone(),
            source,
        })?;
        let witness = inspect_file(&lock_file, &lock_path)?;
        require_regular_owned(&lock_path, witness, owner, Some(0o600))?;

        loop {
            // SAFETY: `lock_file` is live. LOCK_SH blocks behind the writer's
            // LOCK_EX and is held by this descriptor until the view is dropped.
            if unsafe { nix::libc::flock(lock_file.as_raw_fd(), nix::libc::LOCK_SH) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::LockRepositorySnapshot {
                    path: lock_path,
                    source,
                });
            }
        }

        require_name_witness(
            &cache_directory,
            CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
            witness,
            &lock_path,
        )?;
        if inspect_file(&lock_file, &lock_path)? != witness {
            return Err(Error::IndexPathChanged(lock_path));
        }
        Ok(Self {
            _cache_directory: cache_directory,
            _lock_file: lock_file,
        })
    }
}

impl RepositoryMutationLock {
    pub(super) fn acquire(state: &repository::Cached) -> Result<Self, Error> {
        let cache_directory = open_cache_directory(state)?;
        let owner = directory_owner(&cache_directory, &state.cache_dir)?;
        let cache_identity = directory_identity(&cache_directory, &state.cache_dir)?;
        let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
        let lock_file = openat2_file(
            cache_directory.as_raw_fd(),
            REPOSITORY_MUTATION_LOCK_NAME.as_bytes(),
            nix::libc::O_RDWR | nix::libc::O_CREAT | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
            descendant_resolution(),
            &lock_path,
        )
        .map_err(|source| Error::OpenRepositoryMutationLock {
            path: lock_path.clone(),
            source,
        })?;
        let witness = inspect_file(&lock_file, &lock_path)?;
        require_regular_owned(&lock_path, witness, owner, Some(0o600))?;
        lock_file.sync_all().map_err(|source| Error::SyncIndexFile {
            path: lock_path.clone(),
            source,
        })?;
        sync_directory_file(&cache_directory, &state.cache_dir)?;

        loop {
            // SAFETY: `lock_file` is a live descriptor. LOCK_EX coordinates
            // independent opens in this and other processes.
            if unsafe { nix::libc::flock(lock_file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::LockRepositoryMutation {
                    path: lock_path,
                    source,
                });
            }
        }

        require_name_witness(
            &cache_directory,
            CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
            witness,
            &lock_path,
        )?;
        Ok(Self {
            cache_directory,
            cache_identity,
            lock_file,
            lock_witness: witness,
        })
    }
}

pub(super) fn verify_mutation_boundary(
    state: &repository::Cached,
    mutation: &RepositoryMutationLock,
) -> Result<(), Error> {
    let reopened_cache = open_cache_directory(state)?;
    if directory_identity(&reopened_cache, &state.cache_dir)? != mutation.cache_identity {
        return Err(Error::IndexPathChanged(state.cache_dir.clone()));
    }
    let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
    require_name_witness(
        &mutation.cache_directory,
        CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
        mutation.lock_witness,
        &lock_path,
    )?;
    if inspect_file(&mutation.lock_file, &lock_path)? != mutation.lock_witness {
        return Err(Error::IndexPathChanged(lock_path));
    }
    Ok(())
}

pub(crate) fn verify_active_snapshot(
    state: &repository::Cached,
    snapshot: Option<meta::Snapshot>,
) -> Result<meta::Snapshot, Error> {
    let snapshot = snapshot.ok_or_else(|| Error::MissingActiveSnapshot(state.id.clone()))?;
    let target_path = immutable_index_path(state, snapshot.sha256());
    let target_name = immutable_index_name(snapshot.sha256())?;
    let expected = IndexIdentity {
        sha256: snapshot.sha256().to_owned(),
        byte_size: snapshot.byte_size(),
    };
    let mut cache = lock_verified_snapshot_cache(state);
    if let Some(verified) = cache.as_ref()
        && verified.snapshot == snapshot
    {
        let held = inspect_file(&verified.file, &target_path)?;
        if held != verified.witness {
            return Err(Error::IndexChanged(target_path));
        }
        let cache_directory = open_cache_directory(state)?;
        let indexes_directory = open_indexes_directory(state, &cache_directory, false)?;
        let current = openat2_file(
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
        let current_witness = inspect_file(&current, &target_path)?;
        if current_witness != verified.witness {
            return Err(Error::IndexPathChanged(target_path));
        }
        return Ok(snapshot);
    }

    let cache_directory = open_cache_directory(state)?;
    let owner = directory_owner(&cache_directory, &state.cache_dir)?;
    let indexes_directory = open_indexes_directory(state, &cache_directory, false)?;
    let file = openat2_file(
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
    let before = inspect_file(&file, &target_path)?;
    require_regular_owned(&target_path, before, owner, Some(0o444))?;
    let (bytes, after) = read_index_bytes(&file, &target_path)?;
    require_regular_owned(&target_path, after, owner, Some(0o444))?;
    if before != after {
        return Err(Error::IndexChanged(target_path));
    }
    verify_identity(&target_path, &bytes, &expected)?;
    require_name_witness(&indexes_directory, &target_name, after, &target_path)?;
    let file = Arc::new(file);
    *cache = Some(VerifiedSnapshot {
        snapshot: snapshot.clone(),
        file,
        witness: after,
    });
    Ok(snapshot)
}

pub(crate) fn verified_active_snapshot(state: &repository::Cached) -> Result<meta::Snapshot, Error> {
    verify_active_snapshot(state, state.db.active_snapshot()?)
}

pub(super) fn repository_is_initialized(state: &repository::Cached) -> Result<bool, Error> {
    let Some(snapshot) = state.db.active_snapshot()? else {
        return Ok(false);
    };
    Ok(verify_active_snapshot(state, Some(snapshot)).is_ok())
}

pub(super) fn lock_verified_snapshot_cache(
    state: &repository::Cached,
) -> std::sync::MutexGuard<'_, Option<VerifiedSnapshot>> {
    match state.verified_snapshot.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            let mut cache = poisoned.into_inner();
            *cache = None;
            state.verified_snapshot.clear_poison();
            cache
        }
    }
}
