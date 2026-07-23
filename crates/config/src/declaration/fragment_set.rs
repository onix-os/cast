use std::{
    error::Error,
    ffi::OsStr,
    fmt, io,
    path::{Component, Path, PathBuf},
};

use declarative_config::{Diagnostic, LanguageSpec, SourceRoot};
use fs_err as fs;

use super::managed_directory::FileSnapshot;

mod discovery;

#[cfg(test)]
mod tests;

/// Explicit discovery ceilings for one ordered fragment set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentDeclarationLimits {
    max_fragments: usize,
    max_directory_entries: usize,
}

impl FragmentDeclarationLimits {
    pub const fn new(max_fragments: usize, max_directory_entries: usize) -> Self {
        Self {
            max_fragments,
            max_directory_entries,
        }
    }

    pub const fn max_fragments(self) -> usize {
        self.max_fragments
    }

    pub const fn max_directory_entries(self) -> usize {
        self.max_directory_entries
    }
}

/// Every registered declaration discovered from an explicit ordered layer set.
///
/// Repeated logical names across layers remain present in layer order. A
/// consumer can therefore evaluate every discovered declaration before it
/// applies its own whole-fragment precedence rule.
#[derive(Debug, Clone)]
pub struct FragmentDeclarationSet {
    domain: String,
    layer_directories: Vec<PathBuf>,
    declarations: Vec<DiscoveredFragmentDeclaration>,
}

impl FragmentDeclarationSet {
    pub fn discover(
        layer_directories: Vec<PathBuf>,
        domain: impl Into<String>,
        languages: &super::RegisteredLanguages,
        limits: FragmentDeclarationLimits,
    ) -> Result<Self, FragmentDeclarationSetError> {
        let domain = domain.into();
        if !is_safe_component(&domain) {
            return Err(FragmentDeclarationSetError::InvalidDomain { domain });
        }
        let declarations = discovery::discover_layers(
            &layer_directories,
            &domain,
            languages,
            limits,
        )?;
        Ok(Self {
            domain,
            layer_directories,
            declarations,
        })
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn layer_directories(&self) -> &[PathBuf] {
        &self.layer_directories
    }

    pub fn declarations(&self) -> &[DiscoveredFragmentDeclaration] {
        &self.declarations
    }

    pub fn iter(&self) -> impl Iterator<Item = &DiscoveredFragmentDeclaration> {
        self.declarations.iter()
    }

    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }
}

/// One immutable declaration selection with its retained source authority.
#[derive(Debug, Clone)]
pub struct DiscoveredFragmentDeclaration {
    layer_index: usize,
    layer_directory: PathBuf,
    physical_path: PathBuf,
    relative_path: PathBuf,
    logical_name: String,
    language: LanguageSpec,
    source_root: SourceRoot,
    collection: Option<CollectionIdentity>,
}

impl DiscoveredFragmentDeclaration {
    pub fn layer_index(&self) -> usize {
        self.layer_index
    }

    pub fn layer_directory(&self) -> &Path {
        &self.layer_directory
    }

    pub fn physical_path(&self) -> &Path {
        &self.physical_path
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    pub fn language(&self) -> &LanguageSpec {
        &self.language
    }

    pub fn source_root(&self) -> &SourceRoot {
        &self.source_root
    }

    /// Revalidate the selected layer and optional collection immediately
    /// before a descriptor-rooted source read.
    pub fn revalidate_before_read(&self) -> Result<(), FragmentDeclarationSetError> {
        self.verify_source_root()?;
        self.verify_collection()
    }

    /// Revalidate the optional collection and selected layer after a read,
    /// then verify any directories retained while resolving imports.
    pub fn revalidate_after_read(&self) -> Result<(), FragmentDeclarationSetError> {
        self.verify_collection()?;
        self.verify_source_root()?;
        self.source_root
            .verify_retained_directories()
            .map_err(|source| FragmentDeclarationSetError::VerifyRetainedDirectories {
                layer_index: self.layer_index,
                path: self.layer_directory.clone(),
                source,
            })
    }

    fn verify_source_root(&self) -> Result<(), FragmentDeclarationSetError> {
        let current = SourceRoot::new(&self.layer_directory).map_err(|source| {
            FragmentDeclarationSetError::OpenSourceRoot {
                layer_index: self.layer_index,
                path: self.layer_directory.clone(),
                source,
            }
        })?;
        if current == self.source_root {
            Ok(())
        } else {
            Err(FragmentDeclarationSetError::SourceRootChanged {
                layer_index: self.layer_index,
                path: self.layer_directory.clone(),
            })
        }
    }

    fn verify_collection(&self) -> Result<(), FragmentDeclarationSetError> {
        let Some(collection) = &self.collection else {
            return Ok(());
        };
        collection
            .verify()
            .map_err(|source| FragmentDeclarationSetError::CollectionChanged {
                layer_index: self.layer_index,
                path: collection.path.clone(),
                source,
            })
    }
}

#[derive(Debug, Clone)]
struct CollectionIdentity {
    path: PathBuf,
    identity: FileSnapshot,
}

impl CollectionIdentity {
    fn new(path: PathBuf, identity: FileSnapshot) -> Self {
        Self { path, identity }
    }

