// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Primitive, format-neutral values shared by the package and policy ABIs.

use thiserror::Error;
use url::Url;

/// Recipe-wide build options selected by a concrete package declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsSpec {
    pub toolchain: ToolchainSpec,
    pub cspgo: bool,
    pub samplepgo: bool,
    pub debug: bool,
    pub strip: bool,
    pub networking: bool,
    pub compressman: bool,
    pub lastrip: bool,
}

impl Default for OptionsSpec {
    fn default() -> Self {
        Self {
            toolchain: ToolchainSpec::Llvm,
            cspgo: false,
            samplepgo: false,
            debug: true,
            strip: true,
            networking: false,
            compressman: false,
            lastrip: true,
        }
    }
}

/// One explicitly named package tuning selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTuningSpec {
    pub key: String,
    pub value: TuningSpec,
}

/// An authored source request with its kind encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSpec {
    Archive {
        url: String,
        hash: String,
        rename: Option<String>,
        strip_dirs: Option<i64>,
        unpack: bool,
        unpack_dir: Option<String>,
    },
    Git {
        url: String,
        git_ref: String,
        clone_dir: Option<String>,
    },
}

impl UpstreamSpec {
    /// Validate one authored source request at the typed package boundary.
    ///
    /// This is the canonical source-field validator shared by package
    /// evaluation and any caller which constructs an [`UpstreamSpec`]
    /// directly. I/O-backed resolution may prove that a URL or Git reference
    /// exists, but it must never be the first place malformed authored data is
    /// rejected.
    pub fn validate(&self) -> Result<(), UpstreamValidationError> {
        self.validated_materialization_name().map(drop)
    }

    /// Return the exact build-visible filename or directory after validating
    /// the complete source request.
    pub fn materialization_name(&self) -> Result<String, UpstreamValidationError> {
        self.validated_materialization_name()
    }

    /// Authored field which owns the effective materialization destination.
    pub const fn materialization_field(&self) -> &'static str {
        match self {
            Self::Archive { rename: Some(_), .. } => "rename",
            Self::Git { clone_dir: Some(_), .. } => "clone_dir",
            Self::Archive { rename: None, .. } | Self::Git { clone_dir: None, .. } => "url",
        }
    }

    fn validated_materialization_name(&self) -> Result<String, UpstreamValidationError> {
        let url = self.validated_url()?;

        let materialization_name = match self {
            Self::Archive {
                hash,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
                ..
            } => {
                if !is_canonical_sha256(hash) {
                    return Err(UpstreamValidationError::InvalidArchiveSha256 { value: hash.clone() });
                }

                let materialization_name = validate_materialization_name(&url, rename.as_deref(), "rename")?;

                if let Some(value) = strip_dirs {
                    u8::try_from(*value).map_err(|_| UpstreamValidationError::InvalidStripDirs { value: *value })?;
                    if !unpack {
                        return Err(UpstreamValidationError::OptionRequiresUnpack { field: "strip_dirs" });
                    }
                }

                if let Some(value) = unpack_dir {
                    if !unpack {
                        return Err(UpstreamValidationError::OptionRequiresUnpack { field: "unpack_dir" });
                    }
                    if !is_normalized_relative_path(value) {
                        return Err(UpstreamValidationError::InvalidUnpackDir { value: value.clone() });
                    }
                }

                materialization_name
            }
            Self::Git { git_ref, clone_dir, .. } => {
                if git_ref.is_empty() || git_ref.chars().any(char::is_control) {
                    return Err(UpstreamValidationError::InvalidGitRef { value: git_ref.clone() });
                }
                validate_materialization_name(&url, clone_dir.as_deref(), "clone_dir")?
            }
        };

        Ok(materialization_name)
    }

    fn validated_url(&self) -> Result<Url, UpstreamValidationError> {
        let value = match self {
            Self::Archive { url, .. } | Self::Git { url, .. } => url,
        };
        Url::parse(value).map_err(|source| UpstreamValidationError::InvalidUrl {
            value: value.clone(),
            source,
        })
    }
}

/// Failure to validate one authored source request.
#[derive(Debug, Error)]
pub enum UpstreamValidationError {
    #[error("invalid URL `{value}`")]
    InvalidUrl {
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`")]
    InvalidArchiveSha256 { value: String },
    #[error("`{value}` must be one normalized filename component")]
    InvalidMaterializationComponent { field: &'static str, value: String },
    #[error("URL `{url}` does not end in a safe filename component; set `{override_field}` explicitly")]
    InvalidDefaultMaterializationName { url: String, override_field: &'static str },
    #[error("`{value}` is outside the valid 0..=255 integer range")]
    InvalidStripDirs { value: i64 },
    #[error("option has no effect unless `unpack` is true")]
    OptionRequiresUnpack { field: &'static str },
    #[error("`{value}` must be a normalized, non-empty relative path without `.` or `..` components")]
    InvalidUnpackDir { value: String },
    #[error("Git reference `{value}` must be non-empty and contain no control characters")]
    InvalidGitRef { value: String },
}

impl UpstreamValidationError {
    /// Field relative to one `UpstreamSpec` which caused this error.
    pub const fn field(&self) -> &'static str {
        match self {
            Self::InvalidUrl { .. } | Self::InvalidDefaultMaterializationName { .. } => "url",
            Self::InvalidArchiveSha256 { .. } => "hash",
            Self::InvalidMaterializationComponent { field, .. } | Self::OptionRequiresUnpack { field } => field,
            Self::InvalidStripDirs { .. } => "strip_dirs",
            Self::InvalidUnpackDir { .. } => "unpack_dir",
            Self::InvalidGitRef { .. } => "git_ref",
        }
    }
}

fn validate_materialization_name(
    url: &Url,
    explicit: Option<&str>,
    override_field: &'static str,
) -> Result<String, UpstreamValidationError> {
    if let Some(value) = explicit {
        if !is_safe_artifact_component(value) {
            return Err(UpstreamValidationError::InvalidMaterializationComponent {
                field: override_field,
                value: value.to_owned(),
            });
        }
        return Ok(value.to_owned());
    }

    let value = url_file_name(url);
    if !is_safe_artifact_component(value) {
        return Err(UpstreamValidationError::InvalidDefaultMaterializationName {
            url: url.as_str().to_owned(),
            override_field,
        });
    }
    Ok(value.to_owned())
}

fn url_file_name(url: &Url) -> &str {
    url.path().rsplit('/').next().unwrap_or_default()
}

/// Whether `value` is the canonical textual encoding of one SHA-256 digest.
pub fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Whether `value` is the canonical textual encoding of a full Git object ID.
pub fn is_canonical_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(crate) fn is_safe_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_control)
}

pub(crate) fn is_normalized_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// A package output path with its matching behavior encoded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

/// One explicit package tuning selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuningSpec {
    Enable,
    Disable,
    Config { value: String },
}

/// A supported compiler toolchain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolchainSpec {
    #[default]
    Llvm,
    Gnu,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_defaults_are_explicit_and_stable() {
        let options = OptionsSpec::default();
        assert_eq!(options.toolchain, ToolchainSpec::Llvm);
        assert!(options.debug);
        assert!(options.strip);
        assert!(options.lastrip);
        assert!(!options.networking);
    }

    #[test]
    fn git_commits_have_one_canonical_textual_encoding() {
        assert!(is_canonical_git_commit("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_canonical_git_commit("0123456789ABCDEF0123456789ABCDEF01234567"));
        assert!(!is_canonical_git_commit("01234567"));
    }
}
