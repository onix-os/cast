use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use declarative_config::SourceRoot;
use fs_err as fs;

use super::{
    CollectionIdentity, DiscoveredFragmentDeclaration,
    FragmentDeclarationLimits, FragmentDeclarationSetError, is_safe_component,
};
use crate::declaration::{
    RegisteredLanguages,
    managed_directory::{BoundedDirectoryEntries, FileSnapshot, ManagedDirectory},
};

pub(super) fn discover_layers(
    layer_directories: &[PathBuf],
    domain: &str,
    languages: &RegisteredLanguages,
    limits: FragmentDeclarationLimits,
) -> Result<Vec<DiscoveredFragmentDeclaration>, FragmentDeclarationSetError> {
    let mut discovered = Vec::new();
    for (layer_index, layer_directory) in layer_directories.iter().enumerate() {
        let metadata = match fs::symlink_metadata(layer_directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(FragmentDeclarationSetError::InspectLayer {
                    layer_index,
                    path: layer_directory.clone(),
                    source,
                });
            }
        };
        if !metadata.file_type().is_dir() {
            return Err(FragmentDeclarationSetError::OpenSourceRoot {
                layer_index,
                path: layer_directory.clone(),
                source: declarative_config::Diagnostic::io(
                    Some(layer_directory.display().to_string()),
                    io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "declaration layer is not a directory",
                    ),
                ),
            });
        }
        let source_root = SourceRoot::new(layer_directory).map_err(|source| {
            FragmentDeclarationSetError::OpenSourceRoot {
                layer_index,
                path: layer_directory.clone(),
                source,
            }
        })?;
        let mut candidates = discover_root_candidates(
            layer_index,
            layer_directory,
            domain,
            languages,
            &source_root,
        )?;
        candidates.extend(discover_collection_candidates(
            layer_index,
            layer_directory,
            domain,
            languages,
            limits.max_directory_entries(),
            &source_root,
        )?);

        reject_collisions(layer_index, &candidates)?;
        for candidate in candidates {
            if discovered.len() == limits.max_fragments() {
                return Err(FragmentDeclarationSetError::FragmentLimit {
                    limit: limits.max_fragments(),
                });
            }
            discovered.push(candidate);
        }

        let current = SourceRoot::new(layer_directory).map_err(|source| {
            FragmentDeclarationSetError::OpenSourceRoot {
                layer_index,
                path: layer_directory.clone(),
                source,
            }
        })?;
        if current != source_root {
            return Err(FragmentDeclarationSetError::SourceRootChanged {
                layer_index,
                path: layer_directory.clone(),
            });
        }
    }
    Ok(discovered)
}

fn discover_root_candidates(
    layer_index: usize,
    layer_directory: &Path,
    domain: &str,
    languages: &RegisteredLanguages,
    source_root: &SourceRoot,
) -> Result<Vec<DiscoveredFragmentDeclaration>, FragmentDeclarationSetError> {
    let mut candidates = Vec::new();
    for language in languages.iter() {
        let relative_path = PathBuf::from(format!("{domain}.{}", language.extension()));
        let physical_path = layer_directory.join(&relative_path);
        let metadata = match fs::symlink_metadata(&physical_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(FragmentDeclarationSetError::InspectDeclaration {
                    layer_index,
                    path: physical_path,
                    source,
                });
            }
        };
        if !metadata.file_type().is_file() {
            return Err(FragmentDeclarationSetError::NotRegular {
                layer_index,
                path: physical_path,
            });
        }
        candidates.push(DiscoveredFragmentDeclaration {
            layer_index,
            layer_directory: layer_directory.to_owned(),
            physical_path,
            relative_path,
            logical_name: domain.to_owned(),
            language: language.clone(),
            source_root: source_root.clone(),
            collection: None,
        });
    }
    Ok(candidates)
}

