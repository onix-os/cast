//! Private, alias-free materialization for inactive archived-state repair.

use std::{
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use thiserror::Error as ThisError;

use super::{
    AssetMaterialization, BlitExecution, Client, Error, PendingFile, Scope,
    blit_root_into_missing_target_with_materialization, openat2_frozen,
};
use crate::{Installation, linux_fs, package};

// This is a process-local lease for cooperating Forge entry points. It does
// not fence an arbitrary same-UID process from mutating the namespace.
static ARCHIVED_REPAIR_COORDINATOR: Mutex<()> = Mutex::new(());

const ROOTS_RELATIVE: &str = ".cast/root";
const STAGING_RELATIVE: &str = "staging";
const PRIVATE_WRAPPER_MODE: u32 = 0o700;

/// An independently copied archived-repair candidate plus the process-local
/// namespace lease that was acquired before its first staging mutation.
///
/// This token is deliberately non-cloneable. Consuming it in archived repair
/// keeps ordinary stateful blits and other archived repairs away from the
/// fixed staging namespace through metadata, triggers, and publication.
pub(super) struct ArchivedRepairCandidate {
    pub(super) tree: vfs::Tree<PendingFile>,
    pub(super) _coordinator: MutexGuard<'static, ()>,
}

#[derive(Debug, ThisError)]
pub(super) enum MaterializationError {
    #[error("archived repair requires a stateful client")]
    StatefulClientRequired,
    #[error("revalidate the installation root before inactive archived-state materialization")]
    RevalidateInstallation(#[source] crate::installation::Error),
    #[error("open the authenticated roots directory for inactive archived-state materialization")]
    OpenRoots(#[source] io::Error),
    #[error("inspect the fixed archived-repair staging wrapper at {path:?}")]
    InspectStaging {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "refuse archived-state materialization because fixed staging is not an exact owner-private 0700 directory: {path:?} (uid={owner}, mode={mode:04o})"
    )]
    UnsafeStaging { path: PathBuf, owner: u32, mode: u32 },
    #[error("refuse archived-state materialization because fixed staging has a POSIX ACL: {path:?}")]
    StagingAcl {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "refuse archived-state materialization because fixed staging is nonempty or changed; its contents were not traversed or removed: {path:?}"
    )]
    StagingNotEmpty {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "refuse archived-state materialization because the fixed staging name changed during baseline proof: {path:?}"
    )]
    StagingChanged { path: PathBuf },
    #[error("materialize an independently copied inactive archived-state candidate at {path:?}")]
    Blit {
        path: PathBuf,
        #[source]
        source: Box<Error>,
    },
}

impl From<MaterializationError> for Error {
    fn from(source: MaterializationError) -> Self {
        Self::ArchivedRepairMaterialization {
            source: Box::new(source),
        }
    }
}

impl Client {
    /// Build a candidate for one inactive archived state without ever linking
    /// a writable package inode to the persistent asset pool.
    pub(super) fn materialize_archived_repair_root<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<ArchivedRepairCandidate, Error> {
        self.require_non_frozen()?;
        if !matches!(&self.scope, Scope::Stateful) {
            return Err(MaterializationError::StatefulClientRequired.into());
        }

        let coordinator = lock_coordinator()?;
        let tree = self.vfs(packages)?;
        prepare_fixed_staging_baseline(&self.installation)?;

        let staging = self.installation.staging_dir();
        blit_root_into_missing_target_with_materialization(
            &self.installation,
            &tree,
            &staging,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Sequential,
            PRIVATE_WRAPPER_MODE,
        )
        .map_err(|source| MaterializationError::Blit {
            path: staging,
            source: Box::new(source),
        })?;

        Ok(ArchivedRepairCandidate {
            tree,
            _coordinator: coordinator,
        })
    }
}

