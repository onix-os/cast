use super::{execution::analyzer_cleanup_deadline, sandbox::*, *};

/// A sealed, descriptor-authenticated copy of a collected regular file.
/// It is the immutable source for private external-tool sandboxes and never
/// exposes the mutable output-tree path.
#[derive(Debug)]
pub(in super::super) struct VerifiedAnalyzerInput {
    pub(super) file: StdFile,
    pub(super) size: u64,
}

impl VerifiedAnalyzerInput {
    pub(in super::super) fn from_path_info(info: &PathInfo, byte_limit: u64) -> Result<Self, BoxError> {
        info.check_deadline()?;
        if info.size > byte_limit {
            return Err(Box::new(AnalyzerInputError::TooLarge {
                path: info.target_path.clone(),
                size: info.size,
                limit: byte_limit,
            }));
        }

        let mut source = info.open_verified()?;
        // SAFETY: the name is a static NUL-terminated string and successful
        // memfd_create returns a fresh descriptor owned below.
        let descriptor = unsafe {
            nix::libc::memfd_create(
                c"mason-analyzer-input".as_ptr(),
                nix::libc::MFD_ALLOW_SEALING | nix::libc::MFD_CLOEXEC,
            )
        };
        if descriptor == -1 {
            return Err(Box::new(AnalyzerInputError::Create {
                path: info.target_path.clone(),
                source: std::io::Error::last_os_error(),
            }));
        }
        // SAFETY: memfd_create returned a unique owned descriptor.
        let mut writable = unsafe { StdFile::from_raw_fd(descriptor) };
        let copied = std::io::copy(&mut source, &mut writable).map_err(|source| AnalyzerInputError::Copy {
            path: info.target_path.clone(),
            source,
        })?;
        source.finish()?;
        if copied != info.size {
            return Err(Box::new(AnalyzerInputError::Length {
                path: info.target_path.clone(),
                expected: info.size,
                actual: copied,
            }));
        }
        writable.sync_all().map_err(|source| AnalyzerInputError::Sync {
            path: info.target_path.clone(),
            source,
        })?;
        writable
            .set_permissions(Permissions::from_mode(0o400))
            .map_err(|source| AnalyzerInputError::Protect {
                path: info.target_path.clone(),
                source,
            })?;
        let required_seals =
            nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
        // SAFETY: writable is a live sealable memfd and the third argument is
        // the documented bitmask for F_ADD_SEALS.
        if unsafe { nix::libc::fcntl(writable.as_raw_fd(), nix::libc::F_ADD_SEALS, required_seals) } == -1 {
            return Err(Box::new(AnalyzerInputError::Seal {
                path: info.target_path.clone(),
                source: std::io::Error::last_os_error(),
            }));
        }
        // SAFETY: F_GET_SEALS takes no variadic argument for a live memfd.
        let actual_seals = unsafe { nix::libc::fcntl(writable.as_raw_fd(), nix::libc::F_GET_SEALS) };
        if actual_seals == -1 || actual_seals & required_seals != required_seals {
            return Err(Box::new(AnalyzerInputError::SealsMissing {
                path: info.target_path.clone(),
                expected: required_seals,
                actual: actual_seals,
            }));
        }

        writable
            .seek(SeekFrom::Start(0))
            .map_err(|source| AnalyzerInputError::Rewind {
                path: info.target_path.clone(),
                source,
            })?;
        info.check_deadline()?;
        Ok(Self {
            file: writable,
            size: copied,
        })
    }

    pub(in super::super) fn try_clone(&self) -> Result<StdFile, BoxError> {
        let mut file = self.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }

    pub(in super::super) fn read_all(&self, byte_limit: usize) -> Result<Vec<u8>, BoxError> {
        let limit = u64::try_from(byte_limit).unwrap_or(u64::MAX);
        if self.size > limit {
            return Err(Box::new(AnalyzerInputError::TooLarge {
                path: PathBuf::from("<verified analyzer input>"),
                size: self.size,
                limit,
            }));
        }
        let capacity = usize::try_from(self.size).map_err(|_| AnalyzerInputError::Allocation { size: self.size })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|source| AnalyzerInputError::Reserve {
                size: self.size,
                detail: source.to_string(),
            })?;
        self.try_clone()?.read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != self.size {
            return Err(Box::new(AnalyzerInputError::Length {
                path: PathBuf::from("<verified analyzer input>"),
                expected: self.size,
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            }));
        }
        Ok(bytes)
    }
}

