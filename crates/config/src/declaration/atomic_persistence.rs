use std::{ffi::OsString, io, path::Path};

use super::{
    managed_directory::{
        FileSnapshot, ManagedDirectory, inspect_existing_declaration,
    },
    storage_error::SaveDeclarationError,
};

const TEMPORARY_RANDOM_BYTES: usize = 16;
const TEMPORARY_CREATE_ATTEMPTS: usize = 100;

pub(crate) fn require_same_generated_declaration(
    directory: &ManagedDirectory,
    name: &std::ffi::OsStr,
    expected: FileSnapshot,
    size_limit: usize,
    ownership_marker: &[u8],
) -> io::Result<()> {
    match inspect_existing_declaration(
        directory,
        name,
        size_limit,
        ownership_marker,
    )? {
        Some(current)
            if current.is_generated() && current.identity() == expected =>
        {
            Ok(())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed declaration changed after ownership-marker verification",
        )),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExpectedTarget {
    Missing,
    Generated(FileSnapshot),
}

pub(crate) struct AtomicWrite<'a> {
    pub(crate) directory: &'a ManagedDirectory,
    pub(crate) file_name: &'a std::ffi::OsStr,
    pub(crate) path: &'a Path,
    pub(crate) bytes: &'a [u8],
    pub(crate) expected: ExpectedTarget,
    pub(crate) size_limit: usize,
    pub(crate) ownership_marker: &'a [u8],
    pub(crate) temporary_prefix: &'a str,
}

pub(crate) fn atomic_write_with_hook(
    request: AtomicWrite<'_>,
    before_commit: impl FnOnce(&Path),
) -> Result<(), SaveDeclarationError> {
    let AtomicWrite {
        directory,
        file_name,
        path,
        bytes,
        expected,
        size_limit,
        ownership_marker,
        temporary_prefix,
    } = request;
    directory
        .verify_path()
        .map_err(|source| SaveDeclarationError::CreateTemporary {
            path: directory.path().to_owned(),
            source,
        })?;
    let mut temporary = None;
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let random = random_hex().map_err(|source| {
            SaveDeclarationError::CreateTemporary {
                path: directory.path().to_owned(),
                source,
            }
        })?;
        let candidate = OsString::from(format!("{temporary_prefix}{random}"));
        match directory.open_at(
            &candidate,
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW,
            0o666,
        ) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(SaveDeclarationError::CreateTemporary {
                    path: directory.display_path(&candidate),
                    source,
                });
            }
        }
    }
    let (temporary_name, mut file) = temporary.ok_or_else(|| {
        SaveDeclarationError::CreateTemporary {
            path: directory.path().to_owned(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "unable to allocate a temporary declaration name",
            ),
        }
    })?;
    let temporary_path = directory.display_path(&temporary_name);
    let mut guard = TemporaryGuard {
        directory,
        name: Some(temporary_name.clone()),
    };
    let result = (|| {
        use std::io::Write as _;

        file.write_all(bytes)
            .map_err(|source| SaveDeclarationError::WriteTemporary {
                path: temporary_path.clone(),
                source,
            })?;
        file.sync_all()
            .map_err(|source| SaveDeclarationError::SyncTemporary {
                path: temporary_path.clone(),
                source,
            })?;
        drop(file);
        before_commit(&temporary_path);
        directory
            .verify_path()
            .map_err(|source| SaveDeclarationError::ReadExisting {
                path: path.to_owned(),
                source,
            })?;
        let no_replace = match expected {
            ExpectedTarget::Missing => true,
            ExpectedTarget::Generated(identity) => {
                require_same_generated_declaration(
                    directory,
                    file_name,
                    identity,
                    size_limit,
                    ownership_marker,
                )
                .map_err(|source| SaveDeclarationError::ReadExisting {
                    path: path.to_owned(),
                    source,
                })?;
                false
            }
        };
        directory
            .rename(&temporary_name, file_name, no_replace)
            .map_err(|source| SaveDeclarationError::Rename {
                from: temporary_path.clone(),
                to: path.to_owned(),
                source,
            })?;
        guard.name = None;
        directory
            .sync()
            .map_err(|source| SaveDeclarationError::SyncDirectory {
                path: directory.path().to_owned(),
                source,
            })?;
        directory
            .verify_path()
            .map_err(|source| SaveDeclarationError::SyncDirectory {
                path: directory.path().to_owned(),
                source,
            })
    })();

    match result {
        Ok(()) => Ok(()),
        Err(error) => match guard.cleanup() {
            Ok(()) => Err(error),
            Err(source) => Err(SaveDeclarationError::CleanupTemporary {
                path: temporary_path,
                source,
            }),
        },
    }
}

fn random_hex() -> io::Result<String> {
    let mut bytes = [0_u8; TEMPORARY_RANDOM_BYTES];
    let mut filled = 0;
    while filled < bytes.len() {
        // SAFETY: the pointer addresses the remaining initialized byte array,
        // and getrandom writes at most the supplied length.
        let result = unsafe {
            libc::getrandom(
                bytes[filled..].as_mut_ptr().cast(),
                bytes.len() - filled,
                0,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned no temporary-name entropy",
            ));
        }
        filled += usize::try_from(result)
            .expect("getrandom returned a non-negative byte count");
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

struct TemporaryGuard<'a> {
    directory: &'a ManagedDirectory,
    name: Option<OsString>,
}

impl TemporaryGuard<'_> {
    fn cleanup(&mut self) -> io::Result<()> {
        let Some(name) = self.name.take() else {
            return Ok(());
        };
        match self.directory.unlink(&name) {
            Ok(()) => self.directory.sync(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for TemporaryGuard<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
