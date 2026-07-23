fn copy_asset_exact(input: &mut std::fs::File, output: &mut std::fs::File, expected: u64) -> io::Result<u128> {
    use std::io::Read as _;

    struct ExactWriter<'a> {
        inner: &'a mut std::fs::File,
        expected: u64,
        written: u64,
    }

    impl io::Write for ExactWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let remaining = self.expected.saturating_sub(self.written);
            if bytes.len() as u64 > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("asset output exceeds exact bound {}", self.expected),
                ));
            }
            let written = self.inner.write(bytes)?;
            self.written += written as u64;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    let mut exact = ExactWriter {
        inner: output,
        expected,
        written: 0,
    };
    let mut hasher = StoneDigestWriterHasher::new();
    let copied = io::copy(
        &mut input.take(expected),
        &mut StoneDigestWriter::new(&mut exact, &mut hasher),
    )?;
    if copied != expected || exact.written != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("asset source supplied {copied} bytes, expected exactly {expected}"),
        ));
    }
    Ok(hasher.digest128())
}

async fn publish_download_entry_async(
    staged_directory: &Directory,
    staged_name: &OsStr,
    destination_directory: &Directory,
    destination_name: &OsStr,
    staged_file: std::fs::File,
    staged_fingerprint: FileFingerprint,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
) -> io::Result<std::fs::File> {
    let _lock = lock_directory_async(&destination_directory.file).await?;
    for _ in 0..MAX_PUBLICATION_ATTEMPTS {
        match authenticate_sha256_entry_async(
            destination_directory,
            destination_name,
            expected_hash,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )
        .await
        {
            Ok(winner) => return Ok(winner),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    error = format!("{error:#}"),
                    "Removing invalid cached package before publication"
                );
                unlink_entry(destination_directory, destination_name)?;
            }
        }

        let current_staged = authenticate_sha256_entry_async(
            staged_directory,
            staged_name,
            expected_hash,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )
        .await?;
        let current_fingerprint = validate_regular_metadata(
            &current_staged.metadata()?,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )?;
        if current_fingerprint != staged_fingerprint {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "download stage identity changed before publication",
            ));
        }

        let mut rollback = PublishedEntryGuard::new(destination_directory, destination_name, staged_fingerprint)?;
        match rename_noreplace(staged_directory, staged_name, destination_directory, destination_name) {
            Ok(()) => {
                rollback.arm();
                destination_directory.sync()?;
                let final_file = authenticate_sha256_entry_async(
                    destination_directory,
                    destination_name,
                    expected_hash,
                    exact_size,
                    max_size,
                    Some(PRIVATE_FILE_MODE),
                )
                .await?;
                let final_fingerprint =
                    validate_regular_metadata(&final_file.metadata()?, exact_size, max_size, Some(PRIVATE_FILE_MODE))?;
                let retained_fingerprint = FileFingerprint::from_metadata(&staged_file.metadata()?);
                if final_fingerprint != retained_fingerprint
                    || (retained_fingerprint.device, retained_fingerprint.inode)
                        != (staged_fingerprint.device, staged_fingerprint.inode)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published package is not the authenticated staged inode",
                    ));
                }
                rollback.disarm();
                return Ok(staged_file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "package cache publication did not converge",
    ))
}

fn publish_asset_entry(
    stage: &mut NamedStageFile,
    destination_directory: &Directory,
    destination_name: &OsStr,
    staged_fingerprint: FileFingerprint,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<std::fs::File> {
    let _lock = DirectoryLock::exclusive_until(&destination_directory.file, Instant::now() + PUBLICATION_LOCK_TIMEOUT)?;
    for _ in 0..MAX_PUBLICATION_ATTEMPTS {
        match authenticate_asset_entry(destination_directory, destination_name, expected_digest, expected_size) {
            Ok(winner) => return Ok(winner),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    error = format!("{error:#}"),
                    "Removing invalid asset before publication"
                );
                unlink_entry(destination_directory, destination_name)?;
            }
        }

        let current_stage = authenticate_asset_entry(
            &Directory {
                file: stage.parent.try_clone()?,
                path: destination_directory.path.clone(),
            },
            OsStr::new(&stage.name),
            expected_digest,
            expected_size,
        )?;
        if FileFingerprint::from_metadata(&current_stage.metadata()?) != staged_fingerprint {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "asset stage identity changed before publication",
            ));
        }

        let mut rollback = PublishedEntryGuard::new(destination_directory, destination_name, staged_fingerprint)?;
        match rename_noreplace_raw(
            stage.parent.as_raw_fd(),
            OsStr::new(&stage.name),
            destination_directory.file.as_raw_fd(),
            destination_name,
        ) {
            Ok(()) => {
                rollback.arm();
                stage.mark_moved();
                destination_directory.sync()?;
                let final_file =
                    authenticate_asset_entry(destination_directory, destination_name, expected_digest, expected_size)?;
                let final_fingerprint = FileFingerprint::from_metadata(&final_file.metadata()?);
                let retained_fingerprint = FileFingerprint::from_metadata(&stage.file.metadata()?);
                if final_fingerprint != retained_fingerprint
                    || (retained_fingerprint.device, retained_fingerprint.inode)
                        != (staged_fingerprint.device, staged_fingerprint.inode)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published asset is not the authenticated staged inode",
                    ));
                }
                rollback.disarm();
                return Ok(final_file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "asset publication did not converge",
    ))
}

fn rename_noreplace(
    source_directory: &Directory,
    source_name: &OsStr,
    destination_directory: &Directory,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename_noreplace_raw(
        source_directory.file.as_raw_fd(),
        source_name,
        destination_directory.file.as_raw_fd(),
        destination_name,
    )
}

fn rename_noreplace_raw(
    source_directory: RawFd,
    source_name: &OsStr,
    destination_directory: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    let source_name = cstring(source_name)?;
    let destination_name = cstring(destination_name)?;
    // SAFETY: both descriptors and C strings remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            source_directory,
            source_name.as_ptr(),
            destination_directory,
            destination_name.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlink_entry(directory: &Directory, name: &OsStr) -> io::Result<()> {
    unlinkat(directory.file.as_raw_fd(), name, 0)
}

fn unlinkat(directory: RawFd, name: &OsStr, flags: i32) -> io::Result<()> {
    validate_component(name)?;
    let name = cstring(name)?;
    // SAFETY: descriptor and component remain live for the syscall.
    let result = unsafe { nix::libc::unlinkat(directory, name.as_ptr(), flags) };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    } else {
        Ok(())
    }
}