/// Private, pathname-based input for analyzers which cannot consume an open
/// descriptor directly. The path lives only beneath the container's empty
/// disposable `/tmp`, has a fixed caller-selected basename, and is verified
/// and removed after every normally finalized child result. An abnormal Drop
/// retains an ineligible tombstone for mount-namespace teardown rather than
/// racing recursive cleanup. No procfs is required.
pub(in super::super) struct ExternalAnalyzerInput {
    directory: Option<tempfile::TempDir>,
    parent_file: StdFile,
    directory_name: CString,
    pub(super) directory_file: StdFile,
    file: StdFile,
    file_name: CString,
    path: PathBuf,
    expected_directory: SandboxSnapshot,
    expected_file: SandboxSnapshot,
    expected_digest: [u8; 32],
}

/// A private writable copy for analyzers such as objcopy and strip which need
/// a pathname they may replace. The mutable pathname never points into the
/// collected output tree. Once the complete analyzer process boundary has
/// exited, the final single regular file is read into bounded memory, the
/// private directory is verified and removed, and only those bytes may be
/// handed to the collector's replacement transaction.
pub(in super::super) struct ExternalAnalyzerMutation(ExternalAnalyzerInput);

impl ExternalAnalyzerMutation {
    pub(in super::super) fn new(
        input: &VerifiedAnalyzerInput,
        display_path: &Path,
        file_name: &str,
        directory_suffix: &str,
    ) -> Result<Self, BoxError> {
        let mut sandbox = ExternalAnalyzerInput::new(input, display_path, file_name, directory_suffix)?;
        let preparation = (|| -> Result<(), BoxError> {
            // SAFETY: these are live descriptors for the private file and
            // directory constructed above. The immutable source remains
            // sealed; only this disposable copy becomes writable.
            if unsafe { nix::libc::fchmod(sandbox.file.as_raw_fd(), 0o600) } == -1 {
                return Err(Box::new(AnalyzerSandboxError::ProtectFile {
                    path: display_path.to_owned(),
                    source: std::io::Error::last_os_error(),
                }));
            }
            if unsafe { nix::libc::fchmod(sandbox.directory_file.as_raw_fd(), 0o700) } == -1 {
                return Err(Box::new(AnalyzerSandboxError::ProtectDirectory {
                    path: display_path.to_owned(),
                    source: std::io::Error::last_os_error(),
                }));
            }
            sandbox
                .directory_file
                .sync_all()
                .map_err(|source| AnalyzerSandboxError::SyncDirectory {
                    path: display_path.to_owned(),
                    source,
                })?;
            sandbox.expected_directory = SandboxSnapshot::from_metadata(&sandbox.directory_file.metadata()?);
            sandbox.expected_file = SandboxSnapshot::from_metadata(&sandbox.file.metadata()?);
            Ok(())
        })();

        match preparation {
            Ok(()) => Ok(Self(sandbox)),
            Err(operation) => match sandbox.cleanup(true) {
                Ok(()) => Err(operation),
                Err(finalization) => Err(Box::new(AnalyzerOperationFinalizationError {
                    operation,
                    finalization,
                })),
            },
        }
    }

    pub(in super::super) fn path(&self) -> &Path {
        self.0.path()
    }

