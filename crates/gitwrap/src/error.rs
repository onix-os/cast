// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fmt, io, time::Duration};

/// The error type for operations using the `git` executable.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error(#[from] InnerError);

impl Error {
    /// Returns the kind of I/O error if such error happened.
    /// Otherwise, it returns [None].
    ///
    /// Any I/O error is related to calling the `git` executable.
    /// Refer to [Self::run_failed] for I/O errors that occurred within
    /// `git`'s execution.
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        if let InnerError::Io(err) = &self.0 {
            Some(err.kind())
        } else {
            None
        }
    }

    /// Returns whether `git` exited with an error code.
    pub fn run_failed(&self) -> bool {
        matches!(self.0, InnerError::Run { .. })
    }

    /// Returns whether the Git process exceeded its total wall-clock budget.
    pub fn timed_out(&self) -> bool {
        matches!(self.0, InnerError::Timeout { .. })
    }

    /// Returns whether Git exceeded an output, repository-size, or repository-
    /// entry budget.
    pub fn limit_exceeded(&self) -> bool {
        matches!(
            self.0,
            InnerError::OutputLimit { .. }
                | InnerError::ProgressSegmentLimit { .. }
                | InnerError::RepositoryBytes { .. }
                | InnerError::RepositoryEntries { .. }
                | InnerError::RepositoryDepth { .. }
        )
    }

    /// Returns whether a cache-owned mirror's direct origin differed from the
    /// exact origin supplied by its owner.
    pub fn mirror_origin_mismatch(&self) -> bool {
        matches!(self.0, InnerError::MirrorOriginMismatch)
    }

    /// Returns the kind of violated [Constraint] if such error happened.
    /// Otherwise, it returns [None].
    pub fn constraint(&self) -> Option<&Constraint> {
        if let InnerError::Constraint(con) = &self.0 {
            Some(con)
        } else {
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Constraint {
    /// The repository is valid, but it is not bare.
    NotBare,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InnerError {
    /// A generic I/O error occurred.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The `git` executable returned with an error. Git's stderr is
    /// deliberately not included: transports may repeat credential-bearing
    /// source URLs in diagnostics.
    #[error("{}", display_run(code))]
    Run { code: Option<i32> },

    /// The complete subprocess boundary did not finish before its deadline.
    #[error("`git` exceeded its total wall-clock limit of {timeout:?}")]
    Timeout { timeout: Duration },

    /// A captured process stream crossed its byte ceiling.
    #[error("`git` {stream} exceeded its {limit}-byte output limit")]
    OutputLimit { stream: &'static str, limit: usize },

    /// One carriage-return-delimited progress record crossed its byte ceiling.
    #[error("`git` progress record exceeded its {limit}-byte limit")]
    ProgressSegmentLimit { limit: usize },

    /// A repository crossed its combined logical/allocated byte ceiling.
    #[error("Git repository uses {observed} bytes, exceeding the {limit}-byte limit")]
    RepositoryBytes { observed: u64, limit: u64 },

    /// A repository crossed its filesystem-entry ceiling.
    #[error("Git repository contains more than the allowed {limit} filesystem entries")]
    RepositoryEntries { limit: u64 },

    /// Descriptor-rooted traversal crossed its safe nesting ceiling.
    #[error("Git repository nesting exceeds the scanner's {limit}-directory descriptor budget")]
    RepositoryDepth { limit: usize },

    /// A clone destination already exists and therefore cannot be installed
    /// using no-replace semantics.
    #[error("refusing to replace existing Git clone destination")]
    DestinationExists,

    /// A repository root is not an ordinary directory.
    #[error("Git repository root is not an ordinary directory")]
    InvalidRepositoryRoot,

    /// The public path was replaced after its repository inode was opened.
    #[error("Git repository path no longer names the opened repository")]
    RepositoryRootChanged,

    /// A limit set cannot provide a finite live quota/termination boundary.
    #[error("Git quota polling and termination intervals must be non-zero and representable")]
    InvalidLimits,

    /// Only built-in transports with an explicit policy are accepted. Unknown
    /// schemes could dispatch arbitrary `git-remote-*` programs from PATH.
    #[error("Git transport scheme {scheme:?} is not allowed")]
    UnsupportedTransportScheme { scheme: String },

    /// A repository remote did not contain a syntactically valid absolute URL.
    #[error("Git remote URL is not a valid absolute URL")]
    InvalidRemoteUrl,

    /// A cache-owned mirror did not have the exact, finite configuration
    /// required at the network boundary.
    #[error("Git mirror configuration is not canonical or does not match the expected origin")]
    InvalidMirrorConfiguration,

    /// The direct local mirror origin did not equal the caller-owned URL.
    /// Neither URL is included because either may contain credentials.
    #[error("Git mirror origin does not match the expected origin")]
    MirrorOriginMismatch,

    /// A public string argument could be parsed as an option or exceeded the
    /// finite argument boundary.
    #[error("Git {argument} argument is empty, option-like, or too long")]
    InvalidArgument { argument: &'static str },

    /// The private process group could not be proven empty after termination.
    #[error("Git subprocess boundary did not terminate within {timeout:?}")]
    BoundaryTermination { timeout: Duration },

    /// Private clone/fetch state could not be removed after failure.
    #[error("clean up failed Git state: {0}")]
    Cleanup(io::Error),

    #[error(transparent)]
    Constraint(#[from] Constraint),
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBare => write!(f, "this repository is not bare"),
        }
    }
}

fn display_run(code: &Option<i32>) -> String {
    let mut string = String::from("`git` exited ");

    if let Some(code) = code {
        string.push_str(&format!("with code {code}"));
    } else {
        string.push_str("unexpectedly");
    }

    string
}
