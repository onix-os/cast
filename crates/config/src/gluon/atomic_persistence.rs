fn fragment_too_large(size: u64, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("managed Gluon fragment is {size} bytes; limit is {limit} bytes"),
    )
}

fn require_same_generated_fragment(
    directory: &FragmentDirectory,
    name: &OsStr,
    expected: FileSnapshot,
) -> io::Result<()> {
    match inspect_existing_fragment(directory, name, MAX_GENERATED_GLUON_BYTES)? {
        Some(current) if current.generated && current.identity == expected => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed Gluon fragment changed after generated-marker verification",
        )),
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpectedTarget {
    Missing,
    Generated(FileSnapshot),
}

fn atomic_write(
    directory: &FragmentDirectory,
    file_name: &OsStr,
    path: &Path,
    bytes: &[u8],
    expected: ExpectedTarget,
) -> Result<(), SaveGluonError> {
    atomic_write_with_hook(directory, file_name, path, bytes, expected, |_| {})
}

fn atomic_write_with_hook(
    directory: &FragmentDirectory,
    file_name: &OsStr,
    path: &Path,
    bytes: &[u8],
    expected: ExpectedTarget,
    before_commit: impl FnOnce(&Path),
) -> Result<(), SaveGluonError> {
    directory
        .verify_path()
        .map_err(|source| SaveGluonError::CreateTemporary {
            path: directory.path.clone(),
            source,
        })?;
    let mut temporary = None;
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let random = random_hex().map_err(|source| SaveGluonError::CreateTemporary {
            path: directory.path.clone(),
            source,
        })?;
        let candidate = OsString::from(format!("{TEMPORARY_PREFIX}{random}"));
        match directory.open_at(
            &candidate,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            // Preserve the previous OpenOptions default: the process umask
            // derives the final managed-fragment mode from 0o666.
            0o666,
        ) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(SaveGluonError::CreateTemporary {
                    path: directory.display_path(&candidate),
                    source,
                });
            }
        }
    }
    let (temporary_name, mut file) = temporary.ok_or_else(|| SaveGluonError::CreateTemporary {
        path: directory.path.clone(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "unable to allocate a temporary fragment name",
        ),
    })?;
    let temporary_path = directory.display_path(&temporary_name);
    let mut guard = TemporaryGuard {
        directory,
        name: Some(temporary_name.clone()),
    };
    let result = (|| {
        file.write_all(bytes).map_err(|source| SaveGluonError::WriteTemporary {
            path: temporary_path.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| SaveGluonError::SyncTemporary {
            path: temporary_path.clone(),
            source,
        })?;
        drop(file);
        before_commit(&temporary_path);
        directory.verify_path().map_err(|source| SaveGluonError::ReadExisting {
            path: path.to_owned(),
            source,
        })?;
        let no_replace = match expected {
            ExpectedTarget::Missing => true,
            ExpectedTarget::Generated(identity) => {
                require_same_generated_fragment(directory, file_name, identity).map_err(|source| {
                    SaveGluonError::ReadExisting {
                        path: path.to_owned(),
                        source,
                    }
                })?;
                false
            }
        };
        directory
            .rename(&temporary_name, file_name, no_replace)
            .map_err(|source| SaveGluonError::Rename {
                from: temporary_path.clone(),
                to: path.to_owned(),
                source,
            })?;
        guard.name = None;
        directory.sync().map_err(|source| SaveGluonError::SyncDirectory {
            path: directory.path.clone(),
            source,
        })?;
        directory.verify_path().map_err(|source| SaveGluonError::SyncDirectory {
            path: directory.path.clone(),
            source,
        })
    })();

    match result {
        Ok(()) => Ok(()),
        Err(error) => match guard.cleanup() {
            Ok(()) => Err(error),
            Err(source) => Err(SaveGluonError::CleanupTemporary {
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
        let result = unsafe { libc::getrandom(bytes[filled..].as_mut_ptr().cast(), bytes.len() - filled, 0) };
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
        filled += usize::try_from(result).expect("getrandom returned a non-negative byte count");
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
    directory: &'a FragmentDirectory,
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
