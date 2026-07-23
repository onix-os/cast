// SPDX-FileCopyrightText: 2025 AerynOS Developers

use std::path::{Path, PathBuf};

use mailparse::{MailHeaderMap, parse_mail};
use stone::relation::{Dependency, Kind, Provider};
use thiserror::Error;

use crate::package::collect::PathInfo;

use super::{
    BoxError, BucketMut, Decision, ExternalAnalyzerInput, Response, VerifiedAnalyzerInput, analyzer_command,
    checked_output_for,
};

const PYTHON_METADATA_BYTES: usize = 1024 * 1024;

pub fn python(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let Some(metadata_name) = python_metadata_name(&info.target_path)? else {
        return Ok(Decision::NextHandler.into());
    };

    let input = VerifiedAnalyzerInput::from_path_info(info, PYTHON_METADATA_BYTES as u64)?;
    let data = input.read_all(PYTHON_METADATA_BYTES)?;
    info.check_deadline()?;
    let mail = parse_mail(&data)?;
    let python_name_raw = unique_python_name(&mail, &info.target_path)?;
    let provider = Provider::new(Kind::Python, pep_503_normalize(&python_name_raw)?)?;
    let find_deps_script = include_str!("../scripts/get-py-deps.py");
    let program = &bucket
        .analysis
        .tools
        .python
        .as_ref()
        .expect("validated analysis plan requires Python for the Python handler")
        .path;
    let directory_suffix = if metadata_name == "METADATA" {
        ".dist-info"
    } else {
        ".egg-info"
    };
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, metadata_name, directory_suffix)?;
    let operation = (|| {
        let mut command = analyzer_command(program);
        command
            .current_dir(sandbox.working_directory())
            .args(["-I", "-B", "-c"])
            .arg(find_deps_script)
            .arg(sandbox.working_directory())
            .envs([
                ("LC_ALL", "C"),
                ("PYTHONDONTWRITEBYTECODE", "1"),
                ("PYTHONHASHSEED", "0"),
                ("PYTHONNOUSERSITE", "1"),
                ("PYTHONSAFEPATH", "1"),
            ]);
        let output = checked_output_for(info, command)?;
        let stdout = String::from_utf8(output.stdout)?;
        let mut dependencies = Vec::new();
        dependencies.try_reserve(stdout.lines().count())?;
        for dependency in stdout.lines() {
            info.check_deadline()?;
            dependencies.push(Dependency::new(Kind::Python, pep_503_normalize(dependency)?)?);
        }
        info.check_deadline()?;
        Ok(dependencies)
    })();
    let dependencies = sandbox.finish(info, operation)?;

    // Commit relations only after strict decoding, canonical validation,
    // sandbox verification/cleanup, and the shared deadline all succeed.
    bucket.providers.insert(provider);
    bucket.dependencies.extend(dependencies);
    Ok(Decision::NextHandler.into())
}

fn python_metadata_name(path: &Path) -> Result<Option<&str>, PythonMetadataError> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if file_name != "METADATA" && file_name != "PKG-INFO" {
        return Ok(None);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| PythonMetadataError::MissingParent { path: path.to_owned() })?;
    let Some(parent_name) = parent.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let recognized = (file_name == "METADATA" && parent_name.ends_with(".dist-info"))
        || (file_name == "PKG-INFO" && parent_name.ends_with(".egg-info"));
    Ok(recognized.then_some(file_name))
}

fn unique_python_name(mail: &mailparse::ParsedMail<'_>, path: &Path) -> Result<String, PythonMetadataError> {
    let names = mail.get_headers().get_all_values("Name");
    match names.as_slice() {
        [] => Err(PythonMetadataError::MissingName { path: path.to_owned() }),
        [name] => Ok(name.clone()),
        _ => Err(PythonMetadataError::DuplicateName {
            path: path.to_owned(),
            count: names.len(),
        }),
    }
}

#[derive(Debug, Error)]
enum PythonMetadataError {
    #[error("Python metadata path {path} has no parent distribution directory")]
    MissingParent { path: PathBuf },
    #[error("Python metadata {path} has no Name header")]
    MissingName { path: PathBuf },
    #[error("Python metadata {path} has {count} Name headers; exactly one is required")]
    DuplicateName { path: PathBuf, count: usize },
    #[error("invalid Python distribution name {name:?}")]
    InvalidName { name: String },
}

fn pep_503_normalize(input: &str) -> Result<String, BoxError> {
    let bytes = input.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(Box::new(PythonMetadataError::InvalidName { name: input.to_owned() }));
    }

    let mut normalized = String::with_capacity(bytes.len());
    let mut previous_was_separator = false;
    for byte in bytes {
        if matches!(byte, b'-' | b'_' | b'.') {
            if !previous_was_separator {
                normalized.push('-');
                previous_was_separator = true;
            }
        } else {
            normalized.push(char::from(byte.to_ascii_lowercase()));
            previous_was_separator = false;
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_valid_names_and_rejects_empty_or_malformed_names() {
        assert_eq!(pep_503_normalize("PyThOn-_-foo").unwrap(), "python-foo");
        assert_eq!(pep_503_normalize("PyThOn.-f-oo").unwrap(), "python-f-oo");
        for invalid in ["", "-demo", "demo-", "demo name", "demo/name", "Kelvin", "ſetuptools"] {
            assert!(pep_503_normalize(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn recognizes_only_metadata_in_a_distribution_directory() {
        assert_eq!(
            python_metadata_name(Path::new("/usr/lib/python/site-packages/demo.dist-info/METADATA")).unwrap(),
            Some("METADATA")
        );
        assert_eq!(
            python_metadata_name(Path::new("/usr/lib/python/site-packages/demo.egg-info/PKG-INFO")).unwrap(),
            Some("PKG-INFO")
        );
        assert_eq!(python_metadata_name(Path::new("/tmp/METADATA")).unwrap(), None);
    }

    #[test]
    fn candidate_without_parent_is_a_structured_error() {
        assert!(matches!(
            python_metadata_name(Path::new("METADATA")),
            Err(PythonMetadataError::MissingParent { .. })
        ));
    }

    #[test]
    fn missing_name_is_a_structured_error() {
        let mail = parse_mail(b"Version: 1.0\n").unwrap();
        assert!(matches!(
            unique_python_name(&mail, Path::new("/demo.dist-info/METADATA")),
            Err(PythonMetadataError::MissingName { .. })
        ));
    }

    #[test]
    fn duplicate_names_are_rejected_even_when_the_values_match() {
        let conflicting = parse_mail(b"Name: demo\nName: other\nVersion: 1.0\n").unwrap();
        assert!(matches!(
            unique_python_name(&conflicting, Path::new("/demo.dist-info/METADATA")),
            Err(PythonMetadataError::DuplicateName { count: 2, .. })
        ));

        let repeated = parse_mail(b"Name: demo\nName: demo\nVersion: 1.0\n").unwrap();
        assert!(matches!(
            unique_python_name(&repeated, Path::new("/demo.dist-info/METADATA")),
            Err(PythonMetadataError::DuplicateName { count: 2, .. })
        ));
    }
}
