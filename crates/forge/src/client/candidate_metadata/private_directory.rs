//! Private creation and no-replace publication of candidate directories.

use std::ffi::CString;

use super::*;

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CANONICAL_DIRECTORY_MODE: u32 = 0o755;
const MAX_INTERRUPTS: usize = 1_024;
const MAX_PRIVATE_DIRECTORY_ATTEMPTS: usize = 256;
const PRIVATE_DIRECTORY_RANDOM_BYTES: usize = 16;

impl RetainedDirectory {
    /// Create a fresh directory behind a kernel-random private name, retain
    /// that exact inode, and only then publish it with no-replace.
    /// A failure leaves private residue inside the already-private candidate;
    /// the outer repair guard preserves that whole wrapper opaquely rather
    /// than guessing which name is safe to remove.
    pub(super) fn create_private_and_publish(parent: &File, name: &CStr, path: PathBuf) -> Result<Self, MetadataError> {
        for _ in 0..MAX_PRIVATE_DIRECTORY_ATTEMPTS {
            let private_name = random_private_directory_name()?;
            let private_path = path
                .parent()
                .expect("metadata lib path has a parent")
                .join(private_name.to_string_lossy().as_ref());
            match mkdir_private(parent, &private_name) {
                Ok(()) => {}
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(metadata_io(
                        "create private candidate metadata directory",
                        private_path,
                        source,
                    ));
                }
            }

            let pinned = openat2_file(
                parent.as_raw_fd(),
                &private_name,
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                controlled_resolution(),
            )
            .map_err(|source| metadata_io("retain private metadata directory", &private_path, source))?;
            let metadata = pinned
                .metadata()
                .map_err(|source| metadata_io("inspect private metadata directory", &private_path, source))?;
            let mode = metadata.permissions().mode() & 0o7777;
            if !metadata.file_type().is_dir()
                || metadata.uid() != effective_user_id()
                || mode & !PRIVATE_DIRECTORY_MODE != 0
            {
                return Err(MetadataError::UnsafeDirectory {
                    path: private_path,
                    kind: file_type_name(&metadata.file_type()),
                    owner: metadata.uid(),
                    mode,
                });
            }
            crate::linux_fs::chmod_path_descriptor(&pinned, CANONICAL_DIRECTORY_MODE)
                .map_err(|source| metadata_io("normalize private metadata directory", &private_path, source))?;
            let expected = directory_witness(&pinned, &private_path)?;
            let private = Self::open(parent, &private_name, private_path.clone())?;
            if private.witness != expected {
                return Err(MetadataError::DirectoryChanged { path: private_path });
            }
            private.require_empty()?;

            let publication = publish_private_directory_once(parent, &private_name, name);
            let canonical = Self::open(parent, name, path.clone());
            match (publication, canonical) {
                (Ok(()), Ok(canonical)) | (Err(_), Ok(canonical)) if canonical.witness == expected => {
                    // A reported error remains ambiguous: the move may have
                    // reached the namespace. Once exact reconciliation proves
                    // that it did, finish the same durability and
                    // revalidation suffix as an ordinary syscall success.
                    require_name_absent(parent, &private_name, &private_path)?;
                    canonical.require_empty()?;
                    parent.sync_all().map_err(|source| {
                        metadata_io(
                            "sync candidate /usr after metadata directory publication",
                            &path,
                            source,
                        )
                    })?;
                    after_parent_sync();
                    canonical.require_named(parent, name)?;
                    return Ok(canonical);
                }
                (Err(source), _) => {
                    return Err(MetadataError::PublicationCollision { path, source });
                }
                (Ok(()), _) => return Err(MetadataError::DirectoryChanged { path }),
            }
        }
        Err(MetadataError::PrivateDirectoryExhausted {
            limit: MAX_PRIVATE_DIRECTORY_ATTEMPTS,
        })
    }
}

