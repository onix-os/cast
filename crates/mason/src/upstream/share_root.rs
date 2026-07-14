
//! Descriptor-pinned build-visible source root.

use std::{
    ffi::CString,
    fs::{File, Permissions},
    io,
    mem::size_of,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

use fs_err as fs;
use thiserror::Error;

/// A source root reached without following any symlink component and pinned
/// by an open directory descriptor for the whole synchronization operation.
pub(super) struct ShareRoot {
    requested_path: PathBuf,
    directory: File,
    descriptor_path: PathBuf,
    materialized_root: Option<File>,
    materialized_relative: Option<PathBuf>,
}

impl ShareRoot {
    #[cfg(test)]
    pub(super) fn prepare(path: &Path) -> Result<Self, Error> {
        let requested_path = std::path::absolute(path).map_err(|source| Error::Absolute {
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
            materialized_root: None,
            materialized_relative: None,
        })
    }

    /// Create and pin an absolute build-visible source directory beneath the
    /// exact inode returned by Forge materialization.  Neither creation nor
    /// later name verification reopens the global frozen-root pathname.
    pub(super) fn prepare_in(root: &forge::MaterializedFrozenRoot, target: &Path) -> Result<Self, Error> {
        let relative = materialized_relative(target)?;
        let anchor = root.revalidated_anchor().map_err(Error::MaterializedRoot)?;
        let root_directory = File::from(anchor.try_clone_to_owned().map_err(|source| Error::Open {
            path: root.root_path().to_owned(),
            source,
        })?);
        let requested_path = root.root_path().join(&relative);
        let directory = open_materialized_directory_chain(&root_directory, &relative, &requested_path, true)?;
        let descriptor_path = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), directory.as_raw_fd()));
        require_empty(&descriptor_path, &requested_path)?;

        // Catch a destination-name substitution in the final component before
        // any source copier is allowed to receive the descriptor path.
        require_materialized_name(&root_directory, &relative, &directory, &requested_path)?;
        root.revalidate().map_err(Error::MaterializedRoot)?;

        Ok(Self {
            requested_path,
            directory,
            descriptor_path,
            materialized_root: Some(root_directory),
            materialized_relative: Some(relative),
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

        let current = match (&self.materialized_root, &self.materialized_relative) {
            (Some(root), Some(relative)) => {
                open_materialized_directory_chain(root, relative, &self.requested_path, false)?
            }
            (None, None) => open_directory_chain(&self.requested_path, false)?,
            _ => return Err(Error::InvalidRetainedRoot),
        };
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

const MAX_MATERIALIZED_SOURCE_PATH_BYTES: usize = 4_095;
const MAX_MATERIALIZED_SOURCE_COMPONENTS: usize = 32;

fn materialized_relative(target: &Path) -> Result<PathBuf, Error> {
    let bytes = target.as_os_str().as_bytes();
    if !target.is_absolute()
        || bytes.len() > MAX_MATERIALIZED_SOURCE_PATH_BYTES
        || bytes.contains(&0)
        || target.components().count() > MAX_MATERIALIZED_SOURCE_COMPONENTS + 1
        || target
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::InvalidMaterializedTarget(target.to_owned()));
    }
    let relative = target
        .strip_prefix("/")
        .map_err(|_| Error::InvalidMaterializedTarget(target.to_owned()))?;
    if relative.as_os_str().is_empty() {
        return Err(Error::InvalidMaterializedTarget(target.to_owned()));
    }
    Ok(relative.to_owned())
}

fn require_empty(descriptor_path: &Path, requested_path: &Path) -> Result<(), Error> {
    let mut entries = fs::read_dir(descriptor_path).map_err(|source| Error::Read {
        path: requested_path.to_owned(),
        source,
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|source| Error::Read {
            path: requested_path.to_owned(),
            source,
        })?
        .is_some()
    {
        return Err(Error::NotEmpty(requested_path.to_owned()));
    }
    Ok(())
}

fn open_materialized_directory_chain(
    root: &File,
    relative: &Path,
    display: &Path,
    create: bool,
) -> Result<File, Error> {
    let mut current = root.try_clone().map_err(|source| Error::Open {
        path: display.to_owned(),
        source,
    })?;
    let mut traversed = PathBuf::from("/");
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(Error::InvalidMaterializedTarget(display.to_owned()));
        };
        traversed.push(name);
        let name = CString::new(name.as_bytes()).map_err(|_| Error::NulComponent {
            path: traversed.clone(),
        })?;
        let mut next = openat2_directory(&current, &name);
        if create
            && next
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            // SAFETY: the retained parent and component remain live. mkdirat
            // cannot follow the final component; openat2 authenticates it
            // immediately afterwards with NO_SYMLINKS and NO_XDEV.
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
            next = openat2_directory(&current, &name);
        }
        current = next.map_err(|source| Error::Open {
            path: traversed.clone(),
            source,
        })?;
    }
    Ok(current)
}

fn require_materialized_name(root: &File, relative: &Path, pinned: &File, display: &Path) -> Result<(), Error> {
    let named = open_materialized_directory_chain(root, relative, display, false)?;
    let expected = pinned.metadata().map_err(|source| Error::Inspect {
        path: display.to_owned(),
        source,
    })?;
    let found = named.metadata().map_err(|source| Error::Inspect {
        path: display.to_owned(),
        source,
    })?;
    use std::os::unix::fs::MetadataExt as _;
    if expected.dev() != found.dev() || expected.ino() != found.ino() {
        return Err(Error::Replaced(display.to_owned()));
    }
    Ok(())
}

fn openat2_directory(parent: &File, name: &CString) -> io::Result<File> {
    // SAFETY: an all-zero open_how is valid before setting its fields.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from(
        (nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NONBLOCK) as u32,
    );
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: all pointers remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(result).map_err(|_| io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
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
    #[error("materialized source target is not one bounded normalized absolute path: {0:?}")]
    InvalidMaterializedTarget(PathBuf),
    #[error("revalidate the exact Forge materialized root around source preparation")]
    MaterializedRoot(#[source] forge::client::Error),
    #[error("retained source root has inconsistent provenance state")]
    InvalidRetainedRoot,
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

    #[test]
    fn materialized_source_descriptor_never_populates_a_replacement_root() {
        let temporary = tempfile::tempdir().unwrap();
        let published = temporary.path().join("published");
        fs::create_dir(&published).unwrap();
        let root = File::open(&published).unwrap();
        let relative = Path::new("mason/sources");
        let sources = open_materialized_directory_chain(&root, relative, &published.join(relative), true).unwrap();
        let descriptor_path = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), sources.as_raw_fd()));

        let retained = temporary.path().join("retained");
        fs::rename(&published, &retained).unwrap();
        fs::create_dir(&published).unwrap();
        fs::write(descriptor_path.join("source-marker"), b"exact staged root").unwrap();

        assert_eq!(
            fs::read(retained.join(relative).join("source-marker")).unwrap(),
            b"exact staged root"
        );
        assert!(!published.join(relative).exists());
    }
}
