// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use itertools::Itertools;
use nix::{
    errno::Errno,
    sys::{
        signal::{Signal, kill},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{Pid, getpid},
};
use std::{
    ffi::{CStr, CString, OsStr},
    fmt,
    fs::{File as StdFile, OpenOptions as StdOpenOptions, Permissions},
    io::{Read, Seek, SeekFrom, Write},
    mem::{MaybeUninit, size_of},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::mpsc::{self, RecvTimeoutError, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use stone::relation::{Dependency, Kind, Provider};
use thiserror::Error;

use crate::package::collect::{GeneratedArtifact, GeneratedTimes, PathInfo};

pub use self::elf::elf;
pub use self::python::python;
use super::{BoxError, BucketMut, Decision, Response};

mod elf;
mod python;

const ANALYZER_LIMITS: AnalyzerLimits = AnalyzerLimits {
    // llvm-objcopy and llvm-strip can legitimately process very large debug
    // artefacts. Keep the ceiling finite without turning normal large-package
    // analysis into a race against an interactive-command timeout.
    wall_timeout: Duration::from_secs(5 * 60),
    stdout_bytes: 1024 * 1024,
    stderr_bytes: 1024 * 1024,
};

// These per-process ceilings are deliberately independent of the host's
// ambient limits. They complement the namespace-wide descendant cleanup;
// aggregate PID, memory, CPU, and scratch-space accounting belongs to the
// frozen executor boundary rather than being implied by rlimits here.
const ANALYZER_ADDRESS_SPACE_BYTES: nix::libc::rlim_t = 16 * 1024 * 1024 * 1024;
const ANALYZER_FILE_BYTES: nix::libc::rlim_t = 64 * 1024 * 1024 * 1024;
const ANALYZER_OPEN_FILES: nix::libc::rlim_t = 64;
const PKG_CONFIG_INPUT_BYTES: u64 = 4 * 1024 * 1024;
const SANDBOX_CLEANUP_ENTRY_LIMIT: usize = 65_536;
const SANDBOX_CLEANUP_DEPTH_LIMIT: usize = 256;
const ANALYZER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct AnalyzerLimits {
    wall_timeout: Duration,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

/// Construct an analyzer subprocess with no ambient environment or readable
/// standard input. Analyzer tools are part of frozen execution and must not
/// gain inputs from the process which launched Cast.
pub(super) fn analyzer_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear().stdin(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            set_analyzer_limit(nix::libc::RLIMIT_AS, ANALYZER_ADDRESS_SPACE_BYTES)?;
            set_analyzer_limit(nix::libc::RLIMIT_FSIZE, ANALYZER_FILE_BYTES)?;
            set_analyzer_limit(nix::libc::RLIMIT_NOFILE, ANALYZER_OPEN_FILES)?;
            set_analyzer_limit(nix::libc::RLIMIT_CORE, 0)?;
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
}

fn set_analyzer_limit(resource: nix::libc::__rlimit_resource_t, ceiling: nix::libc::rlim_t) -> std::io::Result<()> {
    let mut inherited = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `inherited` is writable and `resource` is one of the constants
    // supplied by `analyzer_command` above.
    if unsafe { nix::libc::getrlimit(resource, &mut inherited) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let bounded = bounded_analyzer_limit(inherited, ceiling);
    // SAFETY: `bounded` contains a soft limit no larger than its hard limit.
    if unsafe { nix::libc::setrlimit(resource, &bounded) } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn bounded_analyzer_limit(inherited: nix::libc::rlimit, ceiling: nix::libc::rlim_t) -> nix::libc::rlimit {
    let hard = inherited.rlim_max.min(ceiling);
    nix::libc::rlimit {
        // A hardening boundary may lower an inherited allowance, but must
        // never silently raise a deliberately lower ambient soft limit.
        rlim_cur: inherited.rlim_cur.min(hard),
        rlim_max: hard,
    }
}

/// A sealed, descriptor-authenticated copy of a collected regular file.
/// It is the immutable source for private external-tool sandboxes and never
/// exposes the mutable output-tree path.
#[derive(Debug)]
pub(super) struct VerifiedAnalyzerInput {
    file: StdFile,
    size: u64,
}

impl VerifiedAnalyzerInput {
    pub(super) fn from_path_info(info: &PathInfo, byte_limit: u64) -> Result<Self, BoxError> {
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

    pub(super) fn try_clone(&self) -> Result<StdFile, BoxError> {
        let mut file = self.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }

    pub(super) fn read_all(&self, byte_limit: usize) -> Result<Vec<u8>, BoxError> {
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
pub(super) struct ExternalAnalyzerInput {
    directory: Option<tempfile::TempDir>,
    parent_file: StdFile,
    directory_name: CString,
    directory_file: StdFile,
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
pub(super) struct ExternalAnalyzerMutation(ExternalAnalyzerInput);

impl ExternalAnalyzerMutation {
    pub(super) fn new(
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

    pub(super) fn path(&self) -> &Path {
        self.0.path()
    }

    pub(super) fn output_path(&self, file_name: &str) -> Result<PathBuf, BoxError> {
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
    pub(super) fn finish(
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

    pub(super) fn finish_with_output(
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
    pub(super) fn new(
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

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn working_directory(&self) -> &Path {
        self.directory.as_ref().expect("live analyzer sandbox").path()
    }

    /// Verify and clean the sandbox regardless of the analyzer result. A
    /// cleanup failure is never hidden behind an earlier analyzer failure.
    pub(super) fn finish<T>(mut self, info: &PathInfo, operation: Result<T, BoxError>) -> Result<T, BoxError> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SandboxSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl SandboxSnapshot {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn same_node(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

fn open_sandbox_file(directory: &StdFile, name: &CStr, path: &Path) -> Result<StdFile, BoxError> {
    // SAFETY: directory and name are live; O_NOFOLLOW rejects a substituted
    // symlink and O_NONBLOCK prevents a substituted FIFO from blocking.
    let descriptor = unsafe {
        nix::libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOCTTY,
            0,
        )
    };
    if descriptor == -1 {
        Err(Box::new(AnalyzerSandboxError::OpenFile {
            path: path.to_owned(),
            source: std::io::Error::last_os_error(),
        }))
    } else {
        // SAFETY: openat returned a fresh owned descriptor.
        Ok(unsafe { StdFile::from_raw_fd(descriptor) })
    }
}

fn digest_sandbox_file(mut file: StdFile, expected_size: u64, path: &Path) -> Result<[u8; 32], BoxError> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| AnalyzerSandboxError::Length {
                path: path.to_owned(),
                expected: expected_size,
                actual: u64::MAX,
            })?;
        if bytes > expected_size {
            return Err(Box::new(AnalyzerSandboxError::Length {
                path: path.to_owned(),
                expected: expected_size,
                actual: bytes,
            }));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected_size {
        return Err(Box::new(AnalyzerSandboxError::Length {
            path: path.to_owned(),
            expected: expected_size,
            actual: bytes,
        }));
    }
    Ok(hasher.finalize().into())
}

fn require_sandbox_snapshot(
    path: &Path,
    expected: SandboxSnapshot,
    actual: &std::fs::Metadata,
    subject: &'static str,
) -> Result<(), BoxError> {
    let actual = SandboxSnapshot::from_metadata(actual);
    if actual == expected {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::SnapshotChanged {
            path: path.to_owned(),
            subject,
        }))
    }
}

fn require_sandbox_node(
    path: &Path,
    expected: SandboxSnapshot,
    actual: &std::fs::Metadata,
    subject: &'static str,
) -> Result<(), BoxError> {
    if expected.same_node(SandboxSnapshot::from_metadata(actual)) {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::SnapshotChanged {
            path: path.to_owned(),
            subject,
        }))
    }
}

fn validate_sandbox_component(file_name: &str) -> Result<(), BoxError> {
    CString::new(file_name).map_err(|_| {
        Box::new(AnalyzerSandboxError::InvalidName {
            name: file_name.to_owned(),
        }) as BoxError
    })?;
    if Path::new(file_name).file_name().and_then(|name| name.to_str()) == Some(file_name) {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InvalidName {
            name: file_name.to_owned(),
        }))
    }
}

fn read_mutated_sandbox_regular(
    info: &PathInfo,
    directory: &StdFile,
    name: &CStr,
    path: &Path,
    byte_limit: u64,
) -> Result<Vec<u8>, BoxError> {
    let mut file = open_sandbox_file(directory, name, path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(Box::new(AnalyzerSandboxError::InvalidMutatedOutput {
            path: path.to_owned(),
            detail: "expected one single-link regular file",
        }));
    }
    let expected = SandboxSnapshot::from_metadata(&metadata);
    if expected.size > byte_limit {
        return Err(Box::new(AnalyzerSandboxError::MutatedOutputTooLarge {
            path: path.to_owned(),
            size: expected.size,
            limit: byte_limit,
        }));
    }
    let capacity = usize::try_from(expected.size).map_err(|_| AnalyzerSandboxError::MutatedOutputAllocation {
        path: path.to_owned(),
        size: expected.size,
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|source| AnalyzerSandboxError::MutatedOutputReserve {
            path: path.to_owned(),
            size: expected.size,
            detail: source.to_string(),
        })?;
    let mut buffer = [0_u8; 64 * 1024];
    while bytes.len() < capacity {
        info.check_deadline()?;
        let allowed = (capacity - bytes.len()).min(buffer.len());
        let read = file.read(&mut buffer[..allowed])?;
        if read == 0 {
            return Err(Box::new(AnalyzerSandboxError::MutatedOutputLength {
                path: path.to_owned(),
                expected: expected.size,
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            }));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let mut probe = [0_u8; 1];
    if file.read(&mut probe)? != 0 {
        return Err(Box::new(AnalyzerSandboxError::MutatedOutputLength {
            path: path.to_owned(),
            expected: expected.size,
            actual: expected.size.saturating_add(1),
        }));
    }
    require_sandbox_snapshot(path, expected, &file.metadata()?, "mutable analyzer output descriptor")?;
    let reopened = open_sandbox_file(directory, name, path)?;
    require_sandbox_snapshot(path, expected, &reopened.metadata()?, "mutable analyzer output path")?;
    Ok(bytes)
}

fn verify_mutated_sandbox_inventory(
    directory: &StdFile,
    display_path: &Path,
    expected_names: &[&CStr],
) -> Result<(), BoxError> {
    let entries = sandbox_directory_entries(directory, expected_names.len() + 1, analyzer_cleanup_deadline()).map_err(
        |source| AnalyzerSandboxError::Inspect {
            path: display_path.to_owned(),
            source,
        },
    )?;
    if entries.len() == expected_names.len()
        && expected_names
            .iter()
            .all(|expected| entries.iter().any(|actual| actual.as_bytes() == expected.to_bytes()))
    {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InventoryChanged {
            path: display_path.to_owned(),
        }))
    }
}

fn verify_sandbox_inventory(
    directory: &StdFile,
    display_path: &Path,
    expected_name: &OsStr,
    expected_snapshot: SandboxSnapshot,
) -> Result<(), BoxError> {
    // Enumerate a duplicate of the pinned descriptor, not the mutable
    // pathname. Checking metadata on both sides turns concurrent directory mutation
    // into a verification failure rather than a clean-path TOCTOU bypass.
    require_sandbox_snapshot(
        display_path,
        expected_snapshot,
        &directory.metadata()?,
        "analyzer sandbox directory before inventory",
    )?;
    let entries = sandbox_directory_entries(directory, 2, analyzer_cleanup_deadline()).map_err(|source| {
        AnalyzerSandboxError::Inspect {
            path: display_path.to_owned(),
            source,
        }
    })?;
    require_sandbox_snapshot(
        display_path,
        expected_snapshot,
        &directory.metadata()?,
        "analyzer sandbox directory after inventory",
    )?;
    if entries.len() == 1 && entries[0].as_bytes() == expected_name.as_bytes() {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InventoryChanged {
            path: display_path.to_owned(),
        }))
    }
}

struct DirectoryStream(*mut nix::libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: fdopendir returned this uniquely owned stream.
        unsafe { nix::libc::closedir(self.0) };
    }
}

fn sandbox_directory_entries(
    directory: &StdFile,
    entry_limit: usize,
    deadline: Instant,
) -> std::io::Result<Vec<CString>> {
    // fdopendir consumes its descriptor, so enumerate through a CLOEXEC
    // duplicate while retaining the authenticated directory descriptor.
    // SAFETY: directory is live and F_DUPFD_CLOEXEC returns a fresh descriptor.
    let descriptor = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: descriptor is a fresh directory descriptor transferred to DIR.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    if stream.is_null() {
        let source = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume descriptor.
        unsafe { nix::libc::close(descriptor) };
        return Err(source);
    }
    let stream = DirectoryStream(stream);
    // dup/fdopendir shares the underlying open-file-description offset with
    // the pinned fd. Always rewind before a fresh exact inventory pass.
    // SAFETY: stream is live and uniquely used by this enumeration.
    unsafe { nix::libc::rewinddir(stream.0) };
    let mut entries = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "sandbox directory enumeration exceeded its cleanup deadline",
            ));
        }
        // readdir uses a null result for both EOF and failure.
        // SAFETY: this project is Linux-only and the errno pointer is valid for
        // the current thread.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live until the DirectoryStream is dropped.
        let entry = unsafe { nix::libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: see the errno reset immediately above.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(errno));
        }
        // SAFETY: POSIX guarantees a NUL-terminated d_name for a live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            if entries.len() == entry_limit {
                return Err(std::io::Error::other(format!(
                    "sandbox directory exceeds its {entry_limit}-entry enumeration limit"
                )));
            }
            entries
                .try_reserve(1)
                .map_err(|source| std::io::Error::other(format!("reserve sandbox directory entry: {source}")))?;
            let source = name.to_bytes_with_nul();
            let mut owned = Vec::new();
            owned
                .try_reserve_exact(source.len())
                .map_err(|error| std::io::Error::other(format!("reserve sandbox entry name: {error}")))?;
            owned.extend_from_slice(source);
            let owned = CString::from_vec_with_nul(owned)
                .map_err(|error| std::io::Error::other(format!("copy sandbox entry name: {error}")))?;
            entries.push(owned);
        }
    }
    Ok(entries)
}

fn sandbox_entry_is_directory(directory: &StdFile, name: &CStr) -> std::io::Result<bool> {
    let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: all pointers are live and AT_SYMLINK_NOFOLLOW authenticates the
    // directory entry itself rather than a possible symlink target.
    let result = unsafe {
        nix::libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstatat initialized metadata on success.
    let metadata = unsafe { metadata.assume_init() };
    Ok(metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR)
}

fn open_sandbox_directory(directory: &StdFile, name: &CStr) -> std::io::Result<StdFile> {
    // Refuse every mount crossing as well as symlink traversal. A hostile
    // analyzer must never turn cleanup into a recursive walk of a foreign bind
    // mount placed below its private directory.
    // SAFETY: an all-zero `open_how` is valid before its public fields are set.
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
    // SAFETY: directory, name, and `how` are live for this descriptor-relative
    // lookup and successful openat2 returns a fresh descriptor.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            directory.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        let descriptor = i32::try_from(result)
            .map_err(|_| std::io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
        // SAFETY: openat returned a fresh owned descriptor.
        Ok(unsafe { StdFile::from_raw_fd(descriptor) })
    }
}

fn empty_sandbox_directory(
    directory: &StdFile,
    remaining_entries: &mut usize,
    depth: usize,
    deadline: Instant,
) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "sandbox cleanup exceeded its deadline",
        ));
    }
    if depth > SANDBOX_CLEANUP_DEPTH_LIMIT {
        return Err(std::io::Error::other(format!(
            "sandbox cleanup exceeded its {SANDBOX_CLEANUP_DEPTH_LIMIT}-directory depth limit"
        )));
    }
    // Multiple finite passes catch entries which were observed during a
    // concurrent rename without allowing hostile churn to make Drop unbounded.
    for _ in 0..8 {
        let entries = sandbox_directory_entries(directory, *remaining_entries, deadline)?;
        if entries.is_empty() {
            return Ok(());
        }
        for name in entries {
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "sandbox cleanup exceeded its deadline",
                ));
            }
            if *remaining_entries == 0 {
                return Err(std::io::Error::other(format!(
                    "sandbox cleanup exceeded its {SANDBOX_CLEANUP_ENTRY_LIMIT}-entry limit"
                )));
            }
            *remaining_entries -= 1;
            let is_directory = match sandbox_entry_is_directory(directory, &name) {
                Ok(value) => value,
                Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => continue,
                Err(source) => return Err(source),
            };
            if is_directory {
                let child = open_sandbox_directory(directory, &name)?;
                // SAFETY: child is the pinned directory just opened above.
                if unsafe { nix::libc::fchmod(child.as_raw_fd(), 0o700) } == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                empty_sandbox_directory(&child, remaining_entries, depth + 1, deadline)?;
                // SAFETY: parent and name are live and the child has been
                // emptied descriptor-relatively without following symlinks.
                let removed =
                    unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) };
                if removed == -1 {
                    let source = std::io::Error::last_os_error();
                    if source.raw_os_error() != Some(nix::libc::ENOENT) {
                        return Err(source);
                    }
                }
            } else {
                // SAFETY: parent and name are live; unlinkat never follows the
                // final component and therefore cannot escape the sandbox.
                let removed = unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
                if removed == -1 {
                    let source = std::io::Error::last_os_error();
                    if source.raw_os_error() != Some(nix::libc::ENOENT) {
                        return Err(source);
                    }
                }
            }
        }
    }
    Err(std::io::Error::other(
        "sandbox directory kept changing during descriptor-rooted cleanup",
    ))
}

