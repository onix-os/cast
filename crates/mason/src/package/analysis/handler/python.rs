// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use fs_err::{self as fs, os::unix::fs::OpenOptionsExt};
use regex::Regex;
use stone::relation::{Dependency, Kind, Provider};
use thiserror::Error;

use crate::package::collect::PathInfo;

use mailparse::{MailHeaderMap, parse_mail};

use super::{BoxError, BucketMut, Decision, Response, analyzer_command, checked_output};

const PYTHON_METADATA_BYTES: usize = 1024 * 1024;

pub fn python(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_path = info.path.clone().into_os_string().into_string().unwrap_or_default();
    let is_dist_info = file_path.contains(".dist-info") && info.file_name().ends_with("METADATA");
    let is_egg_info = file_path.contains(".egg-info") && info.file_name().ends_with("PKG-INFO");

    if !(is_dist_info || is_egg_info) {
        return Ok(Decision::NextHandler.into());
    }

    let data = read_python_metadata(&info.path, PYTHON_METADATA_BYTES)?;
    let mail = parse_mail(&data)?;
    let python_name_raw = mail
        .get_headers()
        .get_first_value("Name")
        .unwrap_or_else(|| panic!("Failed to parse {}", info.file_name()));

    let python_name = pep_503_normalize(&python_name_raw)?;

    /* Insert generic provider */
    bucket.providers.insert(Provider {
        kind: Kind::Python,
        name: python_name.clone(),
    });

    /* Now parse dependencies */
    let dist_path = info
        .path
        .parent()
        .unwrap_or_else(|| panic!("Failed to get parent path for {}", info.file_name()));
    let find_deps_script = include_str!("../scripts/get-py-deps.py");

    let program = &bucket
        .analysis
        .tools
        .python
        .as_ref()
        .expect("validated analysis plan requires Python for the Python handler")
        .path;
    let mut command = analyzer_command(program);
    command.arg("-c").arg(find_deps_script).arg(dist_path).envs([
        ("LC_ALL", "C"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONHASHSEED", "0"),
        ("PYTHONNOUSERSITE", "1"),
    ]);
    let output = checked_output(command)?;

    let deps = String::from_utf8_lossy(&output.stdout);
    for dep in deps.lines() {
        bucket.dependencies.insert(Dependency {
            kind: Kind::Python,
            name: pep_503_normalize(dep)?,
        });
    }

    Ok(Decision::NextHandler.into())
}

fn read_python_metadata(path: &Path, limit: usize) -> Result<Vec<u8>, PythonMetadataReadError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PythonMetadataReadError::Inspect {
        path: path.to_owned(),
        source,
    })?;
    require_bounded_regular_metadata(path, &metadata, limit)?;

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| PythonMetadataReadError::Open {
            path: path.to_owned(),
            source,
        })?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| PythonMetadataReadError::InspectOpened {
            path: path.to_owned(),
            source,
        })?;
    require_bounded_regular_metadata(path, &opened_metadata, limit)?;

    let capacity = usize::try_from(opened_metadata.len()).unwrap_or(limit).min(limit);
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PythonMetadataReadError::Read {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(PythonMetadataReadError::TooLarge {
            path: path.to_owned(),
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(bytes)
}

fn require_bounded_regular_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
    limit: usize,
) -> Result<(), PythonMetadataReadError> {
    if !metadata.file_type().is_file() {
        return Err(PythonMetadataReadError::NotRegular { path: path.to_owned() });
    }
    if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(PythonMetadataReadError::TooLarge {
            path: path.to_owned(),
            size: metadata.len(),
            limit,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
enum PythonMetadataReadError {
    #[error("failed to inspect Python metadata {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Python metadata {path} is not a regular file")]
    NotRegular { path: PathBuf },
    #[error("Python metadata {path} is {size} bytes, exceeding the {limit}-byte limit")]
    TooLarge { path: PathBuf, size: u64, limit: usize },
    #[error("failed to open Python metadata {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect opened Python metadata {path}: {source}")]
    InspectOpened {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read Python metadata {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/* Normalize name per https://peps.python.org/pep-0503/#normalized-names, replacing
all runs of `_` and `.` with `-` and lowercaseing */
fn pep_503_normalize(input: &str) -> Result<String, BoxError> {
    let re = Regex::new(r"[-_.]+")?;

    Ok(re.replace_all(input, "-").to_lowercase())
}

#[cfg(test)]
mod test {
    use std::{
        os::unix::fs::symlink,
        time::{Duration, Instant},
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use super::*;

    #[test]
    fn test_normalization() {
        assert_eq!(pep_503_normalize("PyThOn-_-foo").unwrap(), "python-foo");
        assert_eq!(pep_503_normalize("PyThOn.-f-oo").unwrap(), "python-f-oo");
    }

    #[test]
    fn python_metadata_accepts_exact_byte_limit() {
        const LIMIT: usize = 64;
        let temporary = tempfile::tempdir().unwrap();
        let metadata = temporary.path().join("METADATA");
        fs::write(&metadata, vec![b'x'; LIMIT]).unwrap();

        assert_eq!(read_python_metadata(&metadata, LIMIT).unwrap(), vec![b'x'; LIMIT]);
    }

    #[test]
    fn python_metadata_rejects_one_byte_over_limit() {
        const LIMIT: usize = 64;
        let temporary = tempfile::tempdir().unwrap();
        let metadata = temporary.path().join("PKG-INFO");
        fs::write(&metadata, vec![b'x'; LIMIT + 1]).unwrap();

        let error = read_python_metadata(&metadata, LIMIT).unwrap_err();
        assert!(matches!(
            error,
            PythonMetadataReadError::TooLarge {
                size: 65,
                limit: LIMIT,
                ..
            }
        ));
    }

    #[test]
    fn python_metadata_rejects_symlinks_without_reading_the_target() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("outside");
        let metadata = temporary.path().join("METADATA");
        fs::write(&target, b"Name: escaped\n").unwrap();
        symlink(&target, &metadata).unwrap();

        assert!(matches!(
            read_python_metadata(&metadata, 64),
            Err(PythonMetadataReadError::NotRegular { .. })
        ));
    }

    #[test]
    fn python_metadata_rejects_fifo_without_waiting_for_a_writer() {
        let temporary = tempfile::tempdir().unwrap();
        let metadata = temporary.path().join("PKG-INFO");
        mkfifo(&metadata, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();

        let started = Instant::now();
        assert!(matches!(
            read_python_metadata(&metadata, 64),
            Err(PythonMetadataReadError::NotRegular { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
