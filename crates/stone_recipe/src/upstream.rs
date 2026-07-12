// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{borrow::Borrow, fmt::Display, path::PathBuf, str::FromStr};

use url::Url;

/// Prefix applied to URLs to report they point to a Git repository.
pub static GIT_PREFIX: &str = "git|";

#[derive(Debug, Clone)]
pub struct Upstream {
    pub url: Url,
    pub props: Props,
}

/// Supported kinds of upstream in a recipe.
#[derive(Clone, Debug, Eq, PartialOrd, Ord, PartialEq)]
pub enum Kind {
    /// The upstream is an archive, typically a tarball.
    Archive,
    /// The upstream is a git repository.
    Git,
}

/// A URI from where to download source code.
/// The URI is a combination of a URL plus the kind of
/// upstream, that instructs downloaders how to fetch
/// the resource.
///
/// ### String representation
///
/// In the case of an archive, a regular URL is used.
///
/// For a git repository, the URI is parsed and unparsed
/// with the format `git|<regular_url>`.
#[derive(Clone, Debug, Eq, PartialOrd, Ord, PartialEq)]
pub struct SourceUri {
    /// The kind of source the URL refers to.
    pub kind: Kind,
    /// Location of the source.
    pub url: Url,
}

impl FromStr for SourceUri {
    type Err = url::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(git_url) = s.strip_prefix(GIT_PREFIX) {
            Ok(SourceUri {
                kind: Kind::Git,
                url: git_url.parse()?,
            })
        } else {
            Ok(SourceUri {
                kind: Kind::Archive,
                url: s.parse()?,
            })
        }
    }
}

impl TryFrom<&str> for SourceUri {
    type Error = url::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::from_str(value)
    }
}

impl Display for SourceUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Kind::Archive => write!(f, "{}", self.url.as_str()),
            Kind::Git => {
                write!(f, "{GIT_PREFIX}{}", self.url.as_str())
            }
        }
    }
}

impl From<SourceUri> for Url {
    fn from(value: SourceUri) -> Self {
        value.url
    }
}

impl Borrow<Url> for SourceUri {
    fn borrow(&self) -> &Url {
        &self.url
    }
}

#[derive(Clone, Debug)]
pub enum Props {
    Plain {
        hash: String,
        rename: Option<String>,
        strip_dirs: Option<u8>,
        unpack: bool,
        unpack_dir: Option<PathBuf>,
    },
    Git {
        git_ref: String,
        clone_dir: Option<PathBuf>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    static SRC_URL: &str = "https://example.com/source";

    #[test]
    fn parse_archive() -> Result<(), url::ParseError> {
        let src: SourceUri = SRC_URL.parse()?;
        assert_eq!(
            src,
            SourceUri {
                kind: Kind::Archive,
                url: Url::from_str(SRC_URL)?
            }
        );
        Ok(())
    }

    #[test]
    fn parse_git() -> Result<(), url::ParseError> {
        let src: SourceUri = format!("{GIT_PREFIX}{SRC_URL}").parse()?;
        assert_eq!(
            src,
            SourceUri {
                kind: Kind::Git,
                url: Url::from_str(SRC_URL)?
            }
        );
        Ok(())
    }
}