    fn verify(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_dir()
            && FileSnapshot::from_metadata(&metadata) == self.identity
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "declaration collection changed after discovery",
            ))
        }
    }
}

#[derive(Debug)]
pub enum FragmentDeclarationSetError {
    InvalidDomain {
        domain: String,
    },
    InspectLayer {
        layer_index: usize,
        path: PathBuf,
        source: io::Error,
    },
    OpenSourceRoot {
        layer_index: usize,
        path: PathBuf,
        source: Diagnostic,
    },
    InspectDeclaration {
        layer_index: usize,
        path: PathBuf,
        source: io::Error,
    },
    NotRegular {
        layer_index: usize,
        path: PathBuf,
    },
    CollectionNotDirectory {
        layer_index: usize,
        path: PathBuf,
    },
    OpenCollection {
        layer_index: usize,
        path: PathBuf,
        source: io::Error,
    },
    CollectionChanged {
        layer_index: usize,
        path: PathBuf,
        source: io::Error,
    },
    DirectoryEntryLimit {
        layer_index: usize,
        path: PathBuf,
        limit: usize,
    },
    FragmentLimit {
        limit: usize,
    },
    InvalidLogicalName {
        layer_index: usize,
        path: PathBuf,
    },
    Collision {
        layer_index: usize,
        logical_name: String,
        paths: Vec<PathBuf>,
    },
    SourceRootChanged {
        layer_index: usize,
        path: PathBuf,
    },
    VerifyRetainedDirectories {
        layer_index: usize,
        path: PathBuf,
        source: Diagnostic,
    },
}

impl FragmentDeclarationSetError {
    pub fn collision_paths(&self) -> Option<&[PathBuf]> {
        match self {
            Self::Collision { paths, .. } => Some(paths),
            _ => None,
        }
    }
}

impl fmt::Display for FragmentDeclarationSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDomain { domain } => {
                write!(formatter, "invalid declaration domain basename {domain:?}")
            }
            Self::InspectLayer {
                layer_index, path, ..
            } => write!(
                formatter,
                "inspect declaration layer {layer_index} at {}",
                path.display()
            ),
            Self::OpenSourceRoot {
                layer_index, path, ..
            } => write!(
                formatter,
                "open declaration layer {layer_index} source root at {}",
                path.display()
            ),
            Self::InspectDeclaration {
                layer_index, path, ..
            } => write!(
                formatter,
                "inspect declaration candidate in layer {layer_index} at {}",
                path.display()
            ),
            Self::NotRegular { layer_index, path } => write!(
                formatter,
                "registered declaration in layer {layer_index} at {} is not a regular file",
                path.display()
            ),
            Self::CollectionNotDirectory { layer_index, path } => write!(
                formatter,
                "declaration collection in layer {layer_index} at {} is not a directory",
                path.display()
            ),
            Self::OpenCollection {
                layer_index, path, ..
            } => write!(
                formatter,
                "open declaration collection in layer {layer_index} at {}",
                path.display()
            ),
            Self::CollectionChanged {
                layer_index, path, ..
            } => write!(
                formatter,
                "declaration collection in layer {layer_index} at {} changed after discovery",
                path.display()
            ),
            Self::DirectoryEntryLimit {
                layer_index,
                path,
                limit,
            } => write!(
                formatter,
                "declaration collection in layer {layer_index} at {} exceeds the {limit}-entry limit",
                path.display()
            ),
            Self::FragmentLimit { limit } => {
                write!(formatter, "declaration set exceeds the {limit}-fragment limit")
            }
            Self::InvalidLogicalName { layer_index, path } => write!(
                formatter,
                "declaration in layer {layer_index} at {} has an invalid logical name",
                path.display()
            ),
            Self::Collision {
                layer_index,
                logical_name,
                paths,
            } => write!(
                formatter,
                "declaration layer {layer_index} has {} candidates for logical name {logical_name:?}",
                paths.len()
            ),
            Self::SourceRootChanged { layer_index, path } => write!(
                formatter,
                "declaration layer {layer_index} source root at {} changed after discovery",
                path.display()
            ),
            Self::VerifyRetainedDirectories {
                layer_index, path, ..
            } => write!(
                formatter,
                "verify retained declaration directories for layer {layer_index} at {}",
                path.display()
            ),
        }
    }
}

impl Error for FragmentDeclarationSetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InspectLayer { source, .. }
            | Self::InspectDeclaration { source, .. }
            | Self::OpenCollection { source, .. }
            | Self::CollectionChanged { source, .. } => Some(source),
            Self::OpenSourceRoot { source, .. }
            | Self::VerifyRetainedDirectories { source, .. } => Some(source),
            Self::InvalidDomain { .. }
            | Self::NotRegular { .. }
            | Self::CollectionNotDirectory { .. }
            | Self::DirectoryEntryLimit { .. }
            | Self::FragmentLimit { .. }
            | Self::InvalidLogicalName { .. }
            | Self::Collision { .. }
            | Self::SourceRootChanged { .. } => None,
        }
    }
}

fn is_safe_component(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == OsStr::new(value)
    )
}
