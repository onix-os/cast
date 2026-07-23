#[derive(Debug)]
struct GluonPath {
    logical_name: String,
    source_root_path: PathBuf,
    source_root: SourceRoot,
    relative_path: PathBuf,
    path: PathBuf,
    collection: Option<CollectionIdentity>,
}

#[derive(Debug, Clone)]
struct CollectionIdentity {
    path: PathBuf,
    identity: FileSnapshot,
}

fn collect_gluon_paths(scope: &super::Scope, domain: &str) -> Result<Vec<GluonPath>, LoadGluonError> {
    let mut paths = Vec::new();
    for (entry, resolve) in scope.load_with() {
        let remaining = MAX_GLUON_FRAGMENTS.saturating_sub(paths.len());
        let layer = enumerate_gluon_paths(entry, resolve, domain, remaining)?;
        if paths.len().saturating_add(layer.len()) > MAX_GLUON_FRAGMENTS {
            return Err(LoadGluonError::FragmentLimit {
                limit: MAX_GLUON_FRAGMENTS,
            });
        }
        paths.extend(layer);
    }
    Ok(paths)
}

fn enumerate_gluon_paths(
    entry: Entry,
    resolve: Resolve<'_>,
    domain: &str,
    remaining: usize,
) -> Result<Vec<GluonPath>, LoadGluonError> {
    let source_root_path = resolve.config_dir();
    let source_root = match fs::symlink_metadata(&source_root_path) {
        Ok(_) => SourceRoot::new(&source_root_path).map_err(|source| LoadGluonError::Evaluation {
            path: source_root_path.clone(),
            source,
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LoadGluonError::Enumerate {
                path: source_root_path,
                source,
            });
        }
    };
    match entry {
        Entry::File => {
            let relative_path = PathBuf::from(format!("{domain}.glu"));
            let path = resolve.file(domain, "glu");
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(source) => return Err(LoadGluonError::Enumerate { path, source }),
            };
            require_regular_fragment(&path, &metadata)?;
            if remaining == 0 {
                return Err(LoadGluonError::FragmentLimit {
                    limit: MAX_GLUON_FRAGMENTS,
                });
            }
            verify_source_root(&source_root_path, &source_root)?;
            Ok(vec![GluonPath {
                logical_name: domain.to_owned(),
                source_root_path,
                source_root,
                relative_path,
                path,
                collection: None,
            }])
        }
        Entry::Directory => {
            let relative_dir = PathBuf::from(format!("{domain}.d"));
            let dir = source_root_path.join(&relative_dir);
            let metadata = match fs::symlink_metadata(&dir) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(source) => return Err(LoadGluonError::Enumerate { path: dir, source }),
            };
            if !metadata.file_type().is_dir() {
                return Err(invalid_entry(&dir, "Gluon fragment collection is not a real directory"));
            }
            let directory = ManagedDirectory::open(&dir).map_err(|source| LoadGluonError::Enumerate {
                path: dir.clone(),
                source,
            })?;
            let opened_metadata = directory.metadata().map_err(|source| LoadGluonError::Enumerate {
                path: dir.clone(),
                source,
            })?;
            let collection = CollectionIdentity {
                path: dir.clone(),
                identity: FileSnapshot::from_metadata(&metadata),
            };
            if !opened_metadata.file_type().is_dir()
                || FileSnapshot::from_metadata(&opened_metadata) != collection.identity
            {
                return Err(invalid_entry(
                    &dir,
                    "Gluon fragment collection changed while its descriptor was being opened",
                ));
            }
            let entries = match directory.entry_names(MAX_GLUON_DIRECTORY_ENTRIES).map_err(|source| {
                LoadGluonError::Enumerate {
                    path: dir.clone(),
                    source,
                }
            })? {
                BoundedDirectoryEntries::Complete(entries) => entries,
                BoundedDirectoryEntries::LimitExceeded => {
                    return Err(LoadGluonError::DirectoryEntryLimit {
                        path: dir,
                        limit: MAX_GLUON_DIRECTORY_ENTRIES,
                    });
                }
            };
            let mut paths = Vec::new();
            for name in entries {
                let path = dir.join(&name);
                if Path::new(&name).extension() != Some(OsStr::new("glu")) {
                    continue;
                }
                let entry_metadata = directory
                    .metadata_at(&name)
                    .map_err(|source| LoadGluonError::Enumerate {
                        path: path.clone(),
                        source,
                    })?;
                if !entry_metadata.file_type().is_file() {
                    return Err(invalid_entry(
                        &path,
                        "matching Gluon fragment is not a real regular file",
                    ));
                }
                let logical_name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| LoadGluonError::Enumerate {
                        path: dir.clone(),
                        source: io::Error::new(io::ErrorKind::InvalidData, "Gluon fragment name is not UTF-8"),
                    })?
                    .to_owned();
                if !is_safe_fragment_name(&logical_name) {
                    return Err(invalid_entry(
                        &path,
                        "Gluon fragment name is not a safe normalized component",
                    ));
                }
                if paths.len() == remaining {
                    return Err(LoadGluonError::FragmentLimit {
                        limit: MAX_GLUON_FRAGMENTS,
                    });
                }
                let relative_path = relative_dir.join(&name);
                paths.push(GluonPath {
                    logical_name,
                    source_root_path: source_root_path.clone(),
                    source_root: source_root.clone(),
                    relative_path,
                    path,
                    collection: Some(collection.clone()),
                });
            }
            paths.sort_by(|left, right| left.logical_name.cmp(&right.logical_name));
            verify_collection(Some(&collection))?;
            verify_source_root(&source_root_path, &source_root)?;
            Ok(paths)
        }
    }
}

fn verify_source_root(path: &Path, expected: &SourceRoot) -> Result<(), LoadGluonError> {
    let current = SourceRoot::new(path).map_err(|source| LoadGluonError::Evaluation {
        path: path.to_owned(),
        source,
    })?;
    if &current == expected {
        Ok(())
    } else {
        Err(LoadGluonError::Enumerate {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Gluon source root changed while fragments were being loaded",
            ),
        })
    }
}

fn verify_collection(collection: Option<&CollectionIdentity>) -> Result<(), LoadGluonError> {
    let Some(collection) = collection else {
        return Ok(());
    };
    let metadata = fs::symlink_metadata(&collection.path).map_err(|source| LoadGluonError::Enumerate {
        path: collection.path.clone(),
        source,
    })?;
    if metadata.file_type().is_dir() && FileSnapshot::from_metadata(&metadata) == collection.identity {
        Ok(())
    } else {
        Err(LoadGluonError::Enumerate {
            path: collection.path.clone(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Gluon fragment collection changed while fragments were being loaded",
            ),
        })
    }
}

fn require_regular_fragment(path: &Path, metadata: &Metadata) -> Result<(), LoadGluonError> {
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(invalid_entry(
            path,
            "matching Gluon fragment is not a real regular file",
        ))
    }
}

fn invalid_entry(path: &Path, message: &'static str) -> LoadGluonError {
    LoadGluonError::Enumerate {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}

pub(super) fn is_safe_fragment_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > MAX_GLUON_FRAGMENT_NAME_BYTES
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == OsStr::new(name)
    )
}
