use std::{error::Error, fmt, path::PathBuf};

use declarative_config::Diagnostic;

use super::RootedFragmentDeclarationSetError;

/// The retained-authority checkpoint that rejected a rooted load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootedDeclarationRevalidationPhase {
    BeforeRead,
    AfterRead,
    BeforeEvaluation,
    AfterEvaluation,
    FinalSet,
}

impl fmt::Display for RootedDeclarationRevalidationPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BeforeRead => "before source read",
            Self::AfterRead => "after source read",
            Self::BeforeEvaluation => "before evaluation",
            Self::AfterEvaluation => "after evaluation",
            Self::FinalSet => "after all evaluations",
        })
    }
}

/// Failure while discovering or evaluating one descriptor-rooted declaration
/// set. Engine diagnostics and typed conversion failures remain distinct.
#[derive(Debug)]
pub enum LoadRootedDeclarationsError<E> {
    Discovery {
        root_path: PathBuf,
        source: RootedFragmentDeclarationSetError,
    },
    UnregisteredLanguage {
        path: PathBuf,
        extension: String,
    },
    Revalidation {
        path: PathBuf,
        phase: RootedDeclarationRevalidationPhase,
        source: RootedFragmentDeclarationSetError,
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

impl<E: fmt::Display> fmt::Display for LoadRootedDeclarationsError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery { root_path, source } => write!(
                formatter,
                "discover declarations beneath retained root {}: {source}",
                root_path.display()
            ),
            Self::UnregisteredLanguage { path, extension } => write!(
                formatter,
                "rooted declaration {} selected unregistered extension {extension:?}",
                path.display()
            ),
            Self::Revalidation {
                path,
                phase,
                source,
            } => write!(
                formatter,
                "revalidate rooted declaration authority for {} {phase}: {source}",
                path.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "read rooted declaration {}: {source}", path.display())
            }
            Self::Evaluation { path, source } => write!(
                formatter,
                "evaluate rooted declaration {}: {source}",
                path.display()
            ),
            Self::Conversion { path, source } => write!(
                formatter,
                "convert rooted declaration {}: {source}",
                path.display()
            ),
        }
    }
}

impl<E> Error for LoadRootedDeclarationsError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery { source, .. } | Self::Revalidation { source, .. } => {
                Some(source)
            }
            Self::Read { source, .. } | Self::Evaluation { source, .. } => {
                Some(source)
            }
            Self::Conversion { source, .. } => Some(source),
            Self::UnregisteredLanguage { .. } => None,
        }
    }
}