fn discover_collection_candidates(
    layer_index: usize,
    layer_directory: &Path,
    domain: &str,
    languages: &RegisteredLanguages,
    directory_entry_limit: usize,
    source_root: &SourceRoot,
) -> Result<Vec<DiscoveredFragmentDeclaration>, FragmentDeclarationSetError> {
    let relative_directory = PathBuf::from(format!("{domain}.d"));
    let collection_path = layer_directory.join(&relative_directory);
    let metadata = match fs::symlink_metadata(&collection_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(FragmentDeclarationSetError::OpenCollection {
                layer_index,
                path: collection_path,
                source,
            });
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(FragmentDeclarationSetError::CollectionNotDirectory {
            layer_index,
            path: collection_path,
        });
    }

    let directory = ManagedDirectory::open(&collection_path).map_err(|source| {
        FragmentDeclarationSetError::OpenCollection {
            layer_index,
            path: collection_path.clone(),
            source,
        }
    })?;
    let opened_metadata = directory.metadata().map_err(|source| {
        FragmentDeclarationSetError::OpenCollection {
            layer_index,
            path: collection_path.clone(),
            source,
        }
    })?;
    let collection = CollectionIdentity::new(
        collection_path.clone(),
        FileSnapshot::from_metadata(&metadata),
    );
    if !opened_metadata.file_type().is_dir()
        || FileSnapshot::from_metadata(&opened_metadata) != collection.identity
    {
        return Err(FragmentDeclarationSetError::CollectionChanged {
            layer_index,
            path: collection_path,
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "declaration collection changed while it was being opened",
            ),
        });
    }

    let mut names = match directory.entry_names(directory_entry_limit).map_err(|source| {
        FragmentDeclarationSetError::OpenCollection {
            layer_index,
            path: collection.path.clone(),
            source,
        }
    })? {
        BoundedDirectoryEntries::Complete(names) => names,
        BoundedDirectoryEntries::LimitExceeded => {
            return Err(FragmentDeclarationSetError::DirectoryEntryLimit {
                layer_index,
                path: collection.path,
                limit: directory_entry_limit,
            });
        }
    };
    names.sort();

    let mut candidates = Vec::new();
    for name in names {
        let entry_path = Path::new(&name);
        let Some(extension) = entry_path.extension().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(language) = languages.get(extension) else {
            continue;
        };
        let physical_path = collection.path.join(&name);
        let entry_metadata = directory.metadata_at(&name).map_err(|source| {
            FragmentDeclarationSetError::InspectDeclaration {
                layer_index,
                path: physical_path.clone(),
                source,
            }
        })?;
        if !entry_metadata.file_type().is_file() {
            return Err(FragmentDeclarationSetError::NotRegular {
                layer_index,
                path: physical_path,
            });
        }
        let Some(logical_name) = entry_path.file_stem().and_then(OsStr::to_str) else {
            return Err(FragmentDeclarationSetError::InvalidLogicalName {
                layer_index,
                path: physical_path,
            });
        };
        if !is_safe_component(logical_name) {
            return Err(FragmentDeclarationSetError::InvalidLogicalName {
                layer_index,
                path: physical_path,
            });
        }
        candidates.push(DiscoveredFragmentDeclaration {
            layer_index,
            layer_directory: layer_directory.to_owned(),
            physical_path,
            relative_path: relative_directory.join(&name),
            logical_name: logical_name.to_owned(),
            language: language.clone(),
            source_root: source_root.clone(),
            collection: Some(collection.clone()),
        });
    }

    collection
        .verify()
        .map_err(|source| FragmentDeclarationSetError::CollectionChanged {
            layer_index,
            path: collection.path,
            source,
        })?;
    Ok(candidates)
}

fn reject_collisions(
    layer_index: usize,
    candidates: &[DiscoveredFragmentDeclaration],
) -> Result<(), FragmentDeclarationSetError> {
    let mut paths_by_logical_name: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
    for candidate in candidates {
        paths_by_logical_name
            .entry(candidate.logical_name())
            .or_default()
            .push(candidate.physical_path().to_owned());
    }
    if let Some((logical_name, paths)) = paths_by_logical_name
        .into_iter()
        .find(|(_, paths)| paths.len() > 1)
    {
        return Err(FragmentDeclarationSetError::Collision {
            layer_index,
            logical_name: logical_name.to_owned(),
            paths,
        });
    }
    Ok(())
}
