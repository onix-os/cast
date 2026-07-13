// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Deterministic discovery and hashing for the OS Tools implementation input.
//!
//! This module is compiled by `build.rs`.  The library itself only exposes the
//! resulting build-time value, so hashing is not repeated at runtime.

use std::{
    collections::BTreeMap,
    fs::Metadata,
    io,
    path::{Component, Path, PathBuf},
};

use fs_err::{self as fs, DirEntry};
use sha2::{Digest, Sha256};

const HASH_DOMAIN: &[u8] = b"org.aerynos.os-tools.semantic-build.v1";
const FINGERPRINT_PREFIX: &str = "sha256:";

/// Inputs supplied by Cargo and the compiler rather than stored in the source
/// tree.  Names are part of the hash and must describe a stable semantic role,
/// not an absolute host path.
pub(crate) type ExplicitInput = (String, Vec<u8>);

#[derive(Debug)]
struct SourceInput {
    relative_path: String,
    kind: SourceKind,
    executable: bool,
    link_target: Option<String>,
    contents: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum SourceKind {
    File,
    Symlink,
}

impl SourceKind {
    const fn tag(self) -> u8 {
        match self {
            Self::File => 0,
            Self::Symlink => 1,
        }
    }
}

/// The build-time fingerprint and the paths Cargo must monitor to reproduce
/// it after source additions, removals, or edits.
#[derive(Debug)]
pub(crate) struct SemanticFingerprint {
    value: String,
    watched_paths: Vec<PathBuf>,
    #[cfg(test)]
    relative_paths: Vec<String>,
}

impl SemanticFingerprint {
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn watched_paths(&self) -> &[PathBuf] {
        &self.watched_paths
    }

    #[cfg(test)]
    pub(crate) fn relative_paths(&self) -> &[String] {
        &self.relative_paths
    }
}

/// Hash every production-relevant source and configuration input in `root`.
///
/// Discovery is deliberately independent of Git so dirty worktrees and source
/// archives have exactly the same semantics.  Production package roots are
/// narrow: package manifests, build scripts/configuration, and the `src`,
/// `gluon`, and `data` trees.  Documentation, examples, benches, fixtures, and
/// generated output trees therefore cannot perturb the implementation ID.
pub(crate) fn calculate(
    root: &Path,
    explicit_inputs: impl IntoIterator<Item = ExplicitInput>,
) -> io::Result<SemanticFingerprint> {
    let root = root.canonicalize()?;
    let mut source_inputs = Vec::new();
    let mut watched_paths = Vec::new();

    collect_required_file(&root, Path::new("Cargo.toml"), &mut source_inputs, &mut watched_paths)?;
    collect_required_file(&root, Path::new("Cargo.lock"), &mut source_inputs, &mut watched_paths)?;

    for path in [
        "flake.nix",
        "flake.lock",
        "Makefile",
        "rust-toolchain",
        "rust-toolchain.toml",
    ] {
        collect_optional_file(&root, Path::new(path), &mut source_inputs, &mut watched_paths)?;
    }

    collect_optional_tree(&root, Path::new(".cargo"), &mut source_inputs, &mut watched_paths)?;
    collect_package_group(&root, Path::new("bin"), &mut source_inputs, &mut watched_paths)?;
    collect_package_group(&root, Path::new("crates"), &mut source_inputs, &mut watched_paths)?;

    source_inputs.sort_by(|left, right| left.relative_path.as_bytes().cmp(right.relative_path.as_bytes()));
    reject_duplicate_source_paths(&source_inputs)?;

    let explicit_inputs = normalize_explicit_inputs(explicit_inputs)?;
    let value = hash_inputs(&source_inputs, &explicit_inputs);
    #[cfg(test)]
    let relative_paths = source_inputs.iter().map(|input| input.relative_path.clone()).collect();

    watched_paths.sort();
    watched_paths.dedup();

    Ok(SemanticFingerprint {
        value,
        watched_paths,
        #[cfg(test)]
        relative_paths,
    })
}

fn collect_package_group(
    root: &Path,
    relative_group: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let group = root.join(relative_group);
    watched_paths.push(group.clone());
    let mut packages = read_dir_sorted(&group)?;

    for entry in packages.drain(..) {
        if !entry.file_type()?.is_dir() || is_excluded_directory(&entry.path()) {
            continue;
        }

        let package = entry.path();
        watched_paths.push(package.clone());
        let relative_package = package.strip_prefix(root).map_err(invalid_path)?;
        collect_required_file(root, &relative_package.join("Cargo.toml"), inputs, watched_paths)?;

        for file in ["build.rs", "cbindgen.toml"] {
            collect_optional_file(root, &relative_package.join(file), inputs, watched_paths)?;
        }
        for directory in ["src", "gluon", "data", "build"] {
            collect_optional_tree(root, &relative_package.join(directory), inputs, watched_paths)?;
        }
    }

    Ok(())
}

fn collect_required_file(
    root: &Path,
    relative_path: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let path = root.join(relative_path);
    watched_paths.push(path.clone());
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("required semantic build input is missing: {}", relative_path.display()),
        ));
    }
    collect_path(root, &path, inputs, watched_paths)
}

