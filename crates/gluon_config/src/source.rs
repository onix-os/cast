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
        self.load_inner(relative.as_ref(), max_bytes, LimitKind::SourceSize, false)
    }

    pub(crate) fn load_import(&self, relative: &Path, max_bytes: usize) -> Result<Source, Diagnostic> {
        self.load_inner(relative, max_bytes, LimitKind::ImportedFileSize, true)
    }

    fn load_inner(
        &self,
        relative: &Path,
        max_bytes: usize,
        limit_kind: LimitKind,
        is_import: bool,
    ) -> Result<Source, Diagnostic> {
        let relative = normalize_relative(relative, is_import)?;
        let requested_name = relative.to_string_lossy().replace('\\', "/");
        let candidate = self.canonical.join(&relative);
        let canonical = candidate.canonicalize().map_err(|error| {
            if is_import {
                Diagnostic::import(
                    Some(requested_name.clone()),
                    format!("configuration import cannot be loaded: {error}"),
                )
            } else {
                Diagnostic::io(Some(requested_name.clone()), error)
            }
        })?;
        if !canonical.starts_with(&self.canonical) {
            let message = "source path escapes the configured source root";
            return Err(if is_import {
                Diagnostic::import(Some(requested_name), message)
            } else {
                Diagnostic::io(
                    Some(requested_name),
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
                )
            });
        }

        let logical_name = canonical
            .strip_prefix(&self.canonical)
            .map_err(|_| Diagnostic::internal("contained source lost its source-root prefix"))?
            .to_string_lossy()
            .replace('\\', "/");
        if !canonical.is_file() {
            let message = "source path is not a regular file";
            return Err(if is_import {
                Diagnostic::import(Some(logical_name), message)
            } else {
                Diagnostic::io(
                    Some(logical_name),
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
                )
            });
        }

        let mut file = File::open(&canonical).map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        if bytes.len() > max_bytes {
            return Err(Diagnostic::limit(
                limit_kind,
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

fn normalize_relative(path: &Path, is_import: bool) -> Result<PathBuf, Diagnostic> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                let source_name = Some(path.display().to_string());
                let message = "source path must be relative and cannot contain parent traversal";
                return Err(if is_import {
                    Diagnostic::import(source_name, message)
                } else {
                    Diagnostic::io(
                        source_name,
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
                    )
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        let source_name = Some(path.display().to_string());
        return Err(if is_import {
            Diagnostic::import(source_name, "source path is empty")
        } else {
            Diagnostic::io(
                source_name,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path is empty"),
            )
        });
    }
    Ok(normalized)
}
