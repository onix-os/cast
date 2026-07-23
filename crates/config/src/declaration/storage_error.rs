use std::{error::Error, fmt, io, path::PathBuf};

/// Invalid immutable policy for one generated declaration output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedDeclarationSlotError {
    InvalidName { name: String },
    InvalidOwnershipMarker,
    InvalidTemporaryPrefix { prefix: String },
    ZeroSizeLimit,
}

impl fmt::Display for GeneratedDeclarationSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name } => {
                write!(formatter, "invalid generated declaration name {name:?}")
            }
            Self::InvalidOwnershipMarker => {
                formatter.write_str("generated declaration ownership marker must not be empty")
            }
            Self::InvalidTemporaryPrefix { prefix } => {
                write!(formatter, "invalid generated declaration temporary prefix {prefix:?}")
            }
            Self::ZeroSizeLimit => {
                formatter.write_str("generated declaration size limit must be greater than zero")
            }
        }
    }
}

impl Error for GeneratedDeclarationSlotError {}

#[derive(Debug)]
pub enum SaveDeclarationError {
    CreateDirectory {
        path: PathBuf,
        source: io::Error,
    },
    MissingOwnershipMarker {
        path: PathBuf,
    },
    ReadExisting {
        path: PathBuf,
        source: io::Error,
    },
    AuthoredDeclaration {
        path: PathBuf,
    },
    CreateTemporary {
        path: PathBuf,
        source: io::Error,
    },
    WriteTemporary {
        path: PathBuf,
        source: io::Error,
    },
    SyncTemporary {
        path: PathBuf,
        source: io::Error,
    },
    CleanupTemporary {
        path: PathBuf,
        source: io::Error,
    },
    GeneratedTooLarge {
        size: usize,
        limit: usize,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        source: io::Error,
    },
    SyncDirectory {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for SaveDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDirectory { path, .. } => {
                write!(formatter, "create declaration directory {}", path.display())
            }
            Self::MissingOwnershipMarker { path } => {
                write!(formatter, "generated declaration {} lacks its ownership marker", path.display())
            }
            Self::ReadExisting { path, .. } => {
                write!(formatter, "read existing declaration {}", path.display())
            }
            Self::AuthoredDeclaration { path } => {
                write!(formatter, "refuse to overwrite authored declaration {}", path.display())
            }
            Self::CreateTemporary { path, .. } => {
                write!(formatter, "create temporary declaration {}", path.display())
            }
            Self::WriteTemporary { path, .. } => {
                write!(formatter, "write temporary declaration {}", path.display())
            }
            Self::SyncTemporary { path, .. } => {
                write!(formatter, "sync temporary declaration {}", path.display())
            }
            Self::CleanupTemporary { path, .. } => {
                write!(formatter, "clean up temporary declaration {}", path.display())
            }
            Self::GeneratedTooLarge { size, limit } => {
                write!(formatter, "generated declaration is {size} bytes; limit is {limit} bytes")
            }
            Self::Rename { from, to, .. } => {
                write!(formatter, "rename {} to {}", from.display(), to.display())
            }
            Self::SyncDirectory { path, .. } => {
                write!(formatter, "sync declaration directory {}", path.display())
            }
        }
    }
}

impl Error for SaveDeclarationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. }
            | Self::ReadExisting { source, .. }
            | Self::CreateTemporary { source, .. }
            | Self::WriteTemporary { source, .. }
            | Self::SyncTemporary { source, .. }
            | Self::CleanupTemporary { source, .. }
            | Self::Rename { source, .. }
            | Self::SyncDirectory { source, .. } => Some(source),
            Self::MissingOwnershipMarker { .. }
            | Self::AuthoredDeclaration { .. }
            | Self::GeneratedTooLarge { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum DeleteDeclarationError {
    ReadExisting {
        path: PathBuf,
        source: io::Error,
    },
    AuthoredDeclaration {
        path: PathBuf,
    },
    Remove {
        path: PathBuf,
        source: io::Error,
    },
    SyncDirectory {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for DeleteDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadExisting { path, .. } => {
                write!(formatter, "read existing declaration {}", path.display())
            }
            Self::AuthoredDeclaration { path } => {
                write!(formatter, "refuse to delete authored declaration {}", path.display())
            }
            Self::Remove { path, .. } => {
                write!(formatter, "delete generated declaration {}", path.display())
            }
            Self::SyncDirectory { path, .. } => {
                write!(formatter, "sync declaration directory {}", path.display())
            }
        }
    }
}

impl Error for DeleteDeclarationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadExisting { source, .. }
            | Self::Remove { source, .. }
            | Self::SyncDirectory { source, .. } => Some(source),
            Self::AuthoredDeclaration { .. } => None,
        }
    }
}