    pub(in super::super) fn output_path(&self, file_name: &str) -> Result<PathBuf, BoxError> {
        validate_sandbox_component(file_name)?;
        if self.0.path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            return Err(Box::new(AnalyzerSandboxError::InvalidName {
                name: file_name.to_owned(),
            }));
        }
        Ok(self.0.working_directory().join(file_name))
    }

    /// Finalize a mutable analyzer operation. A failed child result is cleaned
    /// without attempting to consume its partial output. A successful child
    /// must leave exactly one single-link regular file at the authenticated
    /// private name; its bytes are bounded before allocation and reverified
    /// before the sandbox is removed.
    pub(in super::super) fn finish(
        self,
        info: &PathInfo,
        operation: Result<(), BoxError>,
        byte_limit: u64,
    ) -> Result<Vec<u8>, BoxError> {
        self.finish_inner(info, operation, byte_limit, None)
            .map(|(primary, generated)| {
                debug_assert!(generated.is_none());
                primary
            })
    }

    pub(in super::super) fn finish_with_output(
        self,
        info: &PathInfo,
        operation: Result<(), BoxError>,
        byte_limit: u64,
        output_name: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), BoxError> {
        self.finish_inner(info, operation, byte_limit, Some(output_name))
            .map(|(primary, generated)| (primary, generated.expect("requested mutable analyzer output")))
    }

    fn finish_inner(
        self,
        info: &PathInfo,
        operation: Result<(), BoxError>,
        byte_limit: u64,
        output_name: Option<&str>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), BoxError> {
        let mut sandbox = self.0;
        let output_name = output_name
            .map(|name| {
                validate_sandbox_component(name)?;
                if sandbox.path.file_name().and_then(|candidate| candidate.to_str()) == Some(name) {
                    return Err(Box::new(AnalyzerSandboxError::InvalidName { name: name.to_owned() }) as BoxError);
                }
                CString::new(name)
                    .map_err(|_| Box::new(AnalyzerSandboxError::InvalidName { name: name.to_owned() }) as BoxError)
            })
            .transpose();
        let (bytes, verification) = if operation.is_ok() {
            match output_name.and_then(|name| sandbox.read_mutated_output(info, byte_limit, name.as_deref())) {
                Ok(bytes) => (Some(bytes), Ok(())),
                Err(error) => (None, Err(error)),
            }
        } else {
            (None, Ok(()))
        };
        let cleanup = sandbox.cleanup(true);
        let deadline = info.check_deadline().map_err(|error| Box::new(error) as BoxError);
        let finalization = combine_finalization_errors(verification, cleanup, deadline);

        match (operation, bytes, finalization) {
            (Ok(()), Some(bytes), Ok(())) => Ok(bytes),
            (Ok(()), Some(_), Err(finalization)) => Err(finalization),
            (Ok(()), None, Err(finalization)) => Err(finalization),
            (Err(operation), None, Ok(())) => Err(operation),
            (Err(operation), None, Err(finalization)) => Err(Box::new(AnalyzerOperationFinalizationError {
                operation,
                finalization,
            })),
            (Ok(()), None, Ok(())) | (Err(_), Some(_), _) => {
                unreachable!("mutable analyzer finalization state is internally consistent")
            }
        }
    }
}

