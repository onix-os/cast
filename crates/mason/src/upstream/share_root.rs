// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Descriptor-pinned build-visible source root.

use std::{
    ffi::CString,
    fs::{File, Permissions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{self, Component, Path, PathBuf},
};

use fs_err as fs;
use thiserror::Error;

/// A source root reached without following any symlink component and pinned
/// by an open directory descriptor for the whole synchronization operation.
pub(super) struct ShareRoot {
    requested_path: PathBuf,
    directory: File,
    descriptor_path: PathBuf,
}

impl ShareRoot {
    pub(super) fn prepare(path: &Path) -> Result<Self, Error> {
        let requested_path = path::absolute(path).map_err(|source| Error::Absolute {
            path: path.to_owned(),
            source,
        })?;
        let directory = open_directory_chain(&requested_path, true)?;
        let descriptor_path = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), directory.as_raw_fd()));

        let mut entries = fs::read_dir(&descriptor_path).map_err(|source| Error::Read {
            path: requested_path.clone(),
            source,
        })?;
        if entries
            .next()
            .transpose()
            .map_err(|source| Error::Read {
                path: requested_path.clone(),
                source,
            })?
            .is_some()
        {
            return Err(Error::NotEmpty(requested_path));
        }

        Ok(Self {
            requested_path,
            directory,
            descriptor_path,
        })
    }

    /// Path through the held descriptor. Operations beneath this path cannot
    /// be redirected by replacing the authored/host-visible ancestor chain.
    pub(super) fn descriptor_path(&self) -> &Path {
        &self.descriptor_path
    }

    pub(super) fn normalize_and_verify(&self, source_date_epoch: i64) -> Result<(), Error> {
        self.directory
            .set_permissions(Permissions::from_mode(0o755))
            .map_err(|source| Error::Normalize {
                path: self.requested_path.clone(),
                source,
            })?;
        let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
        filetime::set_file_handle_times(&self.directory, Some(timestamp), Some(timestamp)).map_err(|source| {
            Error::Normalize {
                path: self.requested_path.clone(),
                source,
            }
        })?;

        let current = open_directory_chain(&self.requested_path, false)?;
        let expected = self.directory.metadata().map_err(|source| Error::Inspect {
            path: self.requested_path.clone(),
            source,
        })?;
        let found = current.metadata().map_err(|source| Error::Inspect {
            path: self.requested_path.clone(),
            source,
        })?;
        use std::os::unix::fs::MetadataExt;
        if expected.dev() != found.dev() || expected.ino() != found.ino() {
            return Err(Error::Replaced(self.requested_path.clone()));
        }
        Ok(())
    }
}

fn open_directory_chain(path: &Path, create: bool) -> Result<File, Error> {
    let mut current = File::open("/").map_err(|source| Error::Open {
        path: PathBuf::from("/"),
        source,
    })?;
    let mut traversed = PathBuf::from("/");

    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(Error::InvalidComponent {
                path: path.to_owned(),
                component: component.as_os_str().to_owned(),
            });
        };
        traversed.push(name);
        let name = CString::new(name.as_bytes()).map_err(|_| Error::NulComponent {
            path: traversed.clone(),
        })?;

        let mut next = openat_directory(&current, &name);
        if create
            && next
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            let result = unsafe { nix::libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o755) };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() != io::ErrorKind::AlreadyExists {
                    return Err(Error::Create {
                        path: traversed.clone(),
                        source,
                    });
                }
            }
            next = openat_directory(&current, &name);
        }
        current = next.map_err(|source| Error::Open {
            path: traversed.clone(),
            source,
        })?;
    }

    Ok(current)
}

fn openat_directory(parent: &File, name: &CString) -> io::Result<File> {
    let descriptor = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("make source share root path absolute for {path:?}")]
    Absolute {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source share root {path:?} contains unsupported component {component:?}")]
    InvalidComponent {
        path: PathBuf,
        component: std::ffi::OsString,
    },
    #[error("source share root component contains NUL at {path:?}")]
    NulComponent { path: PathBuf },
    #[error("open source share root component {path:?} without following symlinks")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("create source share root component {path:?}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read source share root {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source share root must be empty before synchronization: {0:?}")]
    NotEmpty(PathBuf),
    #[error("normalize source share root {path:?}")]
    Normalize {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect source share root {path:?}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source share root path was replaced during synchronization: {0:?}")]
    Replaced(PathBuf),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, symlink};

    use super::*;

    #[test]
    fn rejects_symlink_components_without_touching_the_target() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = temporary.path().join("outside");
        let link = temporary.path().join("link");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &link).unwrap();

        assert!(matches!(ShareRoot::prepare(&link), Err(Error::Open { .. })));
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn held_descriptor_is_stable_and_visible_path_replacement_is_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("sources");
        let share = ShareRoot::prepare(&root).unwrap();
        let original = share.directory.metadata().unwrap();
        fs::rename(&root, temporary.path().join("moved")).unwrap();
        fs::create_dir(&root).unwrap();

        fs::write(share.descriptor_path().join("held"), b"held").unwrap();
        assert_eq!(original.ino(), fs::metadata(share.descriptor_path()).unwrap().ino());
        assert!(matches!(
            share.normalize_and_verify(1_700_000_000),
            Err(Error::Replaced(_))
        ));
        assert!(!root.join("held").exists());
    }
}