fn combine_finalization_errors(
    verification: Result<(), BoxError>,
    cleanup: Result<(), BoxError>,
    deadline: Result<(), BoxError>,
) -> Result<(), BoxError> {
    let mut errors = Vec::new();
    for error in [verification.err(), cleanup.err(), deadline.err()]
        .into_iter()
        .flatten()
    {
        errors.push(error.to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::Finalization {
            details: errors.join("; "),
        }))
    }
}

fn cleanup_unfinished_sandbox(directory: tempfile::TempDir, display_path: &Path) -> Result<(), BoxError> {
    let path = directory.path().to_owned();
    let protect = std::fs::set_permissions(&path, Permissions::from_mode(0o700));
    let close = directory.close();
    match (protect, close) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(source), Ok(())) | (Ok(()), Err(source)) => Err(Box::new(AnalyzerSandboxError::Cleanup {
            path: display_path.to_owned(),
            source,
        })),
        (Err(first), Err(second)) => Err(Box::new(AnalyzerSandboxError::Finalization {
            details: format!("failed to restore sandbox permissions: {first}; failed to remove sandbox: {second}"),
        })),
    }
}

#[derive(Debug, Error)]
#[error("{operation}; analyzer input finalization also failed: {finalization}")]
struct AnalyzerOperationFinalizationError {
    operation: BoxError,
    finalization: BoxError,
}

