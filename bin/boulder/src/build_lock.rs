// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Generated build-lock lifecycle.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs_err as fs;
use stone_recipe::derivation::{
    BUILD_LOCK_FILE_NAME, BuildLock, BuildLockDecodeError, BuildLockValidationError, decode_build_lock,
    encode_build_lock,
};
use thiserror::Error;

/// Relationship of generated lock data to the current explicit plan request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    Current(BuildLock),
    Stale {
        lock: BuildLock,
        expected: String,
        found: String,
    },
}

pub fn path_for_recipe(recipe: &Path) -> PathBuf {
    recipe.with_file_name(BUILD_LOCK_FILE_NAME)
}

pub fn load(path: &Path, request_fingerprint: &str) -> Result<Status, Error> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Status::Missing),
        Err(source) => {
            return Err(Error::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let lock = decode_build_lock(BUILD_LOCK_FILE_NAME, &bytes).map_err(|source| Error::Decode {
        path: path.to_owned(),
        source: Box::new(source),
    })?;
    if lock.request_fingerprint == request_fingerprint {
        Ok(Status::Current(lock))
    } else {
        Ok(Status::Stale {
            found: lock.request_fingerprint.clone(),
            expected: request_fingerprint.to_owned(),
            lock,
        })
    }
}

pub fn require_current(path: &Path, request_fingerprint: &str) -> Result<BuildLock, Error> {
    match load(path, request_fingerprint)? {
        Status::Current(lock) => Ok(lock),
        Status::Missing => Err(Error::Missing { path: path.to_owned() }),
        Status::Stale { expected, found, .. } => Err(Error::Stale {
            path: path.to_owned(),
            expected,
            found,
        }),
    }
}

/// Validate and atomically replace generated lock data. Identical canonical
/// bytes leave the existing inode untouched.
pub fn write(path: &Path, lock: &BuildLock) -> Result<WriteOutcome, Error> {
    lock.validate()?;
    let encoded = encode_build_lock(lock);
    match fs::read(path) {
        Ok(existing) if existing == encoded.as_bytes() => return Ok(WriteOutcome::Unchanged),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::Read {
                path: path.to_owned(),
                source,
            });
        }
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::InvalidPath(path.to_owned()))?
        .to_string_lossy();
    let (temporary, mut file) = create_temporary(parent, &file_name).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })?;
    let result = (|| {
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })?;
    Ok(WriteOutcome::Written)
}

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn create_temporary(parent: &Path, file_name: &str) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..100 {
        let counter = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{file_name}.tmp-{}-{counter}", std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique build-lock temporary file",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Unchanged,
    Written,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("generated build lock is missing at {path:?}; rerun with `--update-lock`")]
    Missing { path: PathBuf },
    #[error(
        "generated build lock {path:?} is stale (expected request {expected}, found {found}); rerun with `--update-lock`"
    )]
    Stale {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("read generated build lock {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode generated build lock {path:?}")]
    Decode {
        path: PathBuf,
        #[source]
        source: Box<BuildLockDecodeError>,
    },
    #[error("write generated build lock {path:?}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid generated build-lock path {0:?}")]
    InvalidPath(PathBuf),
    #[error(transparent)]
    Validation(#[from] BuildLockValidationError),
}

#[cfg(test)]
mod tests {
    use stone_recipe::derivation::{
        BUILD_LOCK_SCHEMA_VERSION, LockedIdentity, LockedOutput, LockedPackage, Platform, RepositorySnapshot,
    };

    use super::*;

    fn lock(request: &str) -> BuildLock {
        let platform = Platform {
            architecture: "x86_64".to_owned(),
            vendor: "unknown".to_owned(),
            operating_system: "linux".to_owned(),
            abi: "gnu".to_owned(),
        };
        let identity = |name: &str| LockedIdentity {
            name: name.to_owned(),
            fingerprint: format!("{name}-fingerprint"),
        };
        BuildLock {
            schema_version: BUILD_LOCK_SCHEMA_VERSION,
            request_fingerprint: request.to_owned(),
            base_state: "base-state".to_owned(),
            repositories: vec![RepositorySnapshot {
                id: "volatile".to_owned(),
                index_uri: "https://example.invalid/stone.index".to_owned(),
                snapshot: "snapshot".to_owned(),
            }],
            requests: Vec::new(),
            packages: vec![LockedPackage {
                package_id: "package-id".to_owned(),
                name: "example".to_owned(),
                version: "1.0.0-1-1".to_owned(),
                architecture: "x86_64".to_owned(),
                repository: "volatile".to_owned(),
                outputs: vec![LockedOutput {
                    name: "out".to_owned(),
                    id: "package-id".to_owned(),
                }],
                dependencies: Vec::new(),
            }],
            build_platform: platform.clone(),
            host_platform: platform.clone(),
            target_platform: platform,
            policy: identity("policy"),
            profile: identity("profile"),
            toolchain: identity("toolchain"),
            builder: identity("builder"),
        }
    }

    #[test]
    fn missing_current_and_stale_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(BUILD_LOCK_FILE_NAME);
        assert_eq!(load(&path, "request").unwrap(), Status::Missing);

        write(&path, &lock("request")).unwrap();
        assert!(matches!(load(&path, "request").unwrap(), Status::Current(_)));
        assert!(matches!(
            load(&path, "changed").unwrap(),
            Status::Stale { expected, found, .. } if expected == "changed" && found == "request"
        ));
    }

    #[test]
    fn unchanged_lock_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(BUILD_LOCK_FILE_NAME);
        assert_eq!(write(&path, &lock("request")).unwrap(), WriteOutcome::Written);
        let first = fs::metadata(&path).unwrap();
        assert_eq!(write(&path, &lock("request")).unwrap(), WriteOutcome::Unchanged);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(first.ino(), fs::metadata(path).unwrap().ino());
        }
    }
}
