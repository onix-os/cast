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

/// Network transport policy for one declared source URL.
///
/// This distinction is intentionally smaller than the set of schemes the URL
/// parser and source backends happen to understand. Archive downloads are
/// HTTPS-only. Git repositories may use HTTPS or SSH, but never an implicit
/// local-file transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUrlKind {
    Archive,
    Git,
}

/// Parse and enforce the production transport policy for a source URL.
///
/// Every authored source, generated source lock, and frozen source must pass
/// through this function. It deliberately validates only the declared URL.
/// Redirect targets and resolved network addresses require separate checks in
/// the fetching backend; this function is not an SSRF or redirect-isolation
/// boundary.
pub fn validate_source_url(kind: SourceUrlKind, value: &str) -> Result<Url, SourceUrlValidationError> {
    let url = Url::parse(value).map_err(SourceUrlValidationError::InvalidSyntax)?;

    if !url.username().is_empty() || url.password().is_some() {
        return Err(SourceUrlValidationError::EmbeddedCredentials);
    }
    if url.fragment().is_some() {
        return Err(SourceUrlValidationError::Fragment);
    }

    let scheme_allowed = match kind {
        SourceUrlKind::Archive => url.scheme() == "https",
        SourceUrlKind::Git => matches!(url.scheme(), "https" | "ssh"),
    };
    if !scheme_allowed {
        return Err(SourceUrlValidationError::UnsupportedScheme {
            scheme: url.scheme().to_owned(),
            expected: match kind {
                SourceUrlKind::Archive => "https",
                SourceUrlKind::Git => "https or ssh",
            },
        });
    }
    if url.host_str().is_none() {
        return Err(SourceUrlValidationError::MissingHost);
    }

    Ok(url)
}

/// Failure to parse or admit one production source URL.
///
/// Errors never retain or render the full input because URLs commonly contain
/// credentials or other secret-bearing data.
#[derive(Debug, Error)]
pub enum SourceUrlValidationError {
    #[error("URL syntax is invalid")]
    InvalidSyntax(#[source] url::ParseError),
    #[error("URL must not contain embedded credentials")]
    EmbeddedCredentials,
    #[error("URL fragments are not supported")]
    Fragment,
    #[error("URL scheme `{scheme}` is not supported; expected {expected}")]
    UnsupportedScheme { scheme: String, expected: &'static str },
    #[error("URL must include a remote host")]
    MissingHost,
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

    pub fn validated_url(&self) -> Result<Url, UpstreamValidationError> {
        let (kind, value) = match self {
            Self::Archive { url, .. } => (SourceUrlKind::Archive, url),
            Self::Git { url, .. } => (SourceUrlKind::Git, url),
        };
        validate_source_url(kind, value).map_err(|source| UpstreamValidationError::InvalidUrl { source })
    }
}

/// Failure to validate one authored source request.
#[derive(Debug, Error)]
pub enum UpstreamValidationError {
    #[error("invalid source URL: {source}")]
    InvalidUrl {
        #[source]
        source: SourceUrlValidationError,
    },
    #[error("expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`")]
    InvalidArchiveSha256 { value: String },
    #[error("`{value}` must be one normalized filename component")]
    InvalidMaterializationComponent { field: &'static str, value: String },
    #[error("URL does not end in a safe filename component; set `{override_field}` explicitly")]
    InvalidDefaultMaterializationName { override_field: &'static str },
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
        return Err(UpstreamValidationError::InvalidDefaultMaterializationName { override_field });
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

    #[test]
    fn source_url_policy_admits_only_explicit_secure_remote_transports() {
        assert!(validate_source_url(SourceUrlKind::Archive, "https://example.invalid/source.tar.zst").is_ok());
        assert!(validate_source_url(SourceUrlKind::Git, "https://example.invalid/project.git").is_ok());
        assert!(validate_source_url(SourceUrlKind::Git, "ssh://example.invalid/project.git").is_ok());

        for value in [
            "http://example.invalid/source.tar.zst",
            "ssh://example.invalid/source.tar.zst",
            "file:///tmp/source.tar.zst",
            "ftp://example.invalid/source.tar.zst",
        ] {
            assert!(matches!(
                validate_source_url(SourceUrlKind::Archive, value),
                Err(SourceUrlValidationError::UnsupportedScheme { .. })
            ));
        }

        for value in [
            "http://example.invalid/project.git",
            "git://example.invalid/project.git",
            "file:///tmp/project.git",
            "ftp://example.invalid/project.git",
        ] {
            assert!(matches!(
                validate_source_url(SourceUrlKind::Git, value),
                Err(SourceUrlValidationError::UnsupportedScheme { .. })
            ));
        }
    }

    #[test]
    fn source_url_policy_rejects_secret_bearing_and_ambiguous_components_without_echoing_them() {
        let credential_url = "https://user:do-not-print@example.invalid/source.tar.zst";
        let error = validate_source_url(SourceUrlKind::Archive, credential_url).unwrap_err();
        assert!(matches!(error, SourceUrlValidationError::EmbeddedCredentials));
        assert!(!error.to_string().contains("user"));
        assert!(!error.to_string().contains("do-not-print"));

        let fragment_url = "https://example.invalid/source.tar.zst#secret-fragment";
        let error = validate_source_url(SourceUrlKind::Archive, fragment_url).unwrap_err();
        assert!(matches!(error, SourceUrlValidationError::Fragment));
        assert!(!error.to_string().contains("secret-fragment"));

        assert!(matches!(
            validate_source_url(SourceUrlKind::Git, "ssh:/project.git"),
            Err(SourceUrlValidationError::MissingHost)
        ));
        assert!(matches!(
            validate_source_url(SourceUrlKind::Git, "not a URL"),
            Err(SourceUrlValidationError::InvalidSyntax(_))
        ));
    }
}