#[derive(Debug, Error)]
enum AnalyzerSandboxError {
    #[error("invalid analyzer sandbox file name {name:?}")]
    InvalidName { name: String },
    #[error("invalid analyzer sandbox directory path {path}")]
    InvalidDirectoryPath { path: PathBuf },
    #[error("failed to create private analyzer directory for {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open private analyzer directory for {path}: {source}")]
    OpenDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create private analyzer file for {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write private analyzer file for {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("private analyzer file for {path} changed length: expected {expected}, found {actual}")]
    Length { path: PathBuf, expected: u64, actual: u64 },
    #[error("failed to sync private analyzer file for {path}: {source}")]
    SyncFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to protect private analyzer file for {path}: {source}")]
    ProtectFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open private analyzer file for {path}: {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to protect private analyzer directory for {path}: {source}")]
    ProtectDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to sync private analyzer directory for {path}: {source}")]
    SyncDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect private analyzer input {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{subject} changed during analysis at {path}")]
    SnapshotChanged { path: PathBuf, subject: &'static str },
    #[error("private analyzer directory inventory changed at {path}")]
    InventoryChanged { path: PathBuf },
    #[error("private analyzer input content changed at {path}")]
    DigestChanged { path: PathBuf },
    #[error("mutable analyzer output at {path} is invalid: {detail}")]
    InvalidMutatedOutput { path: PathBuf, detail: &'static str },
    #[error("mutable analyzer output at {path} is {size} bytes, exceeding the {limit}-byte limit")]
    MutatedOutputTooLarge { path: PathBuf, size: u64, limit: u64 },
    #[error("mutable analyzer output at {path} of {size} bytes cannot fit in memory")]
    MutatedOutputAllocation { path: PathBuf, size: u64 },
    #[error("failed to reserve {size} bytes for mutable analyzer output at {path}: {detail}")]
    MutatedOutputReserve { path: PathBuf, size: u64, detail: String },
    #[error("mutable analyzer output at {path} changed length: expected {expected}, found {actual}")]
    MutatedOutputLength { path: PathBuf, expected: u64, actual: u64 },
    #[error("private analyzer directory was detached before cleanup: {path}")]
    DetachedDirectory { path: PathBuf },
    #[error("unfinished private analyzer directory retained for namespace teardown: {path}")]
    UnfinishedDirectory { path: PathBuf },
    #[error("failed to clean private analyzer input {path}: {source}")]
    Cleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer input finalization failed: {details}")]
    Finalization { details: String },
}

pub(super) fn checked_output_for(info: &PathInfo, mut command: Command) -> Result<Output, BoxError> {
    info.check_deadline()?;
    let wall_timeout = ANALYZER_LIMITS.wall_timeout.min(info.remaining_time()?);
    let limits = AnalyzerLimits {
        wall_timeout,
        ..ANALYZER_LIMITS
    };
    let output = checked_output_with_limits(&mut command, limits)?;
    info.check_deadline()?;
    Ok(output)
}

#[derive(Debug, Error)]
enum AnalyzerInputError {
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

/// Run one analyzer tool and reject all non-success statuses before consuming
/// any partial stdout. Silently accepting failed analysis would make package
/// relations depend on host/runtime failure state outside the frozen plan.
#[cfg(test)]
pub(super) fn checked_output(mut command: Command) -> Result<Output, BoxError> {
    checked_output_with_limits(&mut command, ANALYZER_LIMITS)
}

fn checked_output_with_limits(command: &mut Command, limits: AnalyzerLimits) -> Result<Output, BoxError> {
    let invocation = format!("{command:?}");
    let output = contained_output(command, analyzer_containment(), limits, &invocation)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Box::new(AnalyzerCommandError {
            invocation,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyzerContainment {
    /// Frozen packaging runs as PID 1. Killing every other process in that
    /// namespace catches process-group changes, `setsid`, and double forks.
    PidNamespace,
    /// Unit tests do not own their PID namespace and use the command's private
    /// process group as a safe behavioral boundary.
    #[cfg(test)]
    ProcessGroup,
}

fn analyzer_containment() -> AnalyzerContainment {
    #[cfg(test)]
    if getpid().as_raw() != 1 {
        return AnalyzerContainment::ProcessGroup;
    }
    AnalyzerContainment::PidNamespace
}

fn contained_output(
    command: &mut Command,
    containment: AnalyzerContainment,
    limits: AnalyzerLimits,
    invocation: &str,
) -> Result<Output, AnalyzerExecutionError> {
    let started = Instant::now();
    if matches!(containment, AnalyzerContainment::PidNamespace) && getpid().as_raw() != 1 {
        return Err(AnalyzerExecutionError::Containment {
            invocation: invocation.to_owned(),
        });
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AnalyzerExecutionError::Spawn {
            invocation: invocation.to_owned(),
            source,
        })?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let (events, received) = mpsc::channel();
    let stdout_reader = match read_analyzer_pipe(
        child.stdout.take().expect("piped analyzer stdout"),
        AnalyzerPipe::Stdout,
        limits.stdout_bytes,
        events.clone(),
    ) {
        Ok(reader) => reader,
        Err(source) => {
            let error = AnalyzerExecutionError::PipeReaderSpawn {
                invocation: invocation.to_owned(),
                pipe: AnalyzerPipe::Stdout,
                source,
            };
            let cleanup = abort_analyzer(&mut child, containment, child_pid, None, false, [], invocation);
            return Err(with_analyzer_cleanup(error, cleanup));
        }
    };
    let stderr_reader = match read_analyzer_pipe(
        child.stderr.take().expect("piped analyzer stderr"),
        AnalyzerPipe::Stderr,
        limits.stderr_bytes,
        events.clone(),
    ) {
        Ok(reader) => reader,
        Err(source) => {
            let error = AnalyzerExecutionError::PipeReaderSpawn {
                invocation: invocation.to_owned(),
                pipe: AnalyzerPipe::Stderr,
                source,
            };
            let cleanup = abort_analyzer(
                &mut child,
                containment,
                child_pid,
                None,
                false,
                [stdout_reader],
                invocation,
            );
            return Err(with_analyzer_cleanup(error, cleanup));
        }
    };
    drop(events);
    let readers = [stdout_reader, stderr_reader];

    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut boundary_terminated = false;

    loop {
        match received.try_recv() {
            Ok(event) => {
                if let Err(error) = accept_analyzer_pipe_event(event, &mut stdout, &mut stderr, invocation) {
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
                continue;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) if stdout.is_some() && stderr.is_some() => {}
            Err(TryRecvError::Disconnected) => {
                let error = AnalyzerExecutionError::PipeChannelClosed {
                    invocation: invocation.to_owned(),
                };
                let cleanup = abort_analyzer(
                    &mut child,
                    containment,
                    child_pid,
                    status,
                    boundary_terminated,
                    readers,
                    invocation,
                );
                return Err(with_analyzer_cleanup(error, cleanup));
            }
        }

        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    status = Some(exit_status);
                    if let Err(termination) = terminate_analyzer_boundary(
                        containment,
                        child_pid,
                        &mut child,
                        status,
                        analyzer_cleanup_deadline(),
                    ) {
                        let operation = AnalyzerExecutionError::Cleanup {
                            invocation: invocation.to_owned(),
                            source: termination,
                        };
                        let readers =
                            join_analyzer_pipe_readers_until(readers, invocation, analyzer_cleanup_deadline());
                        return Err(with_analyzer_cleanup(operation, readers));
                    }
                    boundary_terminated = true;
                }
                Ok(None) => {}
                Err(source) => {
                    let error = AnalyzerExecutionError::Monitor {
                        invocation: invocation.to_owned(),
                        source,
                    };
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
            }
        }

        if let Some(exit_status) = status
            && stdout.is_some()
            && stderr.is_some()
        {
            join_analyzer_pipe_readers_until(readers, invocation, analyzer_cleanup_deadline())?;
            return Ok(Output {
                status: exit_status,
                stdout: stdout.take().expect("checked analyzer stdout"),
                stderr: stderr.take().expect("checked analyzer stderr"),
            });
        }

        let elapsed = started.elapsed();
        if elapsed >= limits.wall_timeout {
            let error = AnalyzerExecutionError::Timeout {
                invocation: invocation.to_owned(),
                timeout: limits.wall_timeout,
            };
            let cleanup = abort_analyzer(
                &mut child,
                containment,
                child_pid,
                status,
                boundary_terminated,
                readers,
                invocation,
            );
            return Err(with_analyzer_cleanup(error, cleanup));
        }

        let remaining = limits.wall_timeout.saturating_sub(elapsed);
        match received.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(event) => {
                if let Err(error) = accept_analyzer_pipe_event(event, &mut stdout, &mut stderr, invocation) {
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if stdout.is_some() && stderr.is_some() => {}
            Err(RecvTimeoutError::Disconnected) => {
                let error = AnalyzerExecutionError::PipeChannelClosed {
                    invocation: invocation.to_owned(),
                };
                let cleanup = abort_analyzer(
                    &mut child,
                    containment,
                    child_pid,
                    status,
                    boundary_terminated,
                    readers,
                    invocation,
                );
                return Err(with_analyzer_cleanup(error, cleanup));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyzerPipe {
    Stdout,
    Stderr,
}

impl fmt::Display for AnalyzerPipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => formatter.write_str("stdout"),
            Self::Stderr => formatter.write_str("stderr"),
        }
    }
}

enum AnalyzerPipeEvent {
    Complete { pipe: AnalyzerPipe, bytes: Vec<u8> },
    LimitExceeded { pipe: AnalyzerPipe, limit: usize },
    ReadFailed { pipe: AnalyzerPipe, source: std::io::Error },
}

fn read_analyzer_pipe<R>(
    mut pipe: R,
    name: AnalyzerPipe,
    limit: usize,
    events: mpsc::Sender<AnalyzerPipeEvent>,
) -> std::io::Result<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("mason-analyzer-{name}"))
        .spawn(move || {
            let mut bytes = Vec::with_capacity(limit.min(8192));
            let mut buffer = [0_u8; 8192];
            let event = loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break AnalyzerPipeEvent::Complete { pipe: name, bytes },
                    Ok(read) if read > limit.saturating_sub(bytes.len()) => {
                        break AnalyzerPipeEvent::LimitExceeded { pipe: name, limit };
                    }
                    Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                    Err(source) => break AnalyzerPipeEvent::ReadFailed { pipe: name, source },
                }
            };
            let _ = events.send(event);
        })
}

fn accept_analyzer_pipe_event(
    event: AnalyzerPipeEvent,
    stdout: &mut Option<Vec<u8>>,
    stderr: &mut Option<Vec<u8>>,
    invocation: &str,
) -> Result<(), AnalyzerExecutionError> {
    match event {
        AnalyzerPipeEvent::Complete { pipe, bytes } => {
            let destination = match pipe {
                AnalyzerPipe::Stdout => stdout,
                AnalyzerPipe::Stderr => stderr,
            };
            if destination.replace(bytes).is_some() {
                return Err(AnalyzerExecutionError::DuplicatePipeResult {
                    invocation: invocation.to_owned(),
                    pipe,
                });
            }
            Ok(())
        }
        AnalyzerPipeEvent::LimitExceeded { pipe, limit } => Err(AnalyzerExecutionError::OutputLimit {
            invocation: invocation.to_owned(),
            pipe,
            limit,
        }),
        AnalyzerPipeEvent::ReadFailed { pipe, source } => Err(AnalyzerExecutionError::PipeRead {
            invocation: invocation.to_owned(),
            pipe,
            source,
        }),
    }
}

fn abort_analyzer<const N: usize>(
    child: &mut Child,
    containment: AnalyzerContainment,
    child_pid: Pid,
    status: Option<ExitStatus>,
    boundary_terminated: bool,
    readers: [JoinHandle<()>; N],
    invocation: &str,
) -> Result<(), AnalyzerExecutionError> {
    let deadline = analyzer_cleanup_deadline();
    let termination = if boundary_terminated {
        Ok(())
    } else {
        terminate_analyzer_boundary(containment, child_pid, child, status, deadline).map_err(|source| {
            AnalyzerExecutionError::Cleanup {
                invocation: invocation.to_owned(),
                source,
            }
        })
    };
    // Even when signalling/reaping fails, still attempt to join readers. This
    // preserves both failures instead of abandoning detached reader threads at
    // the first cleanup error.
    let reader_cleanup = join_analyzer_pipe_readers_until(readers, invocation, deadline);
    combine_analyzer_cleanup(termination, reader_cleanup)
}

fn analyzer_cleanup_deadline() -> Instant {
    Instant::now()
        .checked_add(ANALYZER_CLEANUP_TIMEOUT)
        .unwrap_or_else(Instant::now)
}

fn join_analyzer_pipe_readers_until<const N: usize>(
    readers: [JoinHandle<()>; N],
    invocation: &str,
    deadline: Instant,
) -> Result<(), AnalyzerExecutionError> {
    while !readers.iter().all(JoinHandle::is_finished) {
        let now = Instant::now();
        if now >= deadline {
            return Err(AnalyzerExecutionError::ReaderCleanupTimeout {
                invocation: invocation.to_owned(),
                timeout: ANALYZER_CLEANUP_TIMEOUT,
            });
        }
        thread::sleep(deadline.saturating_duration_since(now).min(Duration::from_millis(2)));
    }
    for reader in readers {
        reader.join().map_err(|_| AnalyzerExecutionError::PipeReaderPanicked {
            invocation: invocation.to_owned(),
        })?;
    }
    Ok(())
}

fn combine_analyzer_cleanup(
    first: Result<(), AnalyzerExecutionError>,
    second: Result<(), AnalyzerExecutionError>,
) -> Result<(), AnalyzerExecutionError> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(AnalyzerExecutionError::MultipleCleanup {
            first: Box::new(first),
            second: Box::new(second),
        }),
    }
}

fn with_analyzer_cleanup(
    operation: AnalyzerExecutionError,
    cleanup: Result<(), AnalyzerExecutionError>,
) -> AnalyzerExecutionError {
    match cleanup {
        Ok(()) => operation,
        Err(cleanup) => AnalyzerExecutionError::OperationCleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    }
}

fn terminate_analyzer_boundary(
    containment: AnalyzerContainment,
    child_pid: Pid,
    child: &mut Child,
    status: Option<ExitStatus>,
    deadline: Instant,
) -> std::io::Result<()> {
    let mut errors = Vec::new();
    let target = analyzer_descendant_signal_target(containment, child_pid);
    if matches!(containment, AnalyzerContainment::PidNamespace) && getpid().as_raw() != 1 {
        errors.push("refusing namespace-wide analyzer cleanup outside PID 1".to_owned());
    } else if let Err(error) = kill(target, Signal::SIGKILL)
        && error != Errno::ESRCH
    {
        errors.push(format!("signal analyzer boundary: {error}"));
    }

    // Reap the direct child through Child first. Never use blocking wait here:
    // SIGKILL can be delayed by an uninterruptible kernel wait, and analyzer
    // finalization must remain bounded even in that case.
    let mut direct_status = status;
    if direct_status.is_none() {
        match child.try_wait() {
            Ok(Some(exit_status)) => direct_status = Some(exit_status),
            Ok(None) => {
                if let Err(source) = child.kill() {
                    errors.push(format!("signal direct analyzer child: {source}"));
                }
            }
            Err(source) => errors.push(format!("inspect direct analyzer child: {source}")),
        }
    }
    while direct_status.is_none() {
        match child.try_wait() {
            Ok(Some(exit_status)) => direct_status = Some(exit_status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    errors.push("timed out reaping direct analyzer child".to_owned());
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(source) => {
                errors.push(format!("reap direct analyzer child: {source}"));
                break;
            }
        }
    }

    if matches!(containment, AnalyzerContainment::PidNamespace) {
        loop {
            if Instant::now() >= deadline {
                errors.push("timed out reaping analyzer descendants".to_owned());
                break;
            }
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {}
                Ok(
                    WaitStatus::Stopped(..)
                    | WaitStatus::PtraceEvent(..)
                    | WaitStatus::PtraceSyscall(..)
                    | WaitStatus::Continued(..)
                    | WaitStatus::StillAlive,
                ) => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(Errno::EINTR) => {}
                Err(Errno::ECHILD) => break,
                Err(error) => {
                    errors.push(format!("reap analyzer descendants: {error}"));
                    break;
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(errors.join("; ")))
    }
}

fn analyzer_descendant_signal_target(containment: AnalyzerContainment, _child_pid: Pid) -> Pid {
    match containment {
        AnalyzerContainment::PidNamespace => Pid::from_raw(-1),
        #[cfg(test)]
        AnalyzerContainment::ProcessGroup => Pid::from_raw(-_child_pid.as_raw()),
    }
}

#[derive(Debug, Error)]
#[error("analyzer command {invocation} failed with {status}: {stderr}")]
struct AnalyzerCommandError {
    invocation: String,
    status: ExitStatus,
    stderr: String,
}

#[derive(Debug, Error)]
enum AnalyzerExecutionError {
    #[error("refusing to start analyzer command {invocation} outside the required PID-1 namespace boundary")]
    Containment { invocation: String },
    #[error("failed to start analyzer command {invocation}: {source}")]
    Spawn {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} exceeded its {timeout:?} wall timeout")]
    Timeout { invocation: String, timeout: Duration },
    #[error("analyzer command {invocation} exceeded its {pipe} limit of {limit} bytes")]
    OutputLimit {
        invocation: String,
        pipe: AnalyzerPipe,
        limit: usize,
    },
    #[error("failed to read {pipe} from analyzer command {invocation}: {source}")]
    PipeRead {
        invocation: String,
        pipe: AnalyzerPipe,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to monitor analyzer command {invocation}: {source}")]
    Monitor {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to terminate and reap analyzer command {invocation}: {source}")]
    Cleanup {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} reported {pipe} more than once")]
    DuplicatePipeResult { invocation: String, pipe: AnalyzerPipe },
    #[error("analyzer command {invocation} closed its pipe result channel early")]
    PipeChannelClosed { invocation: String },
    #[error("analyzer command {invocation} pipe reader panicked")]
    PipeReaderPanicked { invocation: String },
    #[error("failed to start analyzer command {invocation} {pipe} reader: {source}")]
    PipeReaderSpawn {
        invocation: String,
        pipe: AnalyzerPipe,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} pipe readers did not stop within the {timeout:?} cleanup timeout")]
    ReaderCleanupTimeout { invocation: String, timeout: Duration },
    #[error("multiple analyzer cleanup steps failed: {first}; {second}")]
    MultipleCleanup {
        first: Box<AnalyzerExecutionError>,
        second: Box<AnalyzerExecutionError>,
    },
    #[error("{operation}; analyzer cleanup also failed: {cleanup}")]
    OperationCleanup {
        operation: Box<AnalyzerExecutionError>,
        cleanup: Box<AnalyzerExecutionError>,
    },
}

pub fn include_any(_bucket: &mut BucketMut<'_>, _info: &mut PathInfo) -> Result<Response, BoxError> {
    Ok(Decision::IncludeFile.into())
}

pub fn ignore_blocked(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    // non-/usr = bad
    if !info.target_path.starts_with("/usr") {
        return Ok(Decision::IgnoreFile {
            reason: "non /usr/ file".into(),
        }
        .into());
    }

    // libtool files break the world but very rarely a package will need them to function correctly
    if info.file_name().ends_with(".la")
        && (info.target_path.starts_with("/usr/lib") || info.target_path.starts_with("/usr/lib32"))
        && bucket.analysis.remove_libtool
    {
        return Ok(Decision::IgnoreFile {
            reason: "libtool file".into(),
        }
        .into());
    }

    Ok(Decision::NextHandler.into())
}

pub fn binary(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    if info.target_path.starts_with("/usr/bin") {
        let provider = Provider {
            kind: Kind::Binary,
            name: info.file_name().to_owned(),
        };
        bucket.providers.insert(provider);
    } else if info.target_path.starts_with("/usr/sbin") {
        let provider = Provider {
            kind: Kind::SystemBinary,
            name: info.file_name().to_owned(),
        };
        bucket.providers.insert(provider);
    }

    Ok(Decision::NextHandler.into())
}

pub fn pkg_config(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_name = info.file_name();

    if !info.has_component("pkgconfig") || !file_name.ends_with(".pc") {
        return Ok(Decision::NextHandler.into());
    }

    let provider_name = file_name.strip_suffix(".pc").expect("extension exists");
    let emul32 = info.has_component("lib32");
    validate_pkg_config_module(provider_name)?;
    let provider = Provider::new(if emul32 { Kind::PkgConfig32 } else { Kind::PkgConfig }, provider_name)?;
    let logical_pcfiledir = info
        .target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| AnalyzerRelationError::MissingParent {
            path: info.target_path.clone(),
        })?;

    let program = &bucket
        .analysis
        .tools
        .pkg_config
        .as_ref()
        .expect("validated analysis plan requires pkg-config for the pkg-config handler")
        .path;
    let input = VerifiedAnalyzerInput::from_path_info(info, PKG_CONFIG_INPUT_BYTES)?;
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig")?;
    let operation = (|| {
        let mut command = analyzer_command(program);
        command
            .current_dir(sandbox.working_directory())
            .args(["--print-requires", "--print-requires-private", "--silence-errors"])
            // pkg-config otherwise recomputes this built-in from the random
            // sandbox path. Preserve the logical installed location so
            // `${pcfiledir}` stays deterministic and package-correct.
            .arg(format!("--define-variable=pcfiledir={}", logical_pcfiledir.display()))
            .arg(sandbox.path())
            .envs([
                ("LC_ALL", "C"),
                ("PKG_CONFIG_PATH", ""),
                (
                    "PKG_CONFIG_LIBDIR",
                    if emul32 {
                        "/usr/lib32/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
                    } else {
                        "/usr/lib/pkgconfig:/usr/share/pkgconfig"
                    },
                ),
                ("PKG_CONFIG_DISABLE_UNINSTALLED", "1"),
            ]);
        let output = checked_output_for(info, command)?;
        let stdout = String::from_utf8(output.stdout)?;
        parse_pkg_config_dependencies(info, &stdout)
    })();
    let dependency_names = sandbox.finish(info, operation)?;

    let mut dependencies = Vec::new();
    dependencies.try_reserve(dependency_names.len())?;
    for name in dependency_names {
        info.check_deadline()?;
        let kind = pkg_config_dependency_kind(info, logical_pcfiledir, &name, emul32)?;
        dependencies.push(Dependency::new(kind, name)?);
    }
    info.check_deadline()?;

    // Commit relation changes only after the authenticated input, child exit,
    // sandbox verification/cleanup, inventory queries, canonical relation
    // validation, and shared deadline all succeed.
    bucket.providers.insert(provider);
    bucket.dependencies.extend(dependencies);

    Ok(Decision::NextHandler.into())
}

fn pkg_config_dependency_kind(
    info: &PathInfo,
    logical_pcfiledir: &Path,
    name: &str,
    emul32: bool,
) -> Result<Kind, BoxError> {
    if !emul32 {
        return Ok(Kind::PkgConfig);
    }
    let sibling = logical_pcfiledir.join(format!("{name}.pc"));
    let canonical = Path::new("/usr/lib32/pkgconfig").join(format!("{name}.pc"));
    if info.inventory_contains_regular_target(&sibling)?
        || (sibling != canonical && info.inventory_contains_regular_target(&canonical)?)
        || frozen_root_contains_regular_target(&canonical)?
    {
        Ok(Kind::PkgConfig32)
    } else {
        Ok(Kind::PkgConfig)
    }
}

fn parse_pkg_config_dependencies(info: &PathInfo, stdout: &str) -> Result<Vec<String>, BoxError> {
    let mut dependencies = Vec::new();
    dependencies.try_reserve(stdout.lines().count())?;
    for line in stdout.lines() {
        info.check_deadline()?;
        let name = parse_pkg_config_dependency_line(line)?;
        // Route every analyzer-derived name through Stone's canonical
        // relation validator before assigning its architecture-specific kind.
        Dependency::new(Kind::PkgConfig, name)?;
        dependencies.push(name.to_owned());
    }
    info.check_deadline()?;
    Ok(dependencies)
}

fn parse_pkg_config_dependency_line(line: &str) -> Result<&str, AnalyzerRelationError> {
    let mut fields = line.split_whitespace();
    let Some(name) = fields.next() else {
        return Err(AnalyzerRelationError::InvalidPkgConfigDependencyLine { line: line.to_owned() });
    };
    validate_pkg_config_module(name)?;

    match (fields.next(), fields.next(), fields.next()) {
        (None, None, None) => Ok(name),
        (Some(operator), Some(version), None)
            if matches!(operator, "=" | "!=" | "<" | ">" | "<=" | ">=") && !version.is_empty() =>
        {
            Ok(name)
        }
        _ => Err(AnalyzerRelationError::InvalidPkgConfigDependencyLine { line: line.to_owned() }),
    }
}

fn frozen_root_contains_regular_target(target: &Path) -> Result<bool, BoxError> {
    let root = open_frozen_root_anchor(Path::new("/"))?;
    Ok(descriptor_root_contains_regular_target(&root, target)?)
}

fn open_frozen_root_anchor(root: &Path) -> Result<StdFile, FrozenRootLookupError> {
    StdOpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root)
        .map_err(|source| FrozenRootLookupError::OpenRoot {
            path: root.to_owned(),
            source,
        })
}

fn descriptor_root_contains_regular_target(root: &StdFile, target: &Path) -> Result<bool, FrozenRootLookupError> {
    let relative = target
        .strip_prefix("/")
        .map_err(|_| FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        });
    }
    let relative_c =
        CString::new(relative.as_os_str().as_bytes()).map_err(|_| FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        })?;
    // SAFETY: an all-zero `open_how` is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from((nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW) as u32);
    how.resolve = nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: root and relative_c remain live for the syscall and `how`
    // describes a descriptor-only lookup beneath the held root.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            root.as_raw_fd(),
            relative_c.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        let source = std::io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::ENOENT) | Some(nix::libc::ENOTDIR) | Some(nix::libc::ELOOP)
        ) {
            return Ok(false);
        }
        return Err(FrozenRootLookupError::OpenTarget {
            path: target.to_owned(),
            source,
        });
    }
    let descriptor = i32::try_from(result).map_err(|_| FrozenRootLookupError::InvalidDescriptor {
        path: target.to_owned(),
        descriptor: result,
    })?;
    // SAFETY: successful openat2 returned a fresh descriptor owned here.
    let file = unsafe { StdFile::from_raw_fd(descriptor) };
    let metadata = file.metadata().map_err(|source| FrozenRootLookupError::InspectTarget {
        path: target.to_owned(),
        source,
    })?;
    Ok(metadata.file_type().is_file())
}

