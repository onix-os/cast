//! Retained `/etc` and complete authority view for ephemeral triggers.
//!
//! The external root and `/usr` are already retained by their owning
//! capabilities. This module adds `/etc` without ever adopting an occupant of
//! its canonical name: a fresh directory is retained behind a kernel-random
//! private name and then published once with `RENAME_NOREPLACE`. Paths stored
//! here are diagnostics only; every operation is descriptor-relative.

use std::{
    ffi::{CStr, CString},
    fmt, io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use thiserror::Error as ThisError;

use super::RetainedExternalMaterializationTarget;
use crate::{
    Installation,
    client::{Error, candidate_metadata::RetainedEphemeralUsr, effective_user_id},
    linux_fs::{
        chmod_path_descriptor, controlled_resolution, openat2_file, renameat2_noreplace_once, require_no_access_acl,
        require_no_default_acl,
    },
};

const ETC_NAME: &CStr = c"etc";
const PRIVATE_MODE: u32 = 0o700;
const CANONICAL_MODE: u32 = 0o755;
const MAX_INTERRUPTS: usize = 1_024;
const MAX_PRIVATE_ATTEMPTS: usize = 256;
const PRIVATE_RANDOM_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
}

/// One exact ephemeral `/etc` retained beneath an authenticated external root.
#[derive(Debug)]
pub(super) struct RetainedEphemeralEtc {
    directory: std::fs::File,
    path: PathBuf,
    witness: DirectoryWitness,
}

/// Complete descriptor-rooted filesystem authority for ephemeral triggers.
///
/// This is a borrowed view so the owning target, `/usr`, and `/etc`
/// capabilities necessarily outlive discovery, container construction, and
/// execution. Copying the view copies references, never file descriptors or
/// authority.
#[derive(Clone, Copy)]
pub(in crate::client) struct RetainedEphemeralTriggerView<'candidate> {
    target: &'candidate RetainedExternalMaterializationTarget,
    usr: &'candidate RetainedEphemeralUsr,
    etc: &'candidate RetainedEphemeralEtc,
}

impl<'candidate> RetainedEphemeralTriggerView<'candidate> {
    pub(super) fn new(
        target: &'candidate RetainedExternalMaterializationTarget,
        usr: &'candidate RetainedEphemeralUsr,
        etc: &'candidate RetainedEphemeralEtc,
    ) -> Self {
        Self { target, usr, etc }
    }

    /// Revalidate in a strict root -> usr -> etc -> root sandwich.
    pub(in crate::client) fn revalidate(self, installation: &Installation) -> Result<(), Error> {
        let revalidate = || -> Result<(), Error> {
            self.target.require_named(installation)?;
            self.usr.revalidate_under(self.target.target.file())?;
            self.etc.revalidate_under(self.target.target.file())?;
            self.target.require_named(installation)
        };
        revalidate().map_err(authority_error)
    }

    pub(in crate::client) fn usr(self) -> (&'candidate std::fs::File, &'candidate Path) {
        (self.usr.file(), self.usr.diagnostic_path())
    }

    pub(in crate::client) fn etc(self) -> (&'candidate std::fs::File, &'candidate Path) {
        (&self.etc.directory, &self.etc.path)
    }

    pub(in crate::client) fn root_path(self) -> &'candidate Path {
        self.target.path()
    }
}

impl fmt::Debug for RetainedEphemeralTriggerView<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedEphemeralTriggerView")
            .field("root", &self.target.path())
            .field("usr", &self.usr.diagnostic_path())
            .field("etc", &self.etc.path)
            .finish_non_exhaustive()
    }
}

