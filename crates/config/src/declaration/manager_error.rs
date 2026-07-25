use std::{error::Error, fmt, path::PathBuf};

use declarative_config::Diagnostic;

use super::{
    DeleteDeclarationError, FragmentDeclarationSetError,
    GeneratedDeclarationSlotError, SaveDeclarationError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationRevalidationPhase {
    BeforeRead,
    AfterRead,
    BeforeEvaluation,
    AfterEvaluation,
}

impl fmt::Display for DeclarationRevalidationPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BeforeRead => "before source read",
            Self::AfterRead => "after source read",
            Self::BeforeEvaluation => "before evaluation",
            Self::AfterEvaluation => "after evaluation",
        })
    }
}

/// Failure while discovering, reading, or evaluating a typed declaration set.
/// Engine diagnostics and domain-conversion failures remain separate variants.
#[derive(Debug)]
pub enum LoadManagedDeclarationError<E> {
    Discovery {
        source: FragmentDeclarationSetError,
    },
    UnregisteredLanguage {
        path: PathBuf,
        extension: String,
    },
    Revalidation {
        path: PathBuf,
        phase: DeclarationRevalidationPhase,
        source: FragmentDeclarationSetError,
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

impl<E: fmt::Display> fmt::Display for LoadManagedDeclarationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery { source } => {
                write!(formatter, "discover declaration fragments: {source}")
            }
            Self::UnregisteredLanguage { path, extension } => write!(
                formatter,
                "declaration {} selected unregistered extension {extension:?}",
                path.display()
            ),
            Self::Revalidation {
                path,
                phase,
                source,
            } => write!(
                formatter,
                "revalidate declaration {} {phase}: {source}",
                path.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "read declaration {}: {source}", path.display())
            }
            Self::Evaluation { path, source } => write!(
                formatter,
                "evaluate declaration {}: {source}",
                path.display()
            ),
            Self::Conversion { path, source } => write!(
                formatter,
                "convert declaration {}: {source}",
                path.display()
            ),
        }
    }
}

impl<E> Error for LoadManagedDeclarationError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery { source } | Self::Revalidation { source, .. } => {
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

/// Failure while canonically encoding or atomically saving a declaration.
#[derive(Debug)]
pub enum SaveManagedDeclarationError<E> {
    Conversion { source: E },
    SlotPolicy {
        source: GeneratedDeclarationSlotError,
    },
    Storage { source: SaveDeclarationError },
}

impl<E: fmt::Display> fmt::Display for SaveManagedDeclarationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion { source } => {
                write!(formatter, "encode generated declaration: {source}")
            }
            Self::SlotPolicy { source } => source.fmt(formatter),
            Self::Storage { source } => source.fmt(formatter),
        }
    }
}

impl<E> Error for SaveManagedDeclarationError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conversion { source } => Some(source),
            Self::SlotPolicy { source } => Some(source),
            Self::Storage { source } => Some(source),
        }
    }
}

/// Failure while deleting a declaration selected by an explicit adapter.
#[derive(Debug)]
pub enum DeleteManagedDeclarationError {
    SlotPolicy {
        source: GeneratedDeclarationSlotError,
    },
    Storage { source: DeleteDeclarationError },
}

impl fmt::Display for DeleteManagedDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SlotPolicy { source } => source.fmt(formatter),
            Self::Storage { source } => source.fmt(formatter),
        }
    }
}

impl Error for DeleteManagedDeclarationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SlotPolicy { source } => Some(source),
            Self::Storage { source } => Some(source),
        }
    }
}