fn publish_private_directory_once(parent: &File, private_name: &CStr, name: &CStr) -> io::Result<()> {
    let result = crate::linux_fs::renameat2_noreplace_once(parent, private_name, parent, name);
    #[cfg(test)]
    if result.is_ok() && REPORT_APPLIED_PUBLICATION_ERROR.with(|armed| armed.replace(false)) {
        APPLIED_PUBLICATION_ERROR_REPORTED.with(|reported| {
            assert!(
                !reported.replace(true),
                "applied metadata-directory publication error is already pending"
            );
        });
        return Err(io::Error::from_raw_os_error(nix::libc::EIO));
    }
    result
}

#[cfg(test)]
std::thread_local! {
    static REPORT_APPLIED_PUBLICATION_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static APPLIED_PUBLICATION_ERROR_REPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_applied_publication_error(after_parent_sync: impl FnOnce() + 'static) {
    REPORT_APPLIED_PUBLICATION_ERROR.with(|armed| {
        assert!(
            !armed.replace(true),
            "applied metadata-directory publication error is already armed"
        );
    });
    APPLIED_PUBLICATION_ERROR_REPORTED.with(|reported| {
        assert!(
            !reported.get(),
            "applied metadata-directory publication error is already pending"
        );
    });
    AFTER_PARENT_SYNC.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(after_parent_sync));
        assert!(
            previous.is_none(),
            "metadata-directory parent-sync hook is already armed"
        );
    });
}

fn after_parent_sync() {
    #[cfg(test)]
    if APPLIED_PUBLICATION_ERROR_REPORTED.with(|reported| reported.replace(false)) {
        AFTER_PARENT_SYNC.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
    }
}

fn random_private_directory_name() -> Result<CString, MetadataError> {
    let mut random = [0_u8; PRIVATE_DIRECTORY_RANDOM_BYTES];
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
            return Err(metadata_io(
                "generate private metadata directory name",
                "<kernel-random-name>",
                source,
            ));
        }
        let read = usize::try_from(result).map_err(|_| {
            metadata_io(
                "generate private metadata directory name",
                "<kernel-random-name>",
                io::Error::other("getrandom returned an invalid length"),
            )
        })?;
        if read == 0 || read > random.len() - filled {
            return Err(metadata_io(
                "generate private metadata directory name",
                "<kernel-random-name>",
                io::Error::new(io::ErrorKind::UnexpectedEof, "getrandom returned a short result"),
            ));
        }
        filled += read;
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b".forge-metadata-directory-";
    let mut encoded = Vec::with_capacity(prefix.len() + random.len() * 2);
    encoded.extend_from_slice(prefix);
    for byte in random {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(encoded).map_err(|source| {
        metadata_io(
            "encode private metadata directory name",
            "<kernel-random-name>",
            source.into(),
        )
    })
}

fn mkdir_private(parent: &File, name: &CStr) -> io::Result<()> {
    let mut interruptions = 0usize;
    loop {
        // SAFETY: parent and the generated single-component C string remain
        // live; mkdirat neither follows nor replaces the final name.
        if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), PRIVATE_DIRECTORY_MODE) } == 0 {
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

fn require_name_absent(parent: &File, name: &CStr, path: &Path) -> Result<(), MetadataError> {
    match openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Err(source) => Err(metadata_io("prove private metadata name absence", path, source)),
        Ok(_) => Err(MetadataError::DirectoryChanged { path: path.to_owned() }),
    }
}

pub(super) fn require_empty_directory(directory: &File, path: &Path) -> Result<(), MetadataError> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(metadata_io(
            "duplicate private metadata directory for enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // The duplicate shares the retained directory offset. Rewind every scan.
    // SAFETY: duplicate is a fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: duplicate remains uniquely owned after failed lseek.
        unsafe { nix::libc::close(duplicate) };
        return Err(metadata_io("rewind private metadata directory", path, source));
    }
    // SAFETY: fdopendir consumes duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(metadata_io("enumerate private metadata directory", path, source));
    }

    let result = loop {
        // SAFETY: errno is thread-local on Linux.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(metadata_io("enumerate private metadata directory", path, source))
            };
        }
        // SAFETY: Linux dirent names are NUL terminated for the live entry.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        break Err(MetadataError::PrivateDirectoryNotEmpty {
            path: path.to_owned(),
            entry: String::from_utf8_lossy(name).into_owned(),
        });
    };
    // SAFETY: stream was returned by fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(metadata_io(
            "close private metadata directory enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}
