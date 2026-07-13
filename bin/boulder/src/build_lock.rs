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
    BUILD_LOCK_FILE_NAME, BuildLock, BuildLockDecodeError, BuildLockValidationError, LockedIdentity, Platform,
    decode_build_lock, encode_build_lock,
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

/// Planner-selected values which must agree exactly with a reusable lock.
///
/// These values are deliberately independent rather than inferred from one
/// another. In particular, repository policy identity is not a target name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedBuildLockContext {
    pub requested_providers: Vec<String>,
    pub build_platform: Platform,
    pub host_platform: Platform,
    pub target_platform: Platform,
    pub policy: LockedIdentity,
    pub target: LockedIdentity,
    pub profile: LockedIdentity,
    pub toolchain: LockedIdentity,
    pub builder: LockedIdentity,
}

/// Load a generated lock for the current request and prove that its frozen
/// selections still match the authoritative planner context.
///
/// The request fingerprint is not sufficient evidence by itself: generated
/// lock text is user-editable, so a lock may retain that fingerprint while its
/// selected identities, requested roots, or platform tuples are changed by
/// hand.
pub fn require_current_for_context(
    path: &Path,
    request_fingerprint: &str,
    expected: &ExpectedBuildLockContext,
) -> Result<BuildLock, Error> {
    let lock = require_current(path, request_fingerprint)?;
    validate_context(path, &lock, expected)?;
    Ok(lock)
}

fn validate_context(path: &Path, lock: &BuildLock, expected: &ExpectedBuildLockContext) -> Result<(), Error> {
    let mut expected_requests = expected
        .requested_providers
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    expected_requests.sort_unstable();
    let mut found_requests = lock
        .requests
        .iter()
        .map(|request| request.request.as_str())
        .collect::<Vec<_>>();
    found_requests.sort_unstable();
    require_selected_value(
        path,
        "requests",
        &format!("{expected_requests:?}"),
        &format!("{found_requests:?}"),
    )?;

    validate_platform(path, "build_platform", &expected.build_platform, &lock.build_platform)?;
    validate_platform(path, "host_platform", &expected.host_platform, &lock.host_platform)?;
    validate_platform(
        path,
        "target_platform",
        &expected.target_platform,
        &lock.target_platform,
    )?;
    validate_identity(path, "policy", &expected.policy, &lock.policy)?;
    validate_identity(path, "target", &expected.target, &lock.target)?;
    validate_identity(path, "profile", &expected.profile, &lock.profile)?;
    validate_identity(path, "toolchain", &expected.toolchain, &lock.toolchain)?;
    validate_identity(path, "builder", &expected.builder, &lock.builder)
}

fn validate_platform(path: &Path, field: &str, expected: &Platform, found: &Platform) -> Result<(), Error> {
    let fields = [
        (
            "architecture",
            expected.architecture.as_str(),
            found.architecture.as_str(),
        ),
        ("vendor", expected.vendor.as_str(), found.vendor.as_str()),
        (
            "operating_system",
            expected.operating_system.as_str(),
            found.operating_system.as_str(),
        ),
        ("abi", expected.abi.as_str(), found.abi.as_str()),
    ];
    for (component, expected, found) in fields {
        require_selected_value(path, &format!("{field}.{component}"), expected, found)?;
    }
    Ok(())
}

fn validate_identity(path: &Path, field: &str, expected: &LockedIdentity, found: &LockedIdentity) -> Result<(), Error> {
    require_selected_value(path, &format!("{field}.name"), &expected.name, &found.name)?;
    require_selected_value(
        path,
        &format!("{field}.fingerprint"),
        &expected.fingerprint,
        &found.fingerprint,
    )
}

