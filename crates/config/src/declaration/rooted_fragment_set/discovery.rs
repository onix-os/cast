use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use declarative_config::SourceRoot;

use super::{
    FragmentDeclarationLimits, RegisteredLanguages,
    RootedFragmentDeclaration, RootedFragmentDeclarationSetError,
    is_safe_component, reject_collisions,
    retained_authority::{
        BoundedDirectoryEntries, RetainedNode, RetainedNodeKind, RetainedRoot,
    },
};

pub(super) fn discover(
    root_path: &Path,
    root: Arc<RetainedRoot>,
    source_root: &SourceRoot,
    domain: &str,
    languages: &RegisteredLanguages,
    limits: FragmentDeclarationLimits,
) -> Result<Vec<RootedFragmentDeclaration>, RootedFragmentDeclarationSetError> {
    let mut declarations = discover_root_candidates(
        root_path,
        Arc::clone(&root),
        source_root,
        domain,
        languages,
    )?;
    declarations.extend(discover_collection_candidates(
        root_path,
        Arc::clone(&root),
        source_root,
        domain,
        languages,
        limits.max_directory_entries(),
    )?);

    reject_collisions(&declarations)?;
    if declarations.len() > limits.max_fragments() {
        return Err(RootedFragmentDeclarationSetError::FragmentLimit {
            limit: limits.max_fragments(),
            discovered: declarations.len(),
        });
    }
    for declaration in &declarations {
        declaration.revalidate_before_evaluation()?;
    }
    root.verify_descriptor().map_err(|source| {
        RootedFragmentDeclarationSetError::RootChanged {
            path: root_path.to_owned(),
            source,
        }
    })?;
    source_root
        .verify_retained_directories()
        .map_err(|source| {
            RootedFragmentDeclarationSetError::VerifyRetainedDirectories {
                path: root_path.to_owned(),
                source,
            }
        })?;
    Ok(declarations)
}