fn validate_pkg_config_module(name: &str) -> Result<(), AnalyzerRelationError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        Ok(())
    } else {
        Err(AnalyzerRelationError::InvalidPkgConfigName { name: name.to_owned() })
    }
}

#[derive(Debug, Error)]
enum AnalyzerRelationError {
    #[error("analyzer input {path} has no logical parent directory")]
    MissingParent { path: PathBuf },
    #[error("pkg-config emitted invalid module name {name:?}")]
    InvalidPkgConfigName { name: String },
    #[error("pkg-config emitted malformed dependency line {line:?}")]
    InvalidPkgConfigDependencyLine { line: String },
}

#[derive(Debug, Error)]
enum FrozenRootLookupError {
    #[error("failed to open frozen root {path}: {source}")]
    OpenRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid descriptor-anchored frozen-root target {path}")]
    InvalidTarget { path: PathBuf },
    #[error("failed to open frozen-root target {path}: {source}")]
    OpenTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("frozen-root target {path} returned invalid descriptor {descriptor}")]
    InvalidDescriptor { path: PathBuf, descriptor: i64 },
    #[error("failed to inspect frozen-root target {path}: {source}")]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn cmake(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_name = info.file_name();

    if (!file_name.ends_with("Config.cmake") && !file_name.ends_with("-config.cmake"))
        || file_name.ends_with("-Config.cmake")
    {
        return Ok(Decision::NextHandler.into());
    }

    let provider_name = file_name
        .strip_suffix("Config.cmake")
        .or_else(|| file_name.strip_suffix("-config.cmake"))
        .expect("extension exists");

    bucket.providers.insert(Provider {
        kind: Kind::CMake,
        name: provider_name.to_owned(),
    });

    Ok(Decision::NextHandler.into())
}

