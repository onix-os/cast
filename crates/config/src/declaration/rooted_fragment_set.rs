//! Descriptor-rooted declaration discovery and bounded source reads.

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsStr,
    fmt, io,
    os::fd::AsRawFd,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use declarative_config::{Diagnostic, LanguageSpec, Source, SourceRoot};

use super::{FragmentDeclarationLimits, RegisteredLanguages};

mod discovery;
mod retained_authority;

#[cfg(test)]
mod tests;

use retained_authority::{RetainedNode, RetainedRoot};

/// A declaration set selected entirely beneath an already-retained directory.
///
/// `root_path` is diagnostic only. Discovery and every later source read use
/// owned descriptors derived from `root`, so replacing that public path never
/// redirects the selected configuration tree.
#[derive(Debug)]
pub struct RootedFragmentDeclarationSet {
    root_path: PathBuf,
    domain: String,
    root: Arc<RetainedRoot>,
    source_root: SourceRoot,
    declarations: Vec<RootedFragmentDeclaration>,
}

impl RootedFragmentDeclarationSet {
    pub fn discover(
        root_path: impl AsRef<Path>,
        root: &impl AsRawFd,
        domain: impl Into<String>,
        languages: &RegisteredLanguages,
        limits: FragmentDeclarationLimits,
    ) -> Result<Self, RootedFragmentDeclarationSetError> {
        let root_path = root_path.as_ref().to_owned();
        let domain = domain.into();
        if !is_safe_component(&domain) {
            return Err(RootedFragmentDeclarationSetError::InvalidDomain { domain });
        }

        let root = Arc::new(
            RetainedRoot::duplicate(&root_path, root.as_raw_fd()).map_err(|source| {
                RootedFragmentDeclarationSetError::OpenRoot {
                    path: root_path.clone(),
                    source,
                }
            })?,
        );
        let source_root = SourceRoot::from_directory(&root_path, root.descriptor()).map_err(
            |source| RootedFragmentDeclarationSetError::OpenSourceRoot {
                path: root_path.clone(),
                source,
            },
        )?;
        let declarations = discovery::discover(
            &root_path,
            Arc::clone(&root),
            &source_root,
            &domain,
            languages,
            limits,
        )?;

        root.verify_descriptor().map_err(|source| {
            RootedFragmentDeclarationSetError::RootChanged {
                path: root_path.clone(),
                source,
            }
        })?;
        Ok(Self {
            root_path,
            domain,
            root,
            source_root,
            declarations,
        })
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn source_root(&self) -> &SourceRoot {
        &self.source_root
    }

    pub fn declarations(&self) -> &[RootedFragmentDeclaration] {
        &self.declarations
    }

    pub fn iter(&self) -> impl Iterator<Item = &RootedFragmentDeclaration> {
        self.declarations.iter()
    }

    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// Revalidate the retained root even when no declaration was discovered.
    pub fn revalidate(&self) -> Result<(), RootedFragmentDeclarationSetError> {
        self.root.verify_descriptor().map_err(|source| {
            RootedFragmentDeclarationSetError::RootChanged {
                path: self.root_path.clone(),
                source,
            }
        })?;
        self.source_root
            .verify_retained_directories()
            .map_err(|source| {
                RootedFragmentDeclarationSetError::VerifyRetainedDirectories {
                    path: self.root_path.clone(),
                    source,
                }
            })
    }
}

/// One immutable descriptor-rooted declaration selection.
#[derive(Debug, Clone)]
pub struct RootedFragmentDeclaration {
    root_path: PathBuf,
    physical_path: PathBuf,
    relative_path: PathBuf,
    logical_name: String,
    language: LanguageSpec,
    root: Arc<RetainedRoot>,
    collection: Option<Arc<RetainedNode>>,
    file: Arc<RetainedNode>,
    source_root: SourceRoot,
}

impl RootedFragmentDeclaration {
    pub fn root_path(&self) -> &Path {
        &self.root_path
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

    /// Securely read this exact declaration under the adapter's source limit.
    ///
    /// The retained root, optional collection, and file are revalidated both
    /// before and after `SourceRoot` performs its own bounded descriptor read.
    pub fn read_source(
        &self,
        max_source_bytes: usize,
    ) -> Result<Source, RootedFragmentDeclarationSetError> {
        self.revalidate_before_evaluation()?;
        let source = self
            .source_root
            .load(&self.relative_path, max_source_bytes)
            .map_err(|source| RootedFragmentDeclarationSetError::ReadSource {
                path: self.physical_path.clone(),
                source,
            })?;
        self.revalidate_after_evaluation()?;
        Ok(source)
    }

    /// Verify every discovery witness immediately before reading or evaluating.
    pub fn revalidate_before_evaluation(
        &self,
    ) -> Result<(), RootedFragmentDeclarationSetError> {
        self.verify_collection()?;
        self.verify_file()?;
        self.verify_root()
    }

    /// Verify the source/import directory chain and every discovery witness.
    ///
    /// A typed adapter should call this after evaluation, including after all
    /// relative imports have been resolved.
    pub fn revalidate_after_evaluation(
        &self,
    ) -> Result<(), RootedFragmentDeclarationSetError> {
        self.source_root
            .verify_retained_directories()
            .map_err(|source| {
                RootedFragmentDeclarationSetError::VerifyRetainedDirectories {
                    path: self.physical_path.clone(),
                    source,
                }
            })?;
        self.verify_collection()?;
        self.verify_file()?;
        self.verify_root()
    }

    fn verify_root(&self) -> Result<(), RootedFragmentDeclarationSetError> {
        self.root.verify_descriptor().map_err(|source| {
            RootedFragmentDeclarationSetError::RootChanged {
                path: self.root_path.clone(),
                source,
            }
        })
    }

    fn verify_collection(&self) -> Result<(), RootedFragmentDeclarationSetError> {
        let Some(collection) = &self.collection else {
            return Ok(());
        };
        collection
            .verify_beneath(&self.root)
            .map_err(|source| RootedFragmentDeclarationSetError::CollectionChanged {
                path: collection.path().to_owned(),
                source,
            })
    }

    fn verify_file(&self) -> Result<(), RootedFragmentDeclarationSetError> {
        self.file
            .verify_beneath(&self.root)
            .map_err(|source| RootedFragmentDeclarationSetError::DeclarationChanged {
                path: self.physical_path.clone(),
                source,
            })
    }
}

#[derive(Debug)]
pub enum RootedFragmentDeclarationSetError {
    InvalidDomain {
        domain: String,
    },
    OpenRoot {
        path: PathBuf,
        source: io::Error,
    },
    OpenSourceRoot {
        path: PathBuf,
        source: Diagnostic,
    },
    OpenCollection {
        path: PathBuf,
        source: io::Error,
    },
    CollectionNotDirectory {
        path: PathBuf,
    },
    DirectoryEntryLimit {
        path: PathBuf,
        limit: usize,
    },
    InspectDeclaration {
        path: PathBuf,
        source: io::Error,
    },
    NotRegular {
        path: PathBuf,
    },
    InvalidLogicalName {
        path: PathBuf,
    },
    Collision {
        logical_name: String,
        paths: Vec<PathBuf>,
    },
    FragmentLimit {
        limit: usize,
        discovered: usize,
    },
    RootChanged {
        path: PathBuf,
        source: io::Error,
    },
    CollectionChanged {
        path: PathBuf,
        source: io::Error,
    },
    DeclarationChanged {
        path: PathBuf,
        source: io::Error,
    },
    ReadSource {
        path: PathBuf,
        source: Diagnostic,
    },
    VerifyRetainedDirectories {
        path: PathBuf,
        source: Diagnostic,
    },
}

impl RootedFragmentDeclarationSetError {
    pub fn collision_paths(&self) -> Option<&[PathBuf]> {
        match self {
            Self::Collision { paths, .. } => Some(paths),
            _ => None,
        }
    }
}

impl fmt::Display for RootedFragmentDeclarationSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDomain { domain } => {
                write!(formatter, "invalid declaration domain basename {domain:?}")
            }
            Self::OpenRoot { path, .. } => {
                write!(formatter, "retain declaration root at {}", path.display())
            }
            Self::OpenSourceRoot { path, .. } => {
                write!(formatter, "open retained declaration source root at {}", path.display())
            }
            Self::OpenCollection { path, .. } => {
                write!(formatter, "open retained declaration collection at {}", path.display())
            }
            Self::CollectionNotDirectory { path } => write!(
                formatter,
                "declaration collection at {} is not a directory",
                path.display()
            ),
            Self::DirectoryEntryLimit { path, limit } => write!(
                formatter,
                "declaration collection at {} exceeds the {limit}-entry limit",
                path.display()
            ),
            Self::InspectDeclaration { path, .. } => {
                write!(formatter, "inspect retained declaration at {}", path.display())
            }
            Self::NotRegular { path } => write!(
                formatter,
                "registered declaration at {} is not a regular file",
                path.display()
            ),
            Self::InvalidLogicalName { path } => write!(
                formatter,
                "registered declaration at {} has an invalid logical name",
                path.display()
            ),
            Self::Collision {
                logical_name,
                paths,
            } => write!(
                formatter,
                "retained declaration root has {} candidates for logical name {logical_name:?}",
                paths.len()
            ),
            Self::FragmentLimit { limit, discovered } => write!(
                formatter,
                "retained declaration set has {discovered} fragments and exceeds the {limit}-fragment limit"
            ),
            Self::RootChanged { path, .. } => {
                write!(formatter, "retained declaration root at {} changed", path.display())
            }
            Self::CollectionChanged { path, .. } => write!(
                formatter,
                "retained declaration collection at {} changed",
                path.display()
            ),
            Self::DeclarationChanged { path, .. } => {
                write!(formatter, "retained declaration at {} changed", path.display())
            }
            Self::ReadSource { path, .. } => {
                write!(formatter, "read retained declaration at {}", path.display())
            }
            Self::VerifyRetainedDirectories { path, .. } => write!(
                formatter,
                "verify retained declaration directories for {}",
                path.display()
            ),
        }
    }
}

impl Error for RootedFragmentDeclarationSetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OpenRoot { source, .. }
            | Self::OpenCollection { source, .. }
            | Self::InspectDeclaration { source, .. }
            | Self::RootChanged { source, .. }
            | Self::CollectionChanged { source, .. }
            | Self::DeclarationChanged { source, .. } => Some(source),
            Self::OpenSourceRoot { source, .. }
            | Self::ReadSource { source, .. }
            | Self::VerifyRetainedDirectories { source, .. } => Some(source),
            Self::InvalidDomain { .. }
            | Self::CollectionNotDirectory { .. }
            | Self::DirectoryEntryLimit { .. }
            | Self::NotRegular { .. }
            | Self::InvalidLogicalName { .. }
            | Self::Collision { .. }
            | Self::FragmentLimit { .. } => None,
        }
    }
}

fn reject_collisions(
    declarations: &[RootedFragmentDeclaration],
) -> Result<(), RootedFragmentDeclarationSetError> {
    let mut paths_by_name: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
    for declaration in declarations {
        paths_by_name
            .entry(declaration.logical_name())
            .or_default()
            .push(declaration.physical_path().to_owned());
    }
    for paths in paths_by_name.values_mut() {
        paths.sort();
    }
    if let Some((logical_name, paths)) = paths_by_name
        .into_iter()
        .find(|(_, paths)| paths.len() > 1)
    {
        return Err(RootedFragmentDeclarationSetError::Collision {
            logical_name: logical_name.to_owned(),
            paths,
        });
    }
    Ok(())
}

fn is_safe_component(value: &str) -> bool {
    if value.is_empty() || value.contains('\\') || value.chars().any(char::is_control) {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == OsStr::new(value)
    )
}