fn discover_root_candidates(
    root_path: &Path,
    root: Arc<RetainedRoot>,
    source_root: &SourceRoot,
    domain: &str,
    languages: &RegisteredLanguages,
) -> Result<Vec<RootedFragmentDeclaration>, RootedFragmentDeclarationSetError> {
    let mut declarations = Vec::new();
    for language in languages.iter() {
        let relative_path = PathBuf::from(format!("{domain}.{}", language.extension()));
        let physical_path = root_path.join(&relative_path);
        let Some(descriptor) = root
            .open_optional(
                &relative_path,
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                &physical_path,
            )
            .map_err(|source| RootedFragmentDeclarationSetError::InspectDeclaration {
                path: physical_path.clone(),
                source,
            })?
        else {
            continue;
        };
        let metadata = descriptor.metadata().map_err(|source| {
            RootedFragmentDeclarationSetError::InspectDeclaration {
                path: physical_path.clone(),
                source,
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(RootedFragmentDeclarationSetError::NotRegular {
                path: physical_path,
            });
        }
        let file = RetainedNode::from_opened(
            physical_path.clone(),
            relative_path.clone(),
            descriptor,
            RetainedNodeKind::RegularFile,
        )
        .map_err(|source| RootedFragmentDeclarationSetError::InspectDeclaration {
            path: physical_path.clone(),
            source,
        })?;
        declarations.push(RootedFragmentDeclaration {
            root_path: root_path.to_owned(),
            physical_path,
            relative_path,
            logical_name: domain.to_owned(),
            language: language.clone(),
            root: Arc::clone(&root),
            collection: None,
            file: Arc::new(file),
            source_root: source_root.clone(),
        });
    }
    Ok(declarations)
}

fn discover_collection_candidates(
    root_path: &Path,
    root: Arc<RetainedRoot>,
    source_root: &SourceRoot,
    domain: &str,
    languages: &RegisteredLanguages,
    directory_entry_limit: usize,
) -> Result<Vec<RootedFragmentDeclaration>, RootedFragmentDeclarationSetError> {
    let collection_relative_path = PathBuf::from(format!("{domain}.d"));
    let collection_path = root_path.join(&collection_relative_path);
    let Some(descriptor) = root
        .open_optional(
            &collection_relative_path,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            &collection_path,
        )
        .map_err(|source| RootedFragmentDeclarationSetError::OpenCollection {
            path: collection_path.clone(),
            source,
        })?
    else {
        return Ok(Vec::new());
    };
    let metadata = descriptor.metadata().map_err(|source| {
        RootedFragmentDeclarationSetError::OpenCollection {
            path: collection_path.clone(),
            source,
        }
    })?;
    if !metadata.file_type().is_dir() {
        return Err(RootedFragmentDeclarationSetError::CollectionNotDirectory {
            path: collection_path,
        });
    }
    let collection = Arc::new(
        RetainedNode::from_opened(
            collection_path.clone(),
            collection_relative_path.clone(),
            descriptor,
            RetainedNodeKind::Directory,
        )
        .map_err(|source| RootedFragmentDeclarationSetError::OpenCollection {
            path: collection_path.clone(),
            source,
        })?,
    );
    collection.verify_beneath(&root).map_err(|source| {
        RootedFragmentDeclarationSetError::CollectionChanged {
            path: collection_path.clone(),
            source,
        }
    })?;

    let mut names = match collection
        .entry_names(directory_entry_limit)
        .map_err(|source| RootedFragmentDeclarationSetError::OpenCollection {
            path: collection_path.clone(),
            source,
        })?
    {
        BoundedDirectoryEntries::Complete(names) => names,
        BoundedDirectoryEntries::LimitExceeded => {
            return Err(RootedFragmentDeclarationSetError::DirectoryEntryLimit {
                path: collection_path,
                limit: directory_entry_limit,
            });
        }
    };
    names.sort();

    let mut declarations = Vec::new();
    for name in names {
        let entry_path = Path::new(&name);
        let Some(extension) = entry_path.extension().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(language) = languages.get(extension) else {
            continue;
        };
        let relative_path = collection_relative_path.join(&name);
        let physical_path = root_path.join(&relative_path);
        let logical_name = matching_logical_name(&name, &physical_path)?;
        let descriptor = root
            .open_optional(
                &relative_path,
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                &physical_path,
            )
            .map_err(|source| RootedFragmentDeclarationSetError::InspectDeclaration {
                path: physical_path.clone(),
                source,
            })?
            .ok_or_else(|| RootedFragmentDeclarationSetError::InspectDeclaration {
                path: physical_path.clone(),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "registered declaration disappeared during retained discovery",
                ),
            })?;
        let metadata = descriptor.metadata().map_err(|source| {
            RootedFragmentDeclarationSetError::InspectDeclaration {
                path: physical_path.clone(),
                source,
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(RootedFragmentDeclarationSetError::NotRegular {
                path: physical_path,
            });
        }
        let file = RetainedNode::from_opened(
            physical_path.clone(),
            relative_path.clone(),
            descriptor,
            RetainedNodeKind::RegularFile,
        )
        .map_err(|source| RootedFragmentDeclarationSetError::InspectDeclaration {
            path: physical_path.clone(),
            source,
        })?;
        declarations.push(RootedFragmentDeclaration {
            root_path: root_path.to_owned(),
            physical_path,
            relative_path,
            logical_name,
            language: language.clone(),
            root: Arc::clone(&root),
            collection: Some(Arc::clone(&collection)),
            file: Arc::new(file),
            source_root: source_root.clone(),
        });
    }

    collection.verify_beneath(&root).map_err(|source| {
        RootedFragmentDeclarationSetError::CollectionChanged {
            path: collection.path().to_owned(),
            source,
        }
    })?;
    Ok(declarations)
}

fn matching_logical_name(
    name: &OsString,
    physical_path: &Path,
) -> Result<String, RootedFragmentDeclarationSetError> {
    let logical_name = Path::new(name)
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| RootedFragmentDeclarationSetError::InvalidLogicalName {
            path: physical_path.to_owned(),
        })?;
    if !is_safe_component(logical_name) {
        return Err(RootedFragmentDeclarationSetError::InvalidLogicalName {
            path: physical_path.to_owned(),
        });
    }
    Ok(logical_name.to_owned())
}
