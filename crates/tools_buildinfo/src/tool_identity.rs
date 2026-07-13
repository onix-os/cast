// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Content-strong identity for executables selected by a build.
//!
//! A version string is not an executable identity: two independently built
//! tools can report the same version while producing different output.  This
//! module resolves the program exactly as `Command` would, binds its canonical
//! path and bytes, and optionally records a stable version probe.

use std::{
    ffi::OsStr,
    io::{self, Read as _},
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
};

use fs_err as fs;
use sha2::{Digest as _, Sha256};

const IDENTITY_DOMAIN: &[u8] = b"org.aerynos.os-tools.executable-identity.v1";
const COMMAND_IDENTITY_DOMAIN: &[u8] = b"org.aerynos.os-tools.executable-command-identity.v1";
const WORKSPACE_TOKEN: &[u8] = b"${WORKSPACE}";

/// The exact executable selected for one semantic build role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutableIdentity {
    resolved_path: PathBuf,
    content_sha256: [u8; 32],
    version: Option<Vec<u8>>,
}

/// Ordered executable identities participating in one selected command.
///
/// Native compiler selectors may name a known cache/distribution wrapper and
/// then the compiler it delegates to (for example, `sccache clang`).  Both
/// executable byte streams must be bound even when the delegated compiler
/// reports the same version after replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandIdentity {
    executables: Vec<ExecutableIdentity>,
}

impl CommandIdentity {
    pub(crate) fn new(primary: ExecutableIdentity) -> Self {
        Self {
            executables: vec![primary],
        }
    }

    pub(crate) fn push_delegated(&mut self, executable: ExecutableIdentity) {
        self.executables.push(executable);
    }

    pub(crate) fn encode(&self, workspace_root: &Path) -> Vec<u8> {
        let mut encoded = Vec::new();
        write_field(&mut encoded, COMMAND_IDENTITY_DOMAIN);
        write_count(&mut encoded, self.executables.len());
        for executable in &self.executables {
            write_field(&mut encoded, &executable.encode(workspace_root));
        }
        encoded
    }
}

impl ExecutableIdentity {
    /// Construct an identity from already verified parts.
    ///
    /// This is primarily useful for pure selection tests.  Production callers
    /// should use [`identify`] so path resolution, executable validation, and
    /// content hashing cannot drift apart.
    pub(crate) fn from_parts(resolved_path: PathBuf, content_sha256: [u8; 32], version: Option<Vec<u8>>) -> Self {
        Self {
            resolved_path,
            content_sha256,
            version,
        }
    }

    pub(crate) fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    /// Canonically encode this identity for the semantic build fingerprint.
    ///
    /// Workspace-local paths in both the resolved executable path and version
    /// output use a stable token so equivalent source checkouts retain one
    /// identity.  The executable digest itself is always the SHA-256 of the
    /// exact bytes at the resolved path.
    pub(crate) fn encode(&self, workspace_root: &Path) -> Vec<u8> {
        let workspace = workspace_root.as_os_str().as_bytes();
        let mut encoded = Vec::new();
        write_field(&mut encoded, IDENTITY_DOMAIN);
        write_field(
            &mut encoded,
            &normalize_workspace(self.resolved_path.as_os_str().as_bytes(), workspace),
        );
        write_field(&mut encoded, &self.content_sha256);
        match &self.version {
            Some(version) => {
                encoded.push(1);
                write_field(&mut encoded, &normalize_workspace(version, workspace));
            }
            None => encoded.push(0),
        }
        encoded
    }
}

/// Resolve and identify `program` using the supplied command-search context.
///
/// `None` means that no executable was found.  Other failures are errors, so a
/// caller may use `None` for an explicit fallback rule without accidentally
/// treating unreadable or unstable executable state as absence.  `probe`
/// receives the canonical executable path and returns stable version output,
/// or `None` for executable roles (such as Cargo wrappers) without a version
/// protocol.
pub(crate) fn identify<F>(
    program: &OsStr,
    search_path: Option<&OsStr>,
    working_directory: &Path,
    probe: F,
) -> io::Result<Option<ExecutableIdentity>>
where
    F: FnOnce(&Path) -> io::Result<Option<Vec<u8>>>,
{
    let Some(resolved_path) = resolve(program, search_path, working_directory)? else {
        return Ok(None);
    };

    let before = hash_executable(&resolved_path)?;
    let version = probe(&resolved_path)?;
    let after = hash_executable(&resolved_path)?;
    if before != after {
        return Err(invalid_data(format!(
            "selected executable changed while it was being identified: {}",
            resolved_path.display()
        )));
    }

    Ok(Some(ExecutableIdentity::from_parts(resolved_path, before, version)))
}

fn resolve(program: &OsStr, search_path: Option<&OsStr>, working_directory: &Path) -> io::Result<Option<PathBuf>> {
    if program.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected executable program is empty",
        ));
    }

    if program.as_bytes().contains(&b'/') {
        let path = Path::new(program);
        let candidate = if path.is_absolute() {
            path.to_owned()
        } else {
            working_directory.join(path)
        };
        return resolve_candidate(&candidate);
    }

    let search_path = search_path.ok_or_else(|| {
        invalid_data(format!(
            "cannot resolve selected executable {program:?}: PATH is missing"
        ))
    })?;
    let mut unusable = None;
    for directory in std::env::split_paths(search_path) {
        let directory = if directory.as_os_str().is_empty() {
            working_directory.to_owned()
        } else if directory.is_absolute() {
            directory
        } else {
            working_directory.join(directory)
        };
        let candidate = directory.join(program);
        match resolve_candidate(&candidate) {
            Ok(Some(path)) => return Ok(Some(path)),
            Ok(None) => {}
            Err(source) if source.kind() == io::ErrorKind::PermissionDenied => {
                unusable.get_or_insert((candidate, source));
            }
            Err(source) => return Err(source),
        }
    }

    if let Some((candidate, source)) = unusable {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "selected executable candidate is not usable: {}: {source}",
                candidate.display()
            ),
        ))
    } else {
        Ok(None)
    }
}

fn resolve_candidate(candidate: &Path) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::metadata(candidate) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not an executable regular file", candidate.display()),
        ));
    }

    let resolved = candidate.canonicalize()?;
    let resolved_metadata = fs::metadata(&resolved)?;
    if !resolved_metadata.is_file() || resolved_metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not an executable regular file", resolved.display()),
        ));
    }
    Ok(Some(resolved))
}

fn hash_executable(path: &Path) -> io::Result<[u8; 32]> {
    let mut executable = io::BufReader::new(fs::File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = executable.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub(crate) fn normalize_workspace(value: &[u8], workspace: &[u8]) -> Vec<u8> {
    if workspace.is_empty() || value.len() < workspace.len() {
        return value.to_vec();
    }

    let mut normalized = Vec::with_capacity(value.len());
    let mut offset = 0;
    while offset < value.len() {
        let end = offset.saturating_add(workspace.len());
        if end <= value.len() && &value[offset..end] == workspace && (end == value.len() || value[end] == b'/') {
            normalized.extend_from_slice(WORKSPACE_TOKEN);
            offset = end;
        } else {
            normalized.push(value[offset]);
            offset += 1;
        }
    }
    normalized
}

fn write_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("executable identity field length fits in u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

fn write_count(output: &mut Vec<u8>, value: usize) {
    let value = u64::try_from(value).expect("executable identity count fits in u64");
    output.extend_from_slice(&value.to_be_bytes());
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