/// Ensure that man and info files are zst compressed for on-disk space savings.
pub fn compressman(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    /* if the compressman option is turned off, exit early */
    if !bucket.analysis.compress_man {
        return Ok(Decision::NextHandler.into());
    }

    let is_man_file = info.path.components().contains(&Component::Normal("man".as_ref()))
        && info.file_name().ends_with(|c| ('1'..'9').contains(&c));
    let is_info_file =
        info.path.components().contains(&Component::Normal("info".as_ref())) && info.file_name().ends_with(".info");

    /* we only care about compressing man and info files here */
    if !(is_man_file || is_info_file) {
        return Ok(Decision::NextHandler.into());
    }

    let destination = info.target_path.strip_prefix("/")?.with_added_extension("zst");
    let target = Path::new("/").join(&destination);
    let newpath = info.path.with_added_extension("zst");
    let (accessed, modified) = info.file_times()?;
    let times = Some(GeneratedTimes { accessed, modified });

    let artifact = if info.is_symlink() {
        if info.inventory_contains_symlink_target(&target)? {
            None
        } else {
            let compressed_target = Path::new(info.symlink_target()?).with_added_extension("zst");
            let compressed_target = compressed_target
                .to_str()
                .ok_or_else(|| std::io::Error::other("compressed man symlink target is not UTF-8"))?
                .to_owned();
            Some(GeneratedArtifact::symlink(destination, compressed_target, times, false))
        }
    } else if info.is_file() {
        if info.inventory_contains_regular_target(&target)? {
            None
        } else {
            Some(GeneratedArtifact::regular(
                destination,
                compress_zstd(info)?,
                0o644,
                times,
                false,
            ))
        }
    } else {
        return Ok(Decision::NextHandler.into());
    };

    Ok(Response {
        decision: Decision::ReplaceFile { newpath },
        publications: artifact.into_iter().collect(),
    })
}