fn collect_optional_file(
    root: &Path,
    relative_path: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let path = root.join(relative_path);
    watched_paths.push(path.clone());
    if path.exists() {
        collect_path(root, &path, inputs, watched_paths)?;
    }
    Ok(())
}

fn collect_optional_tree(
    root: &Path,
    relative_path: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let path = root.join(relative_path);
    watched_paths.push(path.clone());
    if path.exists() {
        collect_tree(root, &path, inputs, watched_paths)?;
    }
    Ok(())
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in read_dir_sorted(directory)? {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if is_excluded_directory(&path) {
                continue;
            }
            collect_tree(root, &path, inputs, watched_paths)?;
        } else {
            collect_path(root, &path, inputs, watched_paths)?;
        }
    }
    Ok(())
}

fn collect_path(
    root: &Path,
    path: &Path,
    inputs: &mut Vec<SourceInput>,
    watched_paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let relative_path = stable_relative_path(root, path)?;
    let metadata = fs::symlink_metadata(path)?;
    watched_paths.push(path.to_owned());

    if metadata.file_type().is_symlink() {
        let raw_target = fs::read_link(path)?;
        if raw_target.is_absolute() {
            return Err(invalid_data(format!(
                "semantic input {relative_path} uses an absolute symlink target"
            )));
        }
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(root) {
            return Err(invalid_data(format!(
                "semantic input {relative_path} resolves outside the repository"
            )));
        }
        let resolved_metadata = fs::metadata(&resolved)?;
        if !resolved_metadata.is_file() {
            return Err(invalid_data(format!(
                "semantic input {relative_path} must resolve to a regular file"
            )));
        }
        watched_paths.push(resolved.clone());
        inputs.push(SourceInput {
            relative_path,
            kind: SourceKind::Symlink,
            executable: is_executable(&resolved_metadata),
            link_target: Some(stable_relative_path(root, &resolved)?),
            contents: fs::read(resolved)?,
        });
    } else if metadata.is_file() {
        inputs.push(SourceInput {
            relative_path,
            kind: SourceKind::File,
            executable: is_executable(&metadata),
            link_target: None,
            contents: fs::read(path)?,
        });
    } else {
        return Err(invalid_data(format!(
            "semantic input {relative_path} is neither a regular file nor a symlink"
        )));
    }

    Ok(())
}

fn stable_relative_path(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix(root).map_err(invalid_path)?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(
                value
                    .to_str()
                    .ok_or_else(|| invalid_data(format!("semantic input path is not UTF-8: {}", relative.display())))?,
            ),
            _ => {
                return Err(invalid_data(format!(
                    "semantic input path is not normalized: {}",
                    relative.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(invalid_data("the repository root cannot be a semantic input"));
    }
    Ok(parts.join("/"))
}

fn read_dir_sorted(path: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn is_excluded_directory(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| matches!(name.to_str(), Some(".git" | ".direnv" | "target")))
}

#[cfg(unix)]
fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &Metadata) -> bool {
    false
}

fn normalize_explicit_inputs(inputs: impl IntoIterator<Item = ExplicitInput>) -> io::Result<Vec<ExplicitInput>> {
    let mut normalized = BTreeMap::new();
    for (name, value) in inputs {
        if name.is_empty() {
            return Err(invalid_data("semantic explicit input names cannot be empty"));
        }
        if normalized.insert(name.clone(), value).is_some() {
            return Err(invalid_data(format!("duplicate semantic explicit input: {name}")));
        }
    }
    Ok(normalized.into_iter().collect())
}

fn reject_duplicate_source_paths(inputs: &[SourceInput]) -> io::Result<()> {
    if let Some(pair) = inputs
        .windows(2)
        .find(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(invalid_data(format!(
            "duplicate semantic source input: {}",
            pair[0].relative_path
        )));
    }
    Ok(())
}

fn hash_inputs(source_inputs: &[SourceInput], explicit_inputs: &[ExplicitInput]) -> String {
    let mut hasher = Sha256::new();
    write_field(&mut hasher, HASH_DOMAIN);
    write_count(&mut hasher, source_inputs.len());
    for input in source_inputs {
        write_field(&mut hasher, input.relative_path.as_bytes());
        write_field(&mut hasher, &[input.kind.tag()]);
        write_field(&mut hasher, &[u8::from(input.executable)]);
        write_field(&mut hasher, input.link_target.as_deref().unwrap_or_default().as_bytes());
        write_field(&mut hasher, &input.contents);
    }

    write_count(&mut hasher, explicit_inputs.len());
    for (name, value) in explicit_inputs {
        write_field(&mut hasher, name.as_bytes());
        write_field(&mut hasher, value);
    }

    let digest = hasher.finalize();
    let mut value = String::with_capacity(FINGERPRINT_PREFIX.len() + digest.len() * 2);
    value.push_str(FINGERPRINT_PREFIX);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

fn write_count(hasher: &mut Sha256, count: usize) {
    let count = u64::try_from(count).expect("semantic input count fits in u64");
    hasher.update(count.to_be_bytes());
}

fn write_field(hasher: &mut Sha256, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("semantic input length fits in u64");
    hasher.update(length.to_be_bytes());
    hasher.update(value);
}

fn invalid_path(error: impl std::fmt::Display) -> io::Error {
    invalid_data(error.to_string())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