impl ExternalAnalyzerInput {
    pub(in super::super) fn new(
        input: &VerifiedAnalyzerInput,
        display_path: &Path,
        file_name: &str,
        directory_suffix: &str,
    ) -> Result<Self, BoxError> {
        let file_name_c = CString::new(file_name).map_err(|_| AnalyzerSandboxError::InvalidName {
            name: file_name.to_owned(),
        })?;
        if Path::new(file_name).file_name().and_then(|name| name.to_str()) != Some(file_name) {
            return Err(Box::new(AnalyzerSandboxError::InvalidName {
                name: file_name.to_owned(),
            }));
        }

        let directory = tempfile::Builder::new()
            .prefix(".mason-analyzer-")
            .suffix(directory_suffix)
            .tempdir_in("/tmp")
            .map_err(|source| AnalyzerSandboxError::CreateDirectory {
                path: display_path.to_owned(),
                source,
            })?;
        let mut pending_directory = Some(directory);
        let construction = (|| -> Result<Self, BoxError> {
            let directory_path = pending_directory
                .as_ref()
                .expect("pending analyzer sandbox")
                .path()
                .to_owned();
            let parent_path = directory_path
                .parent()
                .ok_or_else(|| AnalyzerSandboxError::InvalidDirectoryPath {
                    path: directory_path.clone(),
                })?;
            let directory_name =
                directory_path
                    .file_name()
                    .ok_or_else(|| AnalyzerSandboxError::InvalidDirectoryPath {
                        path: directory_path.clone(),
                    })?;
            let directory_name =
                CString::new(directory_name.as_bytes()).map_err(|_| AnalyzerSandboxError::InvalidDirectoryPath {
                    path: directory_path.clone(),
                })?;
            let parent_file = StdOpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
                .open(parent_path)
                .map_err(|source| AnalyzerSandboxError::OpenDirectory {
                    path: parent_path.to_owned(),
                    source,
                })?;
            let directory_file = StdOpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
                .open(&directory_path)
                .map_err(|source| AnalyzerSandboxError::OpenDirectory {
                    path: display_path.to_owned(),
                    source,
                })?;

            // SAFETY: directory_file is live, file_name_c is a single NUL-free
            // component, and a successful openat returns a fresh descriptor.
            let descriptor = unsafe {
                nix::libc::openat(
                    directory_file.as_raw_fd(),
                    file_name_c.as_ptr(),
                    nix::libc::O_RDWR
                        | nix::libc::O_CREAT
                        | nix::libc::O_EXCL
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_CLOEXEC
                        | nix::libc::O_NONBLOCK,
                    0o600,
                )
            };
            if descriptor == -1 {
                return Err(Box::new(AnalyzerSandboxError::CreateFile {
                    path: display_path.to_owned(),
                    source: std::io::Error::last_os_error(),
                }));
            }
            // SAFETY: openat returned a fresh owned descriptor.
            let mut writable = unsafe { StdFile::from_raw_fd(descriptor) };
            let mut source = input.try_clone()?;
            let copied =
                std::io::copy(&mut source, &mut writable).map_err(|source| AnalyzerSandboxError::WriteFile {
                    path: display_path.to_owned(),
                    source,
                })?;
            if copied != input.size {
                return Err(Box::new(AnalyzerSandboxError::Length {
                    path: display_path.to_owned(),
                    expected: input.size,
                    actual: copied,
                }));
            }
            writable.sync_all().map_err(|source| AnalyzerSandboxError::SyncFile {
                path: display_path.to_owned(),
                source,
            })?;
            writable
                .set_permissions(Permissions::from_mode(0o400))
                .map_err(|source| AnalyzerSandboxError::ProtectFile {
                    path: display_path.to_owned(),
                    source,
                })?;

            let file = open_sandbox_file(&directory_file, &file_name_c, display_path)?;
            drop(writable);
            // SAFETY: directory_file is a live descriptor owned by this sandbox.
            if unsafe { nix::libc::fchmod(directory_file.as_raw_fd(), 0o500) } == -1 {
                return Err(Box::new(AnalyzerSandboxError::ProtectDirectory {
                    path: display_path.to_owned(),
                    source: std::io::Error::last_os_error(),
                }));
            }
            directory_file
                .sync_all()
                .map_err(|source| AnalyzerSandboxError::SyncDirectory {
                    path: display_path.to_owned(),
                    source,
                })?;

            let expected_digest = digest_sandbox_file(input.try_clone()?, input.size, display_path)?;
            let expected_directory = SandboxSnapshot::from_metadata(&directory_file.metadata()?);
            let expected_file = SandboxSnapshot::from_metadata(&file.metadata()?);
            let path = directory_path.join(file_name);
            Ok(Self {
                directory: pending_directory.take(),
                parent_file,
                directory_name,
                directory_file,
                file,
                file_name: file_name_c,
                path,
                expected_directory,
                expected_file,
                expected_digest,
            })
        })();

        match construction {
            Ok(sandbox) => Ok(sandbox),
            Err(operation) => {
                let directory = pending_directory.take().expect("failed analyzer sandbox construction");
                let cleanup = cleanup_unfinished_sandbox(directory, display_path);
                match cleanup {
                    Ok(()) => Err(operation),
                    Err(finalization) => Err(Box::new(AnalyzerOperationFinalizationError {
                        operation,
                        finalization,
                    })),
                }
            }
        }
    }

    pub(in super::super) fn path(&self) -> &Path {
        &self.path
    }

    pub(in super::super) fn working_directory(&self) -> &Path {
        self.directory.as_ref().expect("live analyzer sandbox").path()
    }