fn compress_zstd(info: &PathInfo) -> Result<Vec<u8>, BoxError> {
    let limit = usize::try_from(info.regular_file_byte_limit()?)
        .map_err(|_| std::io::Error::other("regular file byte limit does not fit in memory"))?;
    let mut input = info.open_verified()?;
    let mut output = BoundedBytes::new(limit);
    zstd::stream::copy_encode(&mut input, &mut output, 16)?;
    input.finish()?;
    Ok(output.into_inner())
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let total =
            self.bytes.len().checked_add(buffer.len()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "compressed output length overflow")
            })?;
        if total > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed output exceeds the collected regular-file limit",
            ));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|source| std::io::Error::other(format!("failed to reserve compressed output: {source}")))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        io::Write as _,
        os::fd::AsRawFd,
        os::unix::fs::symlink,
        path::Path,
        time::{Duration, Instant},
    };

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    use nix::{sys::stat::Mode, unistd::mkfifo};
    use stone::StoneDigestWriterHasher;
    use stone_recipe::derivation::{ExecutablePlan, PathRuleKind, RelationKind, RelationPlan};

    use super::*;
    use crate::{
        Paths, Recipe,
        package::{collect::Collector, test_derivation_plan},
    };

    fn collect_path(root: &Path, path: &Path) -> PathInfo {
        let mut collector = Collector::new(root);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        collector.path(path, &mut hasher).unwrap()
    }

    fn pkg_config_program() -> PathBuf {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join("pkg-config"))
            .find(|path| path.is_file())
            .expect("focused analyzer tests require pkg-config")
    }

    fn test_limits(stdout_bytes: usize, stderr_bytes: usize, wall_timeout: Duration) -> AnalyzerLimits {
        AnalyzerLimits {
            wall_timeout,
            stdout_bytes,
            stderr_bytes,
        }
    }

    fn checked_test_output(mut command: Command, limits: AnalyzerLimits) -> Result<Output, BoxError> {
        checked_output_with_limits(&mut command, limits)
    }

    fn execution_error(error: &BoxError) -> &AnalyzerExecutionError {
        error
            .downcast_ref::<AnalyzerExecutionError>()
            .expect("expected a structured analyzer execution error")
    }

    #[test]
    fn analyzer_limit_never_raises_an_inherited_soft_limit() {
        let inherited = nix::libc::rlimit {
            rlim_cur: 16,
            rlim_max: 128,
        };
        assert_eq!(
            bounded_analyzer_limit(inherited, 64),
            nix::libc::rlimit {
                rlim_cur: 16,
                rlim_max: 64,
            }
        );

        let inherited = nix::libc::rlimit {
            rlim_cur: 96,
            rlim_max: 128,
        };
        assert_eq!(
            bounded_analyzer_limit(inherited, 64),
            nix::libc::rlimit {
                rlim_cur: 64,
                rlim_max: 64,
            }
        );

        let inherited = nix::libc::rlimit {
            rlim_cur: 24,
            rlim_max: 48,
        };
        assert_eq!(bounded_analyzer_limit(inherited, 64), inherited);
    }

    #[test]
    fn verified_input_accepts_exact_limit_and_rejects_one_byte_over() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"12345678").unwrap();
        let info = collect_path(root.path(), &path);

        let input = VerifiedAnalyzerInput::from_path_info(&info, 8).unwrap();
        assert_eq!(input.read_all(8).unwrap(), b"12345678");

        let over_root = tempfile::tempdir().unwrap();
        let over_path = over_root.path().join("input.pc");
        fs::write(&over_path, b"123456789").unwrap();
        let over = collect_path(over_root.path(), &over_path);
        let error = VerifiedAnalyzerInput::from_path_info(&over, 8).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<AnalyzerInputError>(),
            Some(AnalyzerInputError::TooLarge { size: 9, limit: 8, .. })
        ));
    }

    #[test]
    fn verified_input_is_sealed_against_write_truncate_and_growth() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"immutable").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 9).unwrap();
        let required =
            nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
        // SAFETY: input owns this live memfd.
        let seals = unsafe { nix::libc::fcntl(input.file.as_raw_fd(), nix::libc::F_GET_SEALS) };
        assert_eq!(seals & required, required);

        let mut read_only = input.try_clone().unwrap();
        assert!(read_only.write_all(b"x").is_err());
        assert!(read_only.set_len(0).is_err());
        let byte = b'x';
        // SAFETY: the pointer and descriptor are live; the seal must reject
        // this write rather than altering the authenticated bytes.
        assert_eq!(
            unsafe { nix::libc::pwrite(input.file.as_raw_fd(), (&byte as *const u8).cast(), 1, 0) },
            -1
        );
        // SAFETY: input owns this live memfd.
        assert_eq!(unsafe { nix::libc::ftruncate(input.file.as_raw_fd(), 0) }, -1);
        // SAFETY: input owns this live memfd.
        assert_eq!(unsafe { nix::libc::ftruncate(input.file.as_raw_fd(), 10) }, -1);

        assert_eq!(input.read_all(9).unwrap(), b"immutable");
    }

    #[test]
    fn private_regular_file_sandbox_is_available_to_child_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"child-visible").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
        let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
        let sandbox_path = sandbox.working_directory().to_owned();
        let mut command = analyzer_command("/bin/cat");
        command.current_dir(sandbox.working_directory()).arg(sandbox.path());

        let operation = checked_output_for(&info, command);
        let output = sandbox.finish(&info, operation).unwrap();
        assert_eq!(output.stdout, b"child-visible");
        assert!(!sandbox_path.exists());
    }

    #[test]
    fn changed_or_expanded_sandbox_is_rejected_then_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"authenticated").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
        let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
        let sandbox_path = sandbox.working_directory().to_owned();
        let mut command = analyzer_command("/bin/sh");
        command.current_dir(sandbox.working_directory()).args([
            "-c",
            "chmod 700 .; chmod 600 input.pc; printf corrupted > input.pc; : > extra",
        ]);

        let operation = checked_output_for(&info, command);
        assert!(sandbox.finish(&info, operation).is_err());
        assert!(!sandbox_path.exists());
        assert_eq!(fs::read(path).unwrap(), b"authenticated");
    }

    #[test]
    fn sandbox_inventory_accepts_exact_entry_limit_and_rejects_one_over() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("first"), b"one").unwrap();
        let directory = StdOpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(root.path())
            .unwrap();

        assert_eq!(
            sandbox_directory_entries(&directory, 1, analyzer_cleanup_deadline())
                .unwrap()
                .len(),
            1
        );
        fs::write(root.path().join("second"), b"two").unwrap();
        let error = sandbox_directory_entries(&directory, 1, analyzer_cleanup_deadline()).unwrap_err();
        assert!(error.to_string().contains("1-entry enumeration limit"), "{error}");
    }

    #[test]
    fn sandbox_cleanup_accepts_exact_depth_and_rejects_one_over() {
        fn nested(root: &Path, depth: usize) {
            let mut current = root.to_owned();
            for _ in 0..depth {
                current.push("d");
                fs::create_dir(&current).unwrap();
            }
        }

        let exact = tempfile::tempdir().unwrap();
        nested(exact.path(), SANDBOX_CLEANUP_DEPTH_LIMIT);
        let exact_directory = StdOpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(exact.path())
            .unwrap();
        let mut remaining = SANDBOX_CLEANUP_ENTRY_LIMIT;
        empty_sandbox_directory(&exact_directory, &mut remaining, 0, analyzer_cleanup_deadline()).unwrap();
        assert_eq!(fs::read_dir(exact.path()).unwrap().count(), 0);

        let over = tempfile::tempdir().unwrap();
        nested(over.path(), SANDBOX_CLEANUP_DEPTH_LIMIT + 1);
        let over_directory = StdOpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(over.path())
            .unwrap();
        let mut remaining = SANDBOX_CLEANUP_ENTRY_LIMIT;
        let error =
            empty_sandbox_directory(&over_directory, &mut remaining, 0, analyzer_cleanup_deadline()).unwrap_err();
        assert!(error.to_string().contains("depth limit"), "{error}");
    }

    #[test]
    fn unfinished_detached_sandbox_is_retained_without_deleting_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"authenticated").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
        let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
        let original_path = sandbox.working_directory().to_owned();
        let detached_path = original_path.with_file_name(format!(
            "{}.detached",
            original_path.file_name().unwrap().to_string_lossy()
        ));

        fs::rename(&original_path, &detached_path).unwrap();
        fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
        fs::rename(detached_path.join("input.pc"), detached_path.join("renamed-input")).unwrap();
        fs::create_dir(detached_path.join("nested")).unwrap();
        fs::write(detached_path.join("nested/payload"), b"extra").unwrap();

        fs::create_dir(&original_path).unwrap();
        fs::write(original_path.join("replacement-marker"), b"do-not-delete").unwrap();

        let expected_after_mutation = SandboxSnapshot::from_metadata(&sandbox.directory_file.metadata().unwrap());
        assert!(
            verify_sandbox_inventory(
                &sandbox.directory_file,
                &original_path,
                OsStr::new("input.pc"),
                expected_after_mutation,
            )
            .is_err(),
            "inventory must be read from the pinned directory, not the clean replacement path"
        );

        // Drop is the fail-safe path: no explicit finish is called.
        drop(sandbox);

        assert_eq!(
            fs::read(original_path.join("replacement-marker")).unwrap(),
            b"do-not-delete"
        );
        fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read(detached_path.join("renamed-input")).unwrap(), b"authenticated");
        assert_eq!(fs::read(detached_path.join("nested/payload")).unwrap(), b"extra");

        fs::remove_file(detached_path.join("renamed-input")).unwrap();
        fs::remove_file(detached_path.join("nested/payload")).unwrap();
        fs::remove_dir(detached_path.join("nested")).unwrap();
        fs::remove_dir(&detached_path).unwrap();
        fs::remove_file(original_path.join("replacement-marker")).unwrap();
        fs::remove_dir(&original_path).unwrap();
    }

    #[test]
    fn finished_detached_sandbox_is_emptied_without_deleting_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"authenticated").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
        let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
        let original_path = sandbox.working_directory().to_owned();
        let detached_path = original_path.with_file_name(format!(
            "{}.detached-finished",
            original_path.file_name().unwrap().to_string_lossy()
        ));

        fs::rename(&original_path, &detached_path).unwrap();
        fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(detached_path.join("nested")).unwrap();
        fs::write(detached_path.join("nested/payload"), b"extra").unwrap();
        fs::create_dir(&original_path).unwrap();
        fs::write(original_path.join("replacement-marker"), b"do-not-delete").unwrap();

        let operation: Result<(), BoxError> = Err(Box::new(std::io::Error::other("fixture failure")));
        assert!(sandbox.finish(&info, operation).is_err());

        assert_eq!(
            fs::read(original_path.join("replacement-marker")).unwrap(),
            b"do-not-delete"
        );
        fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read_dir(&detached_path).unwrap().count(), 0);

        fs::remove_dir(&detached_path).unwrap();
        fs::remove_file(original_path.join("replacement-marker")).unwrap();
        fs::remove_dir(&original_path).unwrap();
    }

    #[test]
    fn pkg_config_pcfiledir_uses_logical_install_path_not_random_sandbox() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usr/lib/pkgconfig/demo.pc");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"Name: demo\nDescription: demo\nVersion: 1\n").unwrap();
        let info = collect_path(root.path(), &path);
        let input = VerifiedAnalyzerInput::from_path_info(&info, 256).unwrap();
        let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
        let mut command = analyzer_command(pkg_config_program().to_str().unwrap());
        command
            .current_dir(sandbox.working_directory())
            .args(["--define-variable=pcfiledir=/usr/lib/pkgconfig", "--variable=pcfiledir"])
            .arg(sandbox.path())
            .env("LC_ALL", "C");

        let operation = checked_output_for(&info, command);
        let output = sandbox.finish(&info, operation).unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "/usr/lib/pkgconfig");
    }

    #[test]
    fn pkg_config_handler_runs_real_tool_without_original_or_proc_path() {
        let install = tempfile::tempdir().unwrap();
        let path = install.path().join("usr/lib/pkgconfig/demo.pc");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = b"Name: demo\nDescription: demo\nVersion: 1\n";
        fs::write(&path, content).unwrap();

        let mut collector = Collector::new(install.path());
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&path, &mut hasher).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let program = pkg_config_program();
        plan.analysis.tools.pkg_config = Some(ExecutablePlan {
            path: program.to_string_lossy().into_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: "pkg-config".to_owned(),
            },
        });
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            paths: &paths,
        };

        let response = pkg_config(&mut bucket, &mut info).unwrap();
        assert!(matches!(response.decision, Decision::NextHandler));
        assert_eq!(
            providers,
            BTreeSet::from([Provider::new(Kind::PkgConfig, "demo").unwrap()])
        );
        assert!(dependencies.is_empty());
        assert_eq!(fs::read(path).unwrap(), content);
    }

    #[test]
    fn compressman_declares_regular_output_then_collector_publishes_it() {
        let install = tempfile::tempdir().unwrap();
        let path = install.path().join("usr/share/man/man1/demo.1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = b"deterministic manual page\n";
        fs::write(&path, content).unwrap();

        let mut collector = Collector::new(install.path());
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut info = collector.path(&path, &mut hasher).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        plan.analysis.compress_man = true;
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            paths: &paths,
        };

        let response = compressman(&mut bucket, &mut info).unwrap();
        let compressed = path.with_added_extension("zst");
        assert_eq!(compressed, path.parent().unwrap().join("demo.1.zst"));
        assert!(matches!(
            &response.decision,
            Decision::ReplaceFile { newpath } if newpath == &compressed
        ));
        assert_eq!(response.publications.len(), 1);
        assert!(
            !compressed.exists(),
            "handler published outside the collector transaction"
        );
        assert_eq!(fs::read(&path).unwrap(), content);

        let published = collector
            .publish_generated(&response.publications, &mut hasher)
            .unwrap();
        assert_eq!(published.len(), 1);
        assert!(!path.parent().unwrap().join("demo.1..zst").exists());
        let decoded = zstd::stream::decode_all(fs::File::open(&compressed).unwrap()).unwrap();
        assert_eq!(decoded, content);
        collector.seal().unwrap();
    }

    #[test]
    fn compressman_symlink_publication_does_not_eagerly_mutate_its_target() {
        let install = tempfile::tempdir().unwrap();
        let directory = install.path().join("usr/share/man/man1");
        fs::create_dir_all(&directory).unwrap();
        let target = directory.join("demo.1");
        let link = directory.join("alias.1");
        let content = b"target manual page\n";
        fs::write(&target, content).unwrap();
        symlink("demo.1", &link).unwrap();

        let mut collector = Collector::new(install.path());
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        let mut hasher = StoneDigestWriterHasher::new();
        let mut link_info = collector.path(&link, &mut hasher).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        plan.analysis.compress_man = true;
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let mut providers = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut bucket = BucketMut {
            providers: &mut providers,
            dependencies: &mut dependencies,
            analysis: &plan.analysis,
            paths: &paths,
        };

        let link_response = compressman(&mut bucket, &mut link_info).unwrap();
        let compressed_link = link.with_added_extension("zst");
        let compressed_target = target.with_added_extension("zst");
        assert_eq!(compressed_link, directory.join("alias.1.zst"));
        assert_eq!(compressed_target, directory.join("demo.1.zst"));
        assert!(!compressed_link.exists());
        assert!(!compressed_target.exists());
        collector
            .publish_generated(&link_response.publications, &mut hasher)
            .unwrap();
        assert_eq!(fs::read_link(&compressed_link).unwrap(), Path::new("demo.1.zst"));
        assert!(!directory.join("alias.1..zst").exists());
        assert!(!directory.join("demo.1..zst").exists());
        assert!(
            !compressed_target.exists(),
            "symlink handling eagerly wrote a different collected path"
        );

        let mut target_info = collector.path(&target, &mut hasher).unwrap();
        let target_response = compressman(&mut bucket, &mut target_info).unwrap();
        collector
            .publish_generated(&target_response.publications, &mut hasher)
            .unwrap();
        assert_eq!(
            zstd::stream::decode_all(fs::File::open(&compressed_target).unwrap()).unwrap(),
            content
        );
        collector.seal().unwrap();
    }

    #[test]
    fn pkg_config_dependency_output_requires_complete_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        fs::write(&path, b"fixture").unwrap();
        let info = collect_path(root.path(), &path);

        assert!(parse_pkg_config_dependencies(&info, "").unwrap().is_empty());
        assert_eq!(
            parse_pkg_config_dependencies(
                &info,
                "plain\nexact = 1\ndifferent != 1\nolder < 2\nnewer > 3\nmaximum <= 4\nminimum >= 5\n",
            )
            .unwrap(),
            ["plain", "exact", "different", "older", "newer", "maximum", "minimum",]
        );

        for malformed in [
            "\n",
            "   \n",
            "demo ???\n",
            "demo >=\n",
            "demo >= 1 trailing\n",
            "demo>=1\n",
            "demo\n\nother\n",
        ] {
            assert!(
                parse_pkg_config_dependencies(&info, malformed).is_err(),
                "accepted malformed pkg-config output {malformed:?}"
            );
        }
    }

    #[test]
    fn frozen_root_regular_lookup_stays_beneath_its_descriptor_anchor() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("frozen-root");
        let pkgconfig = root.join("usr/lib32/pkgconfig");
        fs::create_dir_all(&pkgconfig).unwrap();
        fs::write(pkgconfig.join("external.pc"), b"external").unwrap();
        symlink("external.pc", pkgconfig.join("link.pc")).unwrap();
        let anchor = open_frozen_root_anchor(&root).unwrap();

        assert!(
            descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/external.pc")).unwrap()
        );
        assert!(!descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/link.pc")).unwrap());
        assert!(
            !descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/missing.pc")).unwrap()
        );

        let displaced = temporary.path().join("displaced-root");
        fs::rename(&root, &displaced).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(
            descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/external.pc")).unwrap(),
            "lookup followed a replaced root pathname instead of its descriptor"
        );
        assert!(descriptor_root_contains_regular_target(&anchor, Path::new("../external.pc")).is_err());
    }

    #[test]
    fn symlink_and_fifo_never_become_analyzer_inputs() {
        let symlink_root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = symlink_root.path().join("input.pc");
        symlink(outside.path(), &link).unwrap();
        let link_info = collect_path(symlink_root.path(), &link);
        assert!(VerifiedAnalyzerInput::from_path_info(&link_info, 64).is_err());

        let fifo_root = tempfile::tempdir().unwrap();
        let fifo = fifo_root.path().join("input.pc");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let fifo_info = collect_path(fifo_root.path(), &fifo);
        let started = Instant::now();
        assert!(VerifiedAnalyzerInput::from_path_info(&fifo_info, 64).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn path_replacement_never_becomes_analyzer_input() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.pc");
        let displaced = root.path().join("displaced");
        fs::write(&path, b"original").unwrap();
        let info = collect_path(root.path(), &path);
        fs::rename(&path, displaced).unwrap();
        fs::write(&path, b"attacker").unwrap();

        assert!(VerifiedAnalyzerInput::from_path_info(&info, 64).is_err());
    }

    #[test]
    fn analyzer_commands_have_no_ambient_environment_stdin_or_descriptors() {
        let environment = checked_output(analyzer_command("/usr/bin/env")).unwrap();
        assert!(environment.stdout.is_empty());

        let inherited = tempfile::tempfile().unwrap();
        let inherited_fd = inherited.as_raw_fd();
        fcntl(inherited_fd, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();

        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", &format!("test ! -e /proc/self/fd/{inherited_fd} && ! read value")]);

        checked_output(command).unwrap();
    }

    #[test]
    fn analyzer_command_failure_is_rejected_even_with_partial_stdout() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf partial-output; printf analyzer-failed >&2; exit 9"]);

        let error = checked_output(command).unwrap_err().to_string();

        assert!(error.contains("exit status: 9"), "{error}");
        assert!(error.contains("analyzer-failed"), "{error}");
    }

    #[test]
    fn analyzer_output_accepts_each_pipe_at_its_exact_byte_limit() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf 12345678; printf abcdefgh >&2"]);

        let output = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap();

        assert_eq!(output.stdout, b"12345678");
        assert_eq!(output.stderr, b"abcdefgh");
    }

    #[test]
    fn analyzer_stdout_rejects_one_byte_over_limit() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf 123456789"]);

        let error = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap_err();

        assert!(matches!(
            execution_error(&error),
            AnalyzerExecutionError::OutputLimit {
                pipe: AnalyzerPipe::Stdout,
                limit: 8,
                ..
            }
        ));
    }

    #[test]
    fn analyzer_stderr_rejects_one_byte_over_limit() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf abcdefghi >&2"]);

        let error = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap_err();

        assert!(matches!(
            execution_error(&error),
            AnalyzerExecutionError::OutputLimit {
                pipe: AnalyzerPipe::Stderr,
                limit: 8,
                ..
            }
        ));
    }

    #[test]
    fn sleeping_analyzer_times_out_and_its_background_process_is_cleaned_up() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("delayed-write");
        let mut command = analyzer_command("/bin/sh");
        command.env("MARKER", &marker).args([
            "-c",
            "(/bin/sleep 0.2; printf escaped > \"$MARKER\") & exec /bin/sleep 30",
        ]);

        let started = Instant::now();
        let error = checked_test_output(command, test_limits(64, 64, Duration::from_millis(50))).unwrap_err();

        assert!(matches!(
            execution_error(&error),
            AnalyzerExecutionError::Timeout {
                timeout,
                ..
            } if *timeout == Duration::from_millis(50)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(400));
        assert!(!marker.exists());
    }

    #[test]
    fn background_analyzer_pipe_holder_cannot_hang_packaging() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf direct-output; (/bin/sleep 30) &"]);

        let started = Instant::now();
        let output = checked_output(command).unwrap();

        assert_eq!(output.stdout, b"direct-output");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn background_analyzer_cannot_mutate_after_direct_child_exit() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("delayed-write");
        let mut command = analyzer_command("/bin/sh");
        command
            .env("MARKER", &marker)
            .args(["-c", "(sleep 0.2; printf escaped > \"$MARKER\") &"]);

        checked_output(command).unwrap();
        thread::sleep(Duration::from_millis(500));

        assert!(!marker.exists());
    }

    #[test]
    fn pipe_reader_cleanup_has_a_finite_deadline() {
        let (stdout_reader, stdout_writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (stderr_reader, stderr_writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (events, _received) = mpsc::channel();
        let readers = [
            read_analyzer_pipe(stdout_reader, AnalyzerPipe::Stdout, 64, events.clone()).unwrap(),
            read_analyzer_pipe(stderr_reader, AnalyzerPipe::Stderr, 64, events).unwrap(),
        ];
        let started = Instant::now();
        let error = join_analyzer_pipe_readers_until(
            readers,
            "blocked pipe fixture",
            Instant::now() + Duration::from_millis(25),
        )
        .unwrap_err();

        assert!(matches!(error, AnalyzerExecutionError::ReaderCleanupTimeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(stdout_writer);
        drop(stderr_writer);
    }

    #[test]
    fn analyzer_operation_and_cleanup_failures_are_both_preserved() {
        let operation = AnalyzerExecutionError::Timeout {
            invocation: "fixture".to_owned(),
            timeout: Duration::from_millis(10),
        };
        let cleanup = Err(AnalyzerExecutionError::ReaderCleanupTimeout {
            invocation: "fixture".to_owned(),
            timeout: Duration::from_millis(20),
        });

        let combined = with_analyzer_cleanup(operation, cleanup);
        assert!(matches!(
            combined,
            AnalyzerExecutionError::OperationCleanup {
                operation,
                cleanup,
            } if matches!(*operation, AnalyzerExecutionError::Timeout { .. })
                && matches!(*cleanup, AnalyzerExecutionError::ReaderCleanupTimeout { .. })
        ));
    }

    #[test]
    fn production_containment_is_rejected_before_analyzer_spawn() {
        if getpid().as_raw() == 1 {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("spawned");
        let mut command = analyzer_command("/bin/sh");
        command
            .env("MARKER", &marker)
            .args(["-c", "printf spawned > \"$MARKER\""]);

        let error = contained_output(
            &mut command,
            AnalyzerContainment::PidNamespace,
            test_limits(64, 64, Duration::from_secs(1)),
            "containment fixture",
        )
        .unwrap_err();

        assert!(matches!(error, AnalyzerExecutionError::Containment { .. }));
        assert!(!marker.exists());
    }

    #[test]
    fn production_analyzer_cleanup_targets_the_complete_pid_namespace() {
        assert_eq!(
            analyzer_descendant_signal_target(AnalyzerContainment::PidNamespace, Pid::from_raw(1234)),
            Pid::from_raw(-1)
        );
    }

    #[test]
    fn production_handlers_do_not_embed_analyzer_program_selection() {
        let production = |source: &'static str| source.split("#[cfg(test)]").next().unwrap();
        let sources = [
            production(include_str!("handler.rs")),
            production(include_str!("handler/python.rs")),
            production(include_str!("handler/elf.rs")),
        ];

        for source in sources {
            assert!(
                !source.contains("/proc/"),
                "production analyzer input depends on procfs"
            );
            assert!(
                !source.contains("command_path"),
                "production analyzer passes inherited-descriptor path"
            );
            for forbidden in [
                "/usr/bin/pkg-config",
                "/usr/bin/python3",
                "/usr/bin/llvm-objcopy",
                "/usr/bin/llvm-strip",
                "/usr/bin/objcopy",
                "/usr/bin/strip",
                "AnalysisToolchain",
            ] {
                assert!(!source.contains(forbidden), "production analyzer embeds {forbidden}");
            }
        }
    }
}
