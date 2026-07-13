// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use fs_err as fs;
use regex::Regex;
use stone::relation::{Dependency, Kind, Provider};

use crate::package::collect::PathInfo;

use mailparse::{MailHeaderMap, parse_mail};

use super::{BoxError, BucketMut, Decision, Response, analyzer_command, checked_output};

pub fn python(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_path = info.path.clone().into_os_string().into_string().unwrap_or_default();
    let is_dist_info = file_path.contains(".dist-info") && info.file_name().ends_with("METADATA");
    let is_egg_info = file_path.contains(".egg-info") && info.file_name().ends_with("PKG-INFO");

    if !(is_dist_info || is_egg_info) {
        return Ok(Decision::NextHandler.into());
    }

    let data = fs::read(&info.path)?;
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

    let mut command = analyzer_command("/usr/bin/python3");
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

/* Normalize name per https://peps.python.org/pep-0503/#normalized-names, replacing
all runs of `_` and `.` with `-` and lowercaseing */
fn pep_503_normalize(input: &str) -> Result<String, BoxError> {
    let re = Regex::new(r"[-_.]+")?;

    Ok(re.replace_all(input, "-").to_lowercase())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_normalization() {
        assert_eq!(pep_503_normalize("PyThOn-_-foo").unwrap(), "python-foo");
        assert_eq!(pep_503_normalize("PyThOn.-f-oo").unwrap(), "python-f-oo");
    }
}