fn require_selected_value(path: &Path, field: &str, expected: &str, found: &str) -> Result<(), Error> {
    if expected == found {
        return Ok(());
    }
    Err(Error::SelectedContextStale {
        path: path.to_owned(),
        field: field.to_owned(),
        expected: expected.to_owned(),
        found: found.to_owned(),
    })
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
    #[error(
        "generated build lock {path:?} is stale ({field}: expected {expected:?}, found {found:?}); rerun with `--update-lock`"
    )]
    SelectedContextStale {
        path: PathBuf,
        field: String,
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
        BUILD_LOCK_SCHEMA_VERSION, LockedOutput, LockedPackage, LockedRequest, RepositorySnapshot,
    };

    use super::*;

    fn platform() -> Platform {
        Platform {
            architecture: "x86_64".to_owned(),
            vendor: "unknown".to_owned(),
            operating_system: "linux".to_owned(),
            abi: "gnu".to_owned(),
        }
    }

    fn identity(name: &str) -> LockedIdentity {
        LockedIdentity {
            name: name.to_owned(),
            fingerprint: format!("{name}-fingerprint"),
        }
    }

    fn lock(request: &str) -> BuildLock {
        BuildLock {
            schema_version: BUILD_LOCK_SCHEMA_VERSION,
            request_fingerprint: request.to_owned(),
            repositories: vec![RepositorySnapshot {
                id: "volatile".to_owned(),
                index_uri: "https://example.invalid/stone.index".to_owned(),
                snapshot: "snapshot".to_owned(),
            }],
            // Two provider names intentionally resolve to one root. Either
            // request can be removed while the package closure remains valid,
            // allowing reuse validation to prove exact root-set matching.
            requests: vec![
                LockedRequest {
                    request: "binary(example)".to_owned(),
                    package_id: "package-id".to_owned(),
                    output: "out".to_owned(),
                },
                LockedRequest {
                    request: "package(example)".to_owned(),
                    package_id: "package-id".to_owned(),
                    output: "out".to_owned(),
                },
            ],
            packages: vec![LockedPackage {
                package_id: "package-id".to_owned(),
                name: "example".to_owned(),
                version: "1.0.0-1-1".to_owned(),
                architecture: "x86_64".to_owned(),
                repository: "volatile".to_owned(),
                outputs: vec![LockedOutput { name: "out".to_owned() }],
                dependencies: Vec::new(),
            }],
            build_platform: platform(),
            host_platform: platform(),
            target_platform: platform(),
            policy: identity("aerynos"),
            target: identity("x86_64"),
            profile: identity("profile"),
            toolchain: identity("toolchain"),
            builder: identity("builder"),
        }
    }

    fn expected_context(lock: &BuildLock) -> ExpectedBuildLockContext {
        ExpectedBuildLockContext {
            requested_providers: lock.requests.iter().map(|request| request.request.clone()).collect(),
            build_platform: lock.build_platform.clone(),
            host_platform: lock.host_platform.clone(),
            target_platform: lock.target_platform.clone(),
            policy: lock.policy.clone(),
            target: lock.target.clone(),
            profile: lock.profile.clone(),
            toolchain: lock.toolchain.clone(),
            builder: lock.builder.clone(),
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

    #[test]
    fn current_lock_must_match_every_planner_selected_field() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(BUILD_LOCK_FILE_NAME);
        let original = lock("request");
        let expected = expected_context(&original);

        write(&path, &original).unwrap();
        assert_eq!(
            require_current_for_context(&path, "request", &expected).unwrap(),
            original
        );

        let mutations: [(&str, fn(&mut BuildLock)); 22] = [
            ("policy.name", |lock| lock.policy.name = "changed".to_owned()),
            ("policy.fingerprint", |lock| {
                lock.policy.fingerprint = "changed".to_owned()
            }),
            ("target.name", |lock| lock.target.name = "changed".to_owned()),
            ("target.fingerprint", |lock| {
                lock.target.fingerprint = "changed".to_owned()
            }),
            ("profile.name", |lock| lock.profile.name = "changed".to_owned()),
            ("profile.fingerprint", |lock| {
                lock.profile.fingerprint = "changed".to_owned()
            }),
            ("toolchain.name", |lock| lock.toolchain.name = "changed".to_owned()),
            ("toolchain.fingerprint", |lock| {
                lock.toolchain.fingerprint = "changed".to_owned()
            }),
            ("builder.name", |lock| lock.builder.name = "changed".to_owned()),
            ("builder.fingerprint", |lock| {
                lock.builder.fingerprint = "changed".to_owned()
            }),
            ("build_platform.architecture", |lock| {
                lock.build_platform.architecture = "changed".to_owned()
            }),
            ("build_platform.vendor", |lock| {
                lock.build_platform.vendor = "changed".to_owned()
            }),
            ("build_platform.operating_system", |lock| {
                lock.build_platform.operating_system = "changed".to_owned()
            }),
            ("build_platform.abi", |lock| {
                lock.build_platform.abi = "changed".to_owned()
            }),
            ("host_platform.architecture", |lock| {
                lock.host_platform.architecture = "changed".to_owned()
            }),
            ("host_platform.vendor", |lock| {
                lock.host_platform.vendor = "changed".to_owned()
            }),
            ("host_platform.operating_system", |lock| {
                lock.host_platform.operating_system = "changed".to_owned()
            }),
            ("host_platform.abi", |lock| {
                lock.host_platform.abi = "changed".to_owned()
            }),
            ("target_platform.architecture", |lock| {
                lock.target_platform.architecture = "changed".to_owned()
            }),
            ("target_platform.vendor", |lock| {
                lock.target_platform.vendor = "changed".to_owned()
            }),
            ("target_platform.operating_system", |lock| {
                lock.target_platform.operating_system = "changed".to_owned()
            }),
            ("target_platform.abi", |lock| {
                lock.target_platform.abi = "changed".to_owned()
            }),
        ];

        for (expected_field, mutate) in mutations {
            let mut changed = lock("request");
            mutate(&mut changed);
            assert_eq!(changed.request_fingerprint, "request");
            write(&path, &changed).unwrap();

            let error = require_current_for_context(&path, "request", &expected).unwrap_err();
            assert!(
                matches!(&error, Error::SelectedContextStale { field, .. } if field == expected_field),
                "unexpected error for {expected_field}: {error:?}"
            );
            assert!(error.to_string().contains("rerun with `--update-lock`"));
        }
    }

    #[test]
    fn current_lock_must_match_exact_requested_provider_roots() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(BUILD_LOCK_FILE_NAME);
        let original = lock("request");
        let expected = expected_context(&original);

        let mut missing = original.clone();
        missing.requests.pop();
        assert_eq!(missing.request_fingerprint, "request");
        write(&path, &missing).unwrap();
        let error = require_current_for_context(&path, "request", &expected).unwrap_err();
        assert!(matches!(&error, Error::SelectedContextStale { field, .. } if field == "requests"));

        let mut extra = original;
        extra.requests.push(LockedRequest {
            request: "soname(libexample.so.1)".to_owned(),
            package_id: "package-id".to_owned(),
            output: "out".to_owned(),
        });
        assert_eq!(extra.request_fingerprint, "request");
        write(&path, &extra).unwrap();
        let error = require_current_for_context(&path, "request", &expected).unwrap_err();
        assert!(matches!(&error, Error::SelectedContextStale { field, .. } if field == "requests"));
        assert!(error.to_string().contains("rerun with `--update-lock`"));
    }
}