pub(super) fn lock_coordinator() -> Result<MutexGuard<'static, ()>, Error> {
    before_coordinator_lock();
    ARCHIVED_REPAIR_COORDINATOR
        .lock()
        .map_err(|_| Error::FixedStagingCoordinatorPoisoned)
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_COORDINATOR_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STAGING_BASELINE_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static FIXED_STAGING_REMOVAL: std::cell::RefCell<Option<Box<dyn FnOnce() -> nix::Result<()>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_coordinator_lock(hook: impl FnOnce() + 'static) {
    BEFORE_COORDINATOR_LOCK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_before_staging_baseline_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_STAGING_BASELINE_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_fixed_staging_removal(hook: impl FnOnce() -> nix::Result<()> + 'static) {
    FIXED_STAGING_REMOVAL.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_coordinator_lock() {
    BEFORE_COORDINATOR_LOCK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_coordinator_lock() {}

#[cfg(test)]
fn before_staging_baseline_revalidation() {
    BEFORE_STAGING_BASELINE_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_staging_baseline_revalidation() {}

#[cfg(test)]
fn remove_fixed_staging(roots: i32) -> nix::Result<()> {
    if let Some(hook) = FIXED_STAGING_REMOVAL.with(|slot| slot.borrow_mut().take()) {
        return hook();
    }
    nix::unistd::unlinkat(
        Some(roots),
        Path::new(STAGING_RELATIVE),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )
}

#[cfg(not(test))]
fn remove_fixed_staging(roots: i32) -> nix::Result<()> {
    nix::unistd::unlinkat(
        Some(roots),
        Path::new(STAGING_RELATIVE),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )
}

/// Admit only a missing fixed staging name or one exact empty 0700 directory.
/// The latter is removed with one `unlinkat(AT_REMOVEDIR)`, never recursively.
/// A nonempty wrapper therefore survives byte-for-byte for crash recovery.
fn prepare_fixed_staging_baseline(installation: &Installation) -> Result<(), Error> {
    installation
        .revalidate_root_directory()
        .map_err(MaterializationError::RevalidateInstallation)?;

    let resolution = (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64;
    let roots = openat2_frozen(
        installation.root_directory().as_raw_fd(),
        Path::new(ROOTS_RELATIVE),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        resolution,
    )
    .map_err(MaterializationError::OpenRoots)?;

    let staging_path = installation.staging_dir();
    let staging = match openat2_frozen(
        roots.as_raw_fd(),
        Path::new(STAGING_RELATIVE),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        resolution,
    ) {
        Ok(staging) => staging,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            installation
                .revalidate_root_directory()
                .map_err(MaterializationError::RevalidateInstallation)?;
            return Ok(());
        }
        Err(source) => {
            return Err(MaterializationError::InspectStaging {
                path: staging_path,
                source,
            }
            .into());
        }
    };

    let metadata = staging
        .metadata()
        .map_err(|source| MaterializationError::InspectStaging {
            path: staging_path.clone(),
            source,
        })?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode != PRIVATE_WRAPPER_MODE {
        return Err(MaterializationError::UnsafeStaging {
            path: staging_path,
            owner: metadata.uid(),
            mode,
        }
        .into());
    }
    linux_fs::require_no_access_acl(staging.file(), &staging_path).map_err(|source| {
        MaterializationError::StagingAcl {
            path: staging_path.clone(),
            source,
        }
    })?;
    linux_fs::require_no_default_acl(staging.file(), &staging_path).map_err(|source| {
        MaterializationError::StagingAcl {
            path: staging_path.clone(),
            source,
        }
    })?;

    let retained_witness = staging_witness(&staging, &staging_path)?;
    before_staging_baseline_revalidation();
    let named_staging = openat2_frozen(
        roots.as_raw_fd(),
        Path::new(STAGING_RELATIVE),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        resolution,
    )
    .map_err(|source| MaterializationError::InspectStaging {
        path: staging_path.clone(),
        source,
    })?;
    if staging_witness(&named_staging, &staging_path)? != retained_witness {
        return Err(MaterializationError::StagingChanged { path: staging_path }.into());
    }
    linux_fs::require_no_access_acl(named_staging.file(), &staging_path).map_err(|source| {
        MaterializationError::StagingAcl {
            path: staging_path.clone(),
            source,
        }
    })?;
    linux_fs::require_no_default_acl(named_staging.file(), &staging_path).map_err(|source| {
        MaterializationError::StagingAcl {
            path: staging_path.clone(),
            source,
        }
    })?;

    // `unlinkat` may have removed the exact wrapper even when userspace sees
    // EINTR. Never repeat this destructive call: a replacement could occupy
    // the fixed name between attempts. Reconcile the name once instead.
    let removal = remove_fixed_staging(roots.as_raw_fd());

    match openat2_frozen(
        roots.as_raw_fd(),
        Path::new(STAGING_RELATIVE),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        resolution,
    ) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let metadata = staging
                .metadata()
                .map_err(|source| MaterializationError::InspectStaging {
                    path: staging_path.clone(),
                    source,
                })?;
            if metadata.nlink() != 0 {
                // The fixed name disappeared, but the exact retained wrapper
                // is still linked somewhere. Treat a move-away as a namespace
                // change rather than adopting it as completed removal.
                return Err(MaterializationError::StagingChanged { path: staging_path }.into());
            }
        }
        Ok(_) => {
            return Err(match removal {
                Ok(()) => MaterializationError::StagingChanged { path: staging_path },
                Err(source) => MaterializationError::StagingNotEmpty {
                    path: staging_path,
                    source: io::Error::from_raw_os_error(source as i32),
                },
            }
            .into());
        }
        Err(source) => {
            return Err(MaterializationError::InspectStaging {
                path: staging_path,
                source,
            }
            .into());
        }
    }

    roots
        .sync_all()
        .map_err(|source| MaterializationError::InspectStaging {
            path: installation.root_path(""),
            source,
        })?;
    installation
        .revalidate_root_directory()
        .map_err(MaterializationError::RevalidateInstallation)?;
    Ok(())
}

fn staging_witness(file: &fs_err::File, path: &Path) -> Result<(u64, u64, u32, u32), Error> {
    let metadata = file.metadata().map_err(|source| MaterializationError::InspectStaging {
        path: path.to_owned(),
        source,
    })?;
    Ok((
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.permissions().mode() & 0o7777,
    ))
}
