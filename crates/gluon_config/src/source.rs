// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

use fs_err::File;

use crate::{Diagnostic, LimitKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    logical_name: String,
    text: String,
}

impl Source {
    pub fn new(logical_name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            logical_name: logical_name.into(),
            text: text.into(),
        }
    }

    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRoot {
    canonical: PathBuf,
}

impl SourceRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, Diagnostic> {
        let canonical = path
            .as_ref()
            .canonicalize()
            .map_err(|error| Diagnostic::io(Some(path.as_ref().display().to_string()), error))?;
        if !canonical.is_dir() {
            return Err(Diagnostic::io(
                Some(canonical.display().to_string()),
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "source root is not a directory"),
            ));
        }
        Ok(Self { canonical })
    }

    pub fn path(&self) -> &Path {
        &self.canonical
    }

    pub fn load(&self, relative: impl AsRef<Path>, max_bytes: usize) -> Result<Source, Diagnostic> {
        let relative = normalize_relative(relative.as_ref())?;
        let logical_name = relative.to_string_lossy().replace('\\', "/");
        let candidate = self.canonical.join(&relative);
        let canonical = candidate
            .canonicalize()
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        if !canonical.starts_with(&self.canonical) {
            return Err(Diagnostic::io(
                Some(logical_name),
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source path escapes the configured source root",
                ),
            ));
        }

        let mut file = File::open(&canonical).map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        if bytes.len() > max_bytes {
            return Err(Diagnostic::limit(
                LimitKind::SourceSize,
                Some(logical_name),
                format!("source exceeds the {max_bytes}-byte limit"),
            ));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            Diagnostic::io(
                Some(logical_name.clone()),
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        Ok(Source::new(logical_name, text))
    }
}

fn normalize_relative(path: &Path) -> Result<PathBuf, Diagnostic> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Diagnostic::io(
                    Some(path.display().to_string()),
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "source path must be relative and cannot contain parent traversal",
                    ),
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(Diagnostic::io(
            Some(path.display().to_string()),
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path is empty"),
        ));
    }
    Ok(normalized)
}
