use std::{error::Error, fmt, io, path::PathBuf};

use declarative_config::Diagnostic;

use super::RootDeclarationDiscoveryError;

/// Authority checkpoint that rejected a fixed-root declaration load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedRootRevalidationPhase {
    BeforeRead,
    AfterRead,
    AfterEvaluation,
}

impl fmt::Display for FixedRootRevalidationPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BeforeRead => "before source read",
            Self::AfterRead => "after source read",
            Self::AfterEvaluation => "after evaluation",
        })
    }
}

/// Failure to prove that a retained directory and fixed slot are unchanged.
#[derive(Debug)]
pub enum FixedRootAuthorityError {
    VerifySourceRoot {
        source: Diagnostic,
    },
    InspectDirectory {
        path: PathBuf,
        source: io::Error,
    },
    DirectoryChanged {
        path: PathBuf,
    },
    DiscoverSlot {
        source: RootDeclarationDiscoveryError,
    },
    SlotChanged {
        logical_name: String,
        expected: Option<PathBuf>,
        actual: Option<PathBuf>,
    },
    InspectDeclaration {
        path: PathBuf,
        source: io::Error,
    },
    DeclarationChanged {
        path: PathBuf,
    },
}

impl fmt::Display for FixedRootAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerifySourceRoot { source } => {
                write!(formatter, "verify retained source root: {source}")
            }
            Self::InspectDirectory { path, .. } => {
                write!(formatter, "inspect fixed declaration directory {}", path.display())
            }
            Self::DirectoryChanged { path } => write!(
                formatter,
                "fixed declaration directory {} changed",
                path.display()
            ),
            Self::DiscoverSlot { source } => {
                write!(formatter, "rediscover fixed declaration slot: {source}")
            }
            Self::SlotChanged {
                logical_name,
                expected,
                actual,
            } => write!(
                formatter,
                "fixed declaration slot {logical_name:?} changed from {} to {}",
                display_optional_path(expected.as_ref()),
                display_optional_path(actual.as_ref())
            ),
            Self::InspectDeclaration { path, .. } => {
                write!(formatter, "inspect retained declaration {}", path.display())
            }
            Self::DeclarationChanged { path } => {
                write!(formatter, "retained declaration {} changed", path.display())
            }
        }
    }
}

impl Error for FixedRootAuthorityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::VerifySourceRoot { source } => Some(source),
            Self::InspectDirectory { source, .. }
            | Self::InspectDeclaration { source, .. } => Some(source),
            Self::DiscoverSlot { source } => Some(source),
            Self::DirectoryChanged { .. }
            | Self::SlotChanged { .. }
            | Self::DeclarationChanged { .. } => None,
        }
    }
}

/// Failure while retaining, discovering, or evaluating one fixed declaration.
#[derive(Debug)]
pub enum LoadFixedRootDeclarationError<E> {
    OpenDirectory {
        path: PathBuf,
        source: io::Error,
    },
    RetainSourceRoot {
        path: PathBuf,
        source: Diagnostic,
    },
    Discovery {
        directory: PathBuf,
        source: RootDeclarationDiscoveryError,
    },
    RetainDeclaration {
        path: PathBuf,
        source: io::Error,
    },
    UnregisteredLanguage {
        path: PathBuf,
        extension: String,
    },
    Revalidation {
        directory: PathBuf,
        phase: FixedRootRevalidationPhase,
        source: FixedRootAuthorityError,
    },
    Read {
        path: PathBuf,
        source: Diagnostic,
    },
    Evaluation {
        path: PathBuf,
        source: Diagnostic,
    },
    Conversion {
        path: PathBuf,
        source: E,
    },
}

impl<E: fmt::Display> fmt::Display for LoadFixedRootDeclarationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenDirectory { path, .. } => {
                write!(formatter, "open fixed declaration directory {}", path.display())
            }
            Self::RetainSourceRoot { path, source } => write!(
                formatter,
                "retain fixed declaration source root {}: {source}",
                path.display()
            ),
            Self::Discovery { directory, source } => write!(
                formatter,
                "discover fixed declaration beneath {}: {source}",
                directory.display()
            ),
            Self::RetainDeclaration { path, .. } => {
                write!(formatter, "retain fixed declaration {}", path.display())
            }
            Self::UnregisteredLanguage { path, extension } => write!(
                formatter,
                "fixed declaration {} selected unregistered extension {extension:?}",
                path.display()
            ),
            Self::Revalidation {
                directory,
                phase,
                source,
            } => write!(
                formatter,
                "revalidate fixed declaration beneath {} {phase}: {source}",
                directory.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "read fixed declaration {}: {source}", path.display())
            }
            Self::Evaluation { path, source } => {
                write!(formatter, "evaluate fixed declaration {}: {source}", path.display())
            }
            Self::Conversion { path, source } => {
                write!(formatter, "convert fixed declaration {}: {source}", path.display())
            }
        }
    }
}

impl<E> Error for LoadFixedRootDeclarationError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OpenDirectory { source, .. }
            | Self::RetainDeclaration { source, .. } => Some(source),
            Self::RetainSourceRoot { source, .. }
            | Self::Read { source, .. }
            | Self::Evaluation { source, .. } => Some(source),
            Self::Discovery { source, .. } => Some(source),
            Self::Revalidation { source, .. } => Some(source),
            Self::Conversion { source, .. } => Some(source),
            Self::UnregisteredLanguage { .. } => None,
        }
    }
}

fn display_optional_path(path: Option<&PathBuf>) -> String {
    path.map_or_else(|| "absence".to_owned(), |path| path.display().to_string())
}