impl RetainedEphemeralEtc {
    pub(super) fn create(root: &std::fs::File, root_path: &Path) -> Result<Self, Error> {
        let path = root_path.join("etc");
        require_absent(root, ETC_NAME, &path)?;

        for _ in 0..MAX_PRIVATE_ATTEMPTS {
            let private_name = random_private_name()?;
            let private_path = root_path.join(private_name.to_string_lossy().as_ref());
            match mkdir_private(root, &private_name) {
                Ok(()) => {}
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(trigger_io("create private ephemeral /etc", &private_path, source)),
            }

            let pinned = open_path_directory(root, &private_name, &private_path)?;
            let initial = private_witness(&pinned, &private_path)?;
            chmod_path_descriptor(&pinned, CANONICAL_MODE)
                .map_err(|source| trigger_io("normalize private ephemeral /etc", &private_path, source))?;

            let private = open_directory(root, &private_name, private_path.clone())?;
            if (private.witness.device, private.witness.inode) != (initial.device, initial.inode) {
                return Err(trigger_changed(&private_path));
            }
            private.require_named(root, &private_name)?;
            private.require_empty()?;
            private
                .directory
                .sync_all()
                .map_err(|source| trigger_io("sync private ephemeral /etc", &private_path, source))?;
            private.require_named(root, &private_name)?;

            before_etc_publication();
            private.require_named(root, &private_name)?;
            let publication = renameat2_noreplace_once(root, &private_name, root, ETC_NAME);
            let canonical = open_directory(root, ETC_NAME, path.clone());
            match (publication, canonical) {
                (Ok(()), Ok(canonical)) | (Err(_), Ok(canonical)) if canonical.witness == private.witness => {
                    require_absent(root, &private_name, &private_path)?;
                    canonical.require_empty()?;
                    root.sync_all().map_err(|source| {
                        trigger_io("sync external root after ephemeral /etc publication", &path, source)
                    })?;
                    canonical.require_named(root, ETC_NAME)?;
                    return Ok(canonical);
                }
                (Err(source), _) => {
                    return Err(trigger_io(
                        "publish private ephemeral /etc without replacement",
                        &path,
                        source,
                    ));
                }
                (Ok(()), _) => return Err(trigger_changed(&path)),
            }
        }

        Err(Error::EphemeralTriggerAuthority {
            source: Box::new(RetainedEphemeralTriggerError::PrivateNamesExhausted {
                limit: MAX_PRIVATE_ATTEMPTS,
            }),
        })
    }

    fn revalidate_under(&self, root: &std::fs::File) -> Result<(), Error> {
        self.require_named(root, ETC_NAME)
    }

    fn require_named(&self, parent: &std::fs::File, name: &CStr) -> Result<(), Error> {
        if directory_witness(&self.directory, &self.path)? != self.witness {
            return Err(trigger_changed(&self.path));
        }
        require_acl_free(&self.directory, &self.path)?;
        let named = open_readable_directory(parent, name, &self.path)?;
        if directory_witness(&named, &self.path)? != self.witness
            || directory_witness(&self.directory, &self.path)? != self.witness
        {
            return Err(trigger_changed(&self.path));
        }
        require_acl_free(&named, &self.path)
    }

    fn require_empty(&self) -> Result<(), Error> {
        self.require_retained()?;
        require_empty_directory(&self.directory, &self.path)?;
        self.require_retained()
    }

    fn require_retained(&self) -> Result<(), Error> {
        if directory_witness(&self.directory, &self.path)? != self.witness {
            return Err(trigger_changed(&self.path));
        }
        require_acl_free(&self.directory, &self.path)
    }
}

fn open_directory(parent: &std::fs::File, name: &CStr, path: PathBuf) -> Result<RetainedEphemeralEtc, Error> {
    let directory = open_readable_directory(parent, name, &path)?;
    let witness = directory_witness(&directory, &path)?;
    require_acl_free(&directory, &path)?;
    Ok(RetainedEphemeralEtc {
        directory,
        path,
        witness,
    })
}

fn open_readable_directory(parent: &std::fs::File, name: &CStr, path: &Path) -> Result<std::fs::File, Error> {
    openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| trigger_io("open retained ephemeral /etc", path, source))
}

fn open_path_directory(parent: &std::fs::File, name: &CStr, path: &Path) -> Result<std::fs::File, Error> {
    openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| trigger_io("retain ephemeral trigger directory", path, source))
}

fn directory_witness(file: &std::fs::File, path: &Path) -> Result<DirectoryWitness, Error> {
    let metadata = file
        .metadata()
        .map_err(|source| trigger_io("inspect retained ephemeral trigger directory", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_user_id()
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(Error::EphemeralTriggerAuthority {
            source: Box::new(RetainedEphemeralTriggerError::UnsafeDirectory {
                path: path.to_owned(),
                owner: metadata.uid(),
                mode,
            }),
        });
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode,
    })
}

fn private_witness(file: &std::fs::File, path: &Path) -> Result<DirectoryWitness, Error> {
    let metadata = file
        .metadata()
        .map_err(|source| trigger_io("inspect private ephemeral /etc", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_user_id()
        || mode & 0o7000 != 0
        || mode & !PRIVATE_MODE != 0
    {
        return Err(Error::EphemeralTriggerAuthority {
            source: Box::new(RetainedEphemeralTriggerError::UnsafeDirectory {
                path: path.to_owned(),
                owner: metadata.uid(),
                mode,
            }),
        });
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode,
    })
}

fn require_acl_free(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    require_no_access_acl(file, path)
        .map_err(|source| trigger_io("reject access ACL on ephemeral trigger directory", path, source))?;
    require_no_default_acl(file, path)
        .map_err(|source| trigger_io("reject default ACL on ephemeral trigger directory", path, source))
}

fn require_absent(parent: &std::fs::File, name: &CStr, path: &Path) -> Result<(), Error> {
    match openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Err(source) => Err(trigger_io("prove ephemeral trigger name absence", path, source)),
        Ok(_) => Err(Error::EphemeralTriggerAuthority {
            source: Box::new(RetainedEphemeralTriggerError::DestinationExists { path: path.to_owned() }),
        }),
    }
}

