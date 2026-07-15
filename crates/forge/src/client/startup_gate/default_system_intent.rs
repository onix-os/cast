//! Retained authority for the canonical `etc/cast/system.glu` intent.

use std::{
    fs::Metadata,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{Installation, system_model};

const INTENT_DIRECTORY: &std::ffi::CStr = c"etc/cast";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl DirectoryWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.permissions().mode() & 0o7777,
            links: metadata.nlink(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

pub(super) fn load(installation: &Installation) -> Result<Option<system_model::LoadedSystemModel>, Error> {
    let directory_path = installation.root.join("etc/cast");
    installation
        .revalidate_root_directory()
        .map_err(|source| Error::RootAuthority {
            path: installation.root.clone(),
            source,
        })?;

    let Some(directory) = open_directory(installation, &directory_path)? else {
        installation
            .revalidate_root_directory()
            .map_err(|source| Error::RootAuthority {
                path: installation.root.clone(),
                source,
            })?;
        if open_directory(installation, &directory_path)?.is_some() {
            return Err(changed(
                &directory_path,
                "intent directory appeared during absence proof",
            ));
        }
        installation
            .revalidate_root_directory()
            .map_err(|source| Error::RootAuthority {
                path: installation.root.clone(),
                source,
            })?;
        return Ok(None);
    };
    let expected = directory_witness(&directory, &directory_path)?;
    require_named(installation, &directory, &directory_path, expected)?;
    installation
        .revalidate_root_directory()
        .map_err(|source| Error::RootAuthority {
            path: installation.root.clone(),
            source,
        })?;

    after_default_directory_retained();

    installation
        .revalidate_root_directory()
        .map_err(|source| Error::RootAuthority {
            path: installation.root.clone(),
            source,
        })?;
    require_named(installation, &directory, &directory_path, expected)?;
    let loaded = system_model::load_rooted(&directory_path, &directory)?;
    require_named(installation, &directory, &directory_path, expected)?;
    installation
        .revalidate_root_directory()
        .map_err(|source| Error::RootAuthority {
            path: installation.root.clone(),
            source,
        })?;
    Ok(loaded)
}

fn open_directory(installation: &Installation, path: &Path) -> Result<Option<std::fs::File>, Error> {
    let flags = nix::libc::O_RDONLY
        | nix::libc::O_DIRECTORY
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK;
    match crate::linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        INTENT_DIRECTORY,
        flags,
        0,
        crate::linux_fs::controlled_resolution(),
    ) {
        Ok(directory) => Ok(Some(directory)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(Error::RetainDirectory {
            path: path.to_owned(),
            source,
        }),
    }
}

fn directory_witness(directory: &std::fs::File, path: &Path) -> Result<DirectoryWitness, Error> {
    let metadata = directory.metadata().map_err(|source| Error::RetainDirectory {
        path: path.to_owned(),
        source,
    })?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir()
        || (metadata.uid() != effective_owner && metadata.uid() != 0)
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
        || mode & 0o500 != 0o500
    {
        return Err(Error::RetainDirectory {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "intent directory is not an effective-user- or root-owned, non-writable readable directory (uid={}, mode={mode:04o})",
                    metadata.uid()
                ),
            ),
        });
    }
    crate::linux_fs::require_no_access_acl(directory, path).map_err(|source| Error::RetainDirectory {
        path: path.to_owned(),
        source,
    })?;
    crate::linux_fs::require_no_default_acl(directory, path).map_err(|source| Error::RetainDirectory {
        path: path.to_owned(),
        source,
    })?;
    Ok(DirectoryWitness::from_metadata(&metadata))
}

fn require_named(
    installation: &Installation,
    retained: &std::fs::File,
    path: &Path,
    expected: DirectoryWitness,
) -> Result<(), Error> {
    if directory_witness(retained, path)? != expected {
        return Err(changed(path, "retained intent directory metadata changed"));
    }
    let named =
        open_directory(installation, path)?.ok_or_else(|| changed(path, "named intent directory disappeared"))?;
    if directory_witness(&named, path)? != expected {
        return Err(changed(path, "named intent directory is not the retained inode"));
    }
    Ok(())
}

fn changed(path: &Path, detail: &'static str) -> Error {
    Error::AuthorityChanged {
        path: path.to_owned(),
        detail,
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("retain exact authored-intent directory {path}")]
    RetainDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("revalidate installation-root authority around authored intent {path}")]
    RootAuthority {
        path: PathBuf,
        #[source]
        source: crate::installation::Error,
    },
    #[error("authored-intent authority changed at {path}: {detail}")]
    AuthorityChanged { path: PathBuf, detail: &'static str },
    #[error("evaluate descriptor-rooted authored system intent")]
    Load(#[from] system_model::LoadError),
}

#[cfg(test)]
std::thread_local! {
    static AFTER_DEFAULT_DIRECTORY_RETAINED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_after_default_directory_retained(hook: impl FnOnce() + 'static) {
    AFTER_DEFAULT_DIRECTORY_RETAINED.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_default_directory_retained() {
    AFTER_DEFAULT_DIRECTORY_RETAINED.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_default_directory_retained() {}