    /// Verify and clean the sandbox regardless of the analyzer result. A
    /// cleanup failure is never hidden behind an earlier analyzer failure.
    pub(in super::super) fn finish<T>(
        mut self,
        info: &PathInfo,
        operation: Result<T, BoxError>,
    ) -> Result<T, BoxError> {
        let verification = self.verify();
        let cleanup = self.cleanup(true);
        let deadline = info.check_deadline().map_err(|error| Box::new(error) as BoxError);
        let finalization = combine_finalization_errors(verification, cleanup, deadline);
        match (operation, finalization) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(operation), Ok(())) => Err(operation),
            (Ok(_), Err(finalization)) => Err(finalization),
            (Err(operation), Err(finalization)) => Err(Box::new(AnalyzerOperationFinalizationError {
                operation,
                finalization,
            })),
        }
    }

    fn verify(&self) -> Result<(), BoxError> {
        require_sandbox_snapshot(
            &self.path,
            self.expected_directory,
            &self.directory_file.metadata()?,
            "analyzer sandbox directory descriptor",
        )?;
        let directory_path = self.working_directory();
        let directory_from_parent =
            open_sandbox_directory(&self.parent_file, &self.directory_name).map_err(|source| {
                AnalyzerSandboxError::Inspect {
                    path: directory_path.to_owned(),
                    source,
                }
            })?;
        require_sandbox_snapshot(
            directory_path,
            self.expected_directory,
            &directory_from_parent.metadata()?,
            "analyzer sandbox directory path",
        )?;
        verify_sandbox_inventory(
            &self.directory_file,
            directory_path,
            self.path.file_name().expect("sandbox file name"),
            self.expected_directory,
        )?;
        require_sandbox_snapshot(
            &self.path,
            self.expected_file,
            &self.file.metadata()?,
            "analyzer sandbox file descriptor",
        )?;
        let reopened = open_sandbox_file(&self.directory_file, &self.file_name, &self.path)?;
        require_sandbox_snapshot(
            &self.path,
            self.expected_file,
            &reopened.metadata()?,
            "analyzer sandbox file path",
        )?;
        let actual_digest = digest_sandbox_file(reopened, self.expected_file.size, &self.path)?;
        if actual_digest != self.expected_digest {
            return Err(Box::new(AnalyzerSandboxError::DigestChanged {
                path: self.path.clone(),
            }));
        }
        Ok(())
    }

    fn read_mutated_output(
        &self,
        info: &PathInfo,
        byte_limit: u64,
        output_name: Option<&CStr>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), BoxError> {
        require_sandbox_node(
            &self.path,
            self.expected_directory,
            &self.directory_file.metadata()?,
            "mutable analyzer directory descriptor",
        )?;
        let directory_path = self.working_directory();
        let directory_from_parent =
            open_sandbox_directory(&self.parent_file, &self.directory_name).map_err(|source| {
                AnalyzerSandboxError::Inspect {
                    path: directory_path.to_owned(),
                    source,
                }
            })?;
        require_sandbox_node(
            directory_path,
            self.expected_directory,
            &directory_from_parent.metadata()?,
            "mutable analyzer directory path",
        )?;
        let expected_names = match output_name {
            Some(output) => vec![self.file_name.as_c_str(), output],
            None => vec![self.file_name.as_c_str()],
        };
        verify_mutated_sandbox_inventory(&self.directory_file, directory_path, &expected_names)?;

        let primary =
            read_mutated_sandbox_regular(info, &self.directory_file, &self.file_name, &self.path, byte_limit)?;
        let generated = output_name
            .map(|name| {
                let path = directory_path.join(OsStr::from_bytes(name.to_bytes()));
                read_mutated_sandbox_regular(info, &self.directory_file, name, &path, byte_limit)
            })
            .transpose()?;
        verify_mutated_sandbox_inventory(&self.directory_file, directory_path, &expected_names)?;
        require_sandbox_node(
            directory_path,
            self.expected_directory,
            &self.directory_file.metadata()?,
            "mutable analyzer directory after output read",
        )?;
        let directory_from_parent =
            open_sandbox_directory(&self.parent_file, &self.directory_name).map_err(|source| {
                AnalyzerSandboxError::Inspect {
                    path: directory_path.to_owned(),
                    source,
                }
            })?;
        require_sandbox_node(
            directory_path,
            self.expected_directory,
            &directory_from_parent.metadata()?,
            "mutable analyzer directory path after output read",
        )?;
        info.check_deadline()?;
        Ok((primary, generated))
    }

    fn cleanup(&mut self, remove_attached: bool) -> Result<(), BoxError> {
        let Some(directory) = self.directory.take() else {
            return Ok(());
        };
        let mut errors = Vec::new();
        if !remove_attached {
            // Drop may run while an unexpected panic is unwinding through an
            // analyzer call. With no child handle here, recursively deleting
            // names could race the still-running child and target a
            // replacement. Retain an inaccessible, ineligible tombstone; the
            // surrounding disposable mount namespace is then torn down.
            // SAFETY: directory_file remains a live descriptor.
            if unsafe { nix::libc::fchmod(self.directory_file.as_raw_fd(), 0) } == -1 {
                errors.push(format!(
                    "disable unfinished analyzer directory: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let retained = directory.keep();
            errors.push(AnalyzerSandboxError::UnfinishedDirectory { path: retained }.to_string());
            return Err(Box::new(AnalyzerSandboxError::Finalization {
                details: errors.join("; "),
            }));
        }
        // `finish` is called only after `checked_output_for` has terminated and
        // reaped the complete analyzer PID boundary. Frozen execution also
        // gives this code a fresh, otherwise-empty tmpfs at /tmp. Consequently
        // every same-mount descendant here was created by this analyzer; mount
        // crossings are independently rejected by openat2(NO_XDEV).
        // SAFETY: directory_file is a live descriptor owned by this sandbox.
        if unsafe { nix::libc::fchmod(self.directory_file.as_raw_fd(), 0o700) } == -1 {
            errors.push(format!(
                "restore directory permissions: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut remaining_entries = SANDBOX_CLEANUP_ENTRY_LIMIT;
        let cleanup_deadline = analyzer_cleanup_deadline();
        if let Err(source) = empty_sandbox_directory(&self.directory_file, &mut remaining_entries, 0, cleanup_deadline)
        {
            errors.push(format!("empty analyzer directory through pinned descriptor: {source}"));
        }
        if let Err(source) = self.directory_file.sync_all() {
            errors.push(format!("sync analyzer directory after unlink: {source}"));
        }

        let attached = open_sandbox_directory(&self.parent_file, &self.directory_name)
            .ok()
            .and_then(|directory| directory.metadata().ok())
            .map(|metadata| SandboxSnapshot::from_metadata(&metadata))
            .is_some_and(|snapshot| snapshot.same_node(self.expected_directory));
        let retained = directory.keep();
        if !attached {
            // Never let TempDir recursively remove a replacement installed at
            // its former pathname. Every entry reachable from the pinned
            // descriptor was removed above. Linux has no race-free rmdir-by-fd,
            // so make the now-empty detached directory an explicit tombstone.
            // SAFETY: directory_file remains a live descriptor.
            if unsafe { nix::libc::fchmod(self.directory_file.as_raw_fd(), 0) } == -1 {
                errors.push(format!(
                    "disable detached analyzer directory: {}",
                    std::io::Error::last_os_error()
                ));
            }
            errors.push(AnalyzerSandboxError::DetachedDirectory { path: retained }.to_string());
        } else {
            // Normal finalization reaches this point only after the complete
            // analyzer boundary has terminated. Remove the one authenticated,
            // empty component through its pinned parent rather than traversing
            // the stored pathname.
            // SAFETY: parent_file and directory_name are live and the final
            // component is removed without following it.
            let removed = unsafe {
                nix::libc::unlinkat(
                    self.parent_file.as_raw_fd(),
                    self.directory_name.as_ptr(),
                    nix::libc::AT_REMOVEDIR,
                )
            };
            if removed == -1 {
                let source = std::io::Error::last_os_error();
                // SAFETY: retain an inaccessible empty directory on failure.
                let _ = unsafe { nix::libc::fchmod(self.directory_file.as_raw_fd(), 0) };
                errors.push(format!("remove exact analyzer directory: {source}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Box::new(AnalyzerSandboxError::Finalization {
                details: errors.join("; "),
            }))
        }
    }
}

impl Drop for ExternalAnalyzerInput {
    fn drop(&mut self) {
        // `finish` consumes the TempDir before returning its report. This is a
        // fail-safe for unwinding or a future caller which forgets to finish;
        // cleanup errors cannot be reported from Drop but pathname-recursive
        // TempDir cleanup must never be allowed to target a replacement.
        let _ = self.cleanup(false);
    }
}

#[derive(Debug, Error)]
pub(super) enum AnalyzerInputError {
    #[error("analyzer input {path} is {size} bytes, exceeding the {limit}-byte limit")]
    TooLarge { path: PathBuf, size: u64, limit: u64 },
    #[error("failed to create anonymous analyzer input for {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to copy verified analyzer input {path}: {source}")]
    Copy {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("verified analyzer input {path} changed length: expected {expected}, copied {actual}")]
    Length { path: PathBuf, expected: u64, actual: u64 },
    #[error("failed to sync anonymous analyzer input for {path}: {source}")]
    Sync {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to make anonymous analyzer input for {path} read-only: {source}")]
    Protect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to seal anonymous analyzer input for {path}: {source}")]
    Seal {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("anonymous analyzer input for {path} lacks required seals {expected:#x}; found {actual:#x}")]
    SealsMissing { path: PathBuf, expected: i32, actual: i32 },
    #[error("failed to rewind anonymous analyzer input for {path}: {source}")]
    Rewind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer input of {size} bytes cannot fit in memory")]
    Allocation { size: u64 },
    #[error("failed to reserve {size} bytes for analyzer input: {detail}")]
    Reserve { size: u64, detail: String },
}