fn require_empty_directory(directory: &std::fs::File, path: &Path) -> Result<(), Error> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(trigger_io(
            "duplicate private ephemeral /etc for inventory",
            path,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: the fresh descriptor is a directory and remains uniquely owned.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed without consuming the descriptor.
        unsafe { nix::libc::close(duplicate) };
        return Err(trigger_io("enumerate private ephemeral /etc", path, source));
    }

    let result = loop {
        // SAFETY: errno is thread-local and the stream remains live.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: the stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(trigger_io("enumerate private ephemeral /etc", path, source))
            };
        }
        // SAFETY: Linux dirent names are NUL terminated for this live entry.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        break Err(Error::EphemeralTriggerAuthority {
            source: Box::new(RetainedEphemeralTriggerError::PrivateDirectoryNotEmpty { path: path.to_owned() }),
        });
    };
    // SAFETY: the stream came from fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(trigger_io(
            "close private ephemeral /etc inventory",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

fn mkdir_private(parent: &std::fs::File, name: &CStr) -> io::Result<()> {
    let mut interruptions = 0usize;
    loop {
        // SAFETY: parent and the generated single-component name remain live;
        // mkdirat neither follows nor replaces the final component.
        if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), PRIVATE_MODE) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::Interrupted && interruptions < MAX_INTERRUPTS {
            interruptions += 1;
            continue;
        }
        return Err(source);
    }
}

fn random_private_name() -> Result<CString, Error> {
    let mut random = [0_u8; PRIVATE_RANDOM_BYTES];
    let mut filled = 0usize;
    let mut interruptions = 0usize;
    while filled < random.len() {
        // SAFETY: the remaining byte slice is writable for the supplied
        // length. GRND_NONBLOCK keeps this preparation boundary finite.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                nix::libc::GRND_NONBLOCK,
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interruptions < MAX_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(trigger_io(
                "generate private ephemeral /etc name",
                Path::new("<kernel-random-name>"),
                source,
            ));
        }
        let read = usize::try_from(result).map_err(|_| trigger_changed(Path::new("<kernel-random-name>")))?;
        if read == 0 || read > random.len() - filled {
            return Err(trigger_changed(Path::new("<kernel-random-name>")));
        }
        filled += read;
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b".forge-ephemeral-etc-";
    let mut encoded = Vec::with_capacity(prefix.len() + random.len() * 2);
    encoded.extend_from_slice(prefix);
    for byte in random {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(encoded).map_err(|source| {
        trigger_io(
            "encode private ephemeral /etc name",
            Path::new("<kernel-random-name>"),
            source.into(),
        )
    })
}

fn trigger_io(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::EphemeralTriggerAuthority {
        source: Box::new(RetainedEphemeralTriggerError::Io {
            operation,
            path: path.to_owned(),
            source,
        }),
    }
}

fn trigger_changed(path: &Path) -> Error {
    Error::EphemeralTriggerAuthority {
        source: Box::new(RetainedEphemeralTriggerError::DirectoryChanged { path: path.to_owned() }),
    }
}

pub(super) fn authority_error(source: Error) -> Error {
    match source {
        Error::EphemeralTriggerAuthority { .. } => source,
        source => Error::EphemeralTriggerAuthority {
            source: Box::new(source),
        },
    }
}

#[derive(Debug, ThisError)]
enum RetainedEphemeralTriggerError {
    #[error("{operation} at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe ephemeral trigger directory `{}` (uid={owner}, mode={mode:04o})", path.display())]
    UnsafeDirectory { path: PathBuf, owner: u32, mode: u32 },
    #[error("ephemeral trigger directory changed while retained at `{}`", path.display())]
    DirectoryChanged { path: PathBuf },
    #[error("ephemeral trigger destination `{}` already exists", path.display())]
    DestinationExists { path: PathBuf },
    #[error("private ephemeral trigger directory `{}` is not empty", path.display())]
    PrivateDirectoryNotEmpty { path: PathBuf },
    #[error("cannot reserve a private ephemeral /etc directory after {limit} attempts")]
    PrivateNamesExhausted { limit: usize },
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_ETC_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_etc_publication(hook: impl FnOnce() + 'static) {
    BEFORE_ETC_PUBLICATION.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "ephemeral /etc publication hook is already armed");
    });
}

#[cfg(test)]
fn before_etc_publication() {
    BEFORE_ETC_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_etc_publication() {}
