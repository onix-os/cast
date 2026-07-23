//! Descriptor-rooted loading for configuration embedded in retained trees.

use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, OsStr, OsString},
    fs::Metadata,
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::MetadataExt as _,
        },
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use fs_err::File;
use gluon_config::{Evaluator, SourceRoot};

use super::{
    Config as _, GluonCodec, GluonCodecError, LoadGluonError, LoadedGluonConfig,
    gluon::{MAX_GLUON_DIRECTORY_ENTRIES, MAX_GLUON_FRAGMENT_NAME_BYTES, MAX_GLUON_FRAGMENTS, is_safe_fragment_name},
};

const MAX_INTERRUPTED_OPEN_RETRIES: usize = 1_024;
const ROOTED_RESOLUTION: u64 =
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl NodeSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct RootedFragment {
    logical_name: String,
    relative_path: PathBuf,
    path: PathBuf,
    expected: NodeSnapshot,
    collection: Option<Arc<RetainedCollection>>,
}

#[derive(Debug)]
struct RetainedCollection {
    relative_path: PathBuf,
    path: PathBuf,
    descriptor: File,
    expected: NodeSnapshot,
}

/// Load one custom Gluon domain from an already-retained directory.
///
/// `root_path` is diagnostic only. Fragment enumeration, source reads, and
/// relative imports all stay beneath an owned duplicate of `root`, reject
/// links and mount crossings, and are revalidated against exact inode
/// snapshots before returning evaluated values.
pub fn load_gluon_rooted<C: GluonCodec>(
    root_path: &Path,
    root: &impl std::os::fd::AsRawFd,
    evaluator: &Evaluator,
    codec: &C,
) -> Result<Vec<LoadedGluonConfig<C::Config>>, LoadGluonError> {
    let root = openat2_file(
        root.as_raw_fd(),
        Path::new("."),
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        root_path,
    )
    .map_err(|source| LoadGluonError::Enumerate {
        path: root_path.to_owned(),
        source,
    })?;
    let root_metadata = root.metadata().map_err(|source| LoadGluonError::Enumerate {
        path: root_path.to_owned(),
        source,
    })?;
    if !root_metadata.file_type().is_dir() {
        return Err(invalid_entry(root_path, "rooted Gluon source is not a directory"));
    }
    let root_snapshot = NodeSnapshot::from_metadata(&root_metadata);

    let domain = C::Config::domain();
    let fragments = enumerate_fragments(root_path, &root, &domain)?;
    require_snapshot(root_path, &root, root_snapshot)?;

    let max_source_bytes = evaluator.limits().max_source_bytes;
    let mut loaded = BTreeMap::new();
    for fragment in fragments {
        let source_root =
            SourceRoot::from_directory(root_path, &root).map_err(|source| LoadGluonError::Evaluation {
                path: root_path.to_owned(),
                source,
            })?;
        require_collection(&root, fragment.collection.as_deref())?;
        require_named_snapshot(&root, &fragment.relative_path, &fragment.path, fragment.expected)?;
        let source = source_root
            .load(&fragment.relative_path, max_source_bytes)
            .map_err(|source| LoadGluonError::Evaluation {
                path: fragment.path.clone(),
                source,
            })?;
        require_collection(&root, fragment.collection.as_deref())?;
        require_named_snapshot(&root, &fragment.relative_path, &fragment.path, fragment.expected)?;
        require_snapshot(root_path, &root, root_snapshot)?;
        source_root
            .verify_retained_directories()
            .map_err(|source| LoadGluonError::Evaluation {
                path: fragment.path.clone(),
                source,
            })?;

        let evaluator = evaluator.clone().with_source_root(source_root.clone());
        let decoded = codec.decode(&evaluator, &source).map_err(|error| match error {
            GluonCodecError::Evaluation(source) => LoadGluonError::Evaluation {
                path: fragment.path.clone(),
                source,
            },
            GluonCodecError::Conversion(source) => LoadGluonError::Conversion {
                path: fragment.path.clone(),
                source,
            },
        })?;

        source_root
            .verify_retained_directories()
            .map_err(|source| LoadGluonError::Evaluation {
                path: fragment.path.clone(),
                source,
            })?;
        require_collection(&root, fragment.collection.as_deref())?;
        require_named_snapshot(&root, &fragment.relative_path, &fragment.path, fragment.expected)?;
        require_snapshot(root_path, &root, root_snapshot)?;
        loaded.insert(
            fragment.logical_name.clone(),
            LoadedGluonConfig {
                logical_name: fragment.logical_name,
                path: fragment.path,
                value: decoded.value,
                fingerprint: decoded.fingerprint,
            },
        );
    }
    require_snapshot(root_path, &root, root_snapshot)?;
    Ok(loaded.into_values().collect())
}

fn enumerate_fragments(root_path: &Path, root: &File, domain: &str) -> Result<Vec<RootedFragment>, LoadGluonError> {
    let mut fragments = Vec::new();
    let file_name = OsString::from(format!("{domain}.glu"));
    if let Some(fragment) = open_fragment(root_path, root, Path::new(&file_name), domain.to_owned(), None)? {
        fragments.push(fragment);
    }

    let directory_name = OsString::from(format!("{domain}.d"));
    let directory_path = root_path.join(&directory_name);
    let Some(directory) = open_optional(
        root,
        Path::new(&directory_name),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        &directory_path,
    )?
    else {
        return Ok(fragments);
    };
    let directory_snapshot = snapshot(&directory, &directory_path)?;
    let collection = Arc::new(RetainedCollection {
        relative_path: PathBuf::from(&directory_name),
        path: directory_path.clone(),
        descriptor: directory,
        expected: directory_snapshot,
    });
    let mut names = entry_names(&collection.descriptor, &directory_path)?;
    names.sort();

    for name in names {
        if Path::new(&name).extension() != Some(OsStr::new("glu")) {
            continue;
        }
        let logical_name = Path::new(&name)
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid_entry(&directory_path, "Gluon fragment name is not UTF-8"))?
            .to_owned();
        if !is_safe_fragment_name(&logical_name) || name.as_bytes().len() > MAX_GLUON_FRAGMENT_NAME_BYTES + 4 {
            return Err(invalid_entry(
                &directory_path.join(&name),
                "Gluon fragment name is not a safe normalized component",
            ));
        }
        if fragments.len() == MAX_GLUON_FRAGMENTS {
            return Err(LoadGluonError::FragmentLimit {
                limit: MAX_GLUON_FRAGMENTS,
            });
        }
        let relative_path = Path::new(&directory_name).join(&name);
        let fragment = open_fragment(
            root_path,
            root,
            &relative_path,
            logical_name,
            Some(Arc::clone(&collection)),
        )?
        .ok_or_else(|| {
            invalid_entry(
                &root_path.join(&relative_path),
                "Gluon fragment disappeared during rooted enumeration",
            )
        })?;
        fragments.push(fragment);
    }

    require_collection(root, Some(&collection))?;
    Ok(fragments)
}

fn open_fragment(
    root_path: &Path,
    root: &File,
    relative_path: &Path,
    logical_name: String,
    collection: Option<Arc<RetainedCollection>>,
) -> Result<Option<RootedFragment>, LoadGluonError> {
    let path = root_path.join(relative_path);
    let Some(file) = open_optional(
        root,
        relative_path,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        &path,
    )?
    else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(|source| LoadGluonError::Enumerate {
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(invalid_entry(&path, "matching Gluon fragment is not a regular file"));
    }
    Ok(Some(RootedFragment {
        logical_name,
        relative_path: relative_path.to_owned(),
        path,
        expected: NodeSnapshot::from_metadata(&metadata),
        collection,
    }))
}

fn entry_names(directory: &File, path: &Path) -> Result<Vec<OsString>, LoadGluonError> {
    let duplicate = openat2_file(
        directory.as_raw_fd(),
        Path::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        path,
    )
    .map_err(|source| LoadGluonError::Enumerate {
        path: path.to_owned(),
        source,
    })?;
    let descriptor: OwnedFd = duplicate.into();
    let raw_descriptor = descriptor.into_raw_fd();
    // SAFETY: `descriptor` is a fresh owned directory descriptor. fdopendir
    // consumes it on success; DirectoryStream closes it exactly once.
    let stream = unsafe { libc::fdopendir(raw_descriptor) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume the fd.
        unsafe {
            libc::close(raw_descriptor);
        }
        return Err(LoadGluonError::Enumerate {
            path: path.to_owned(),
            source,
        });
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local and is cleared immediately before the
        // exclusive readdir call on this private stream.
        unsafe {
            *libc::__errno_location() = 0;
        }
        // SAFETY: stream remains live and exclusively borrowed here.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(LoadGluonError::Enumerate {
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a live dirent with a NUL-terminated name.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if names.len() == MAX_GLUON_DIRECTORY_ENTRIES {
            return Err(LoadGluonError::DirectoryEntryLimit {
                path: path.to_owned(),
                limit: MAX_GLUON_DIRECTORY_ENTRIES,
            });
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    Ok(names)
}

fn require_named_snapshot(
    parent: &File,
    relative_path: &Path,
    path: &Path,
    expected: NodeSnapshot,
) -> Result<(), LoadGluonError> {
    let file = openat2_file(
        parent.as_raw_fd(),
        relative_path,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        path,
    )
    .map_err(|source| LoadGluonError::Enumerate {
        path: path.to_owned(),
        source,
    })?;
    require_snapshot(path, &file, expected)
}

fn require_collection(root: &File, collection: Option<&RetainedCollection>) -> Result<(), LoadGluonError> {
    let Some(collection) = collection else {
        return Ok(());
    };
    require_snapshot(&collection.path, &collection.descriptor, collection.expected)?;
    require_named_snapshot(root, &collection.relative_path, &collection.path, collection.expected)
}

fn require_snapshot(path: &Path, file: &File, expected: NodeSnapshot) -> Result<(), LoadGluonError> {
    if snapshot(file, path)? == expected {
        Ok(())
    } else {
        Err(invalid_entry(
            path,
            "rooted Gluon source changed while it was being loaded",
        ))
    }
}

fn snapshot(file: &File, path: &Path) -> Result<NodeSnapshot, LoadGluonError> {
    file.metadata()
        .map(|metadata| NodeSnapshot::from_metadata(&metadata))
        .map_err(|source| LoadGluonError::Enumerate {
            path: path.to_owned(),
            source,
        })
}

fn open_optional(parent: &File, relative_path: &Path, flags: i32, path: &Path) -> Result<Option<File>, LoadGluonError> {
    match openat2_file(parent.as_raw_fd(), relative_path, flags, path) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(LoadGluonError::Enumerate {
            path: path.to_owned(),
            source,
        }),
    }
}

fn openat2_file(dirfd: i32, relative_path: &Path, flags: i32, diagnostic_path: &Path) -> io::Result<File> {
    let path = CString::new(relative_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rooted Gluon path contains a NUL byte"))?;
    // SAFETY: every open_how field accepts zero before explicit assignment.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.resolve = ROOTED_RESOLUTION;
    let mut interruptions = 0usize;
    loop {
        // SAFETY: path and open_how remain live, and success returns one new fd.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dirfd,
                path.as_ptr(),
                &how,
                size_of::<libc::open_how>(),
            )
        };
        if descriptor != -1 {
            // SAFETY: openat2 returned a fresh owned descriptor.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
            return Ok(File::from_parts(descriptor.into(), diagnostic_path));
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
        if interruptions == MAX_INTERRUPTED_OPEN_RETRIES {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!(
                    "rooted Gluon open exceeded {MAX_INTERRUPTED_OPEN_RETRIES} interrupted retries at {}",
                    diagnostic_path.display()
                ),
            ));
        }
        interruptions += 1;
    }
}

fn invalid_entry(path: &Path, message: &'static str) -> LoadGluonError {
    LoadGluonError::Enumerate {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: fdopendir created this stream and it is closed once here.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use fs_err as fs;
    use gluon_config::Source;

    use super::*;
    use crate::{DecodedGluon, GluonCodecError};

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RootedValue(String);

    impl crate::Config for RootedValue {
        fn domain() -> String {
            "rooted".to_owned()
        }
    }

    struct RootedCodec;

    impl GluonCodec for RootedCodec {
        type Config = RootedValue;

        fn decode(
            &self,
            evaluator: &Evaluator,
            source: &Source,
        ) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
            let evaluated = evaluator.evaluate::<String>(source)?;
            Ok(DecodedGluon {
                value: RootedValue(evaluated.value),
                fingerprint: evaluated.fingerprint,
            })
        }

        fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
            Ok(format!("{:?}", config.0))
        }
    }

    struct ReplacingImportDirectoryCodec {
        collection: PathBuf,
        detached: PathBuf,
        replaced: Cell<bool>,
        injected_evaluated: Cell<bool>,
    }

    impl GluonCodec for ReplacingImportDirectoryCodec {
        type Config = RootedValue;

        fn decode(
            &self,
            evaluator: &Evaluator,
            source: &Source,
        ) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
            if source.logical_name() == "rooted.d/imported.glu" {
                let modules = self.collection.join("modules");
                fs::rename(&modules, &self.detached).unwrap();
                write(modules.join("value.glu"), "\"injected-import\"");
                self.replaced.set(true);
            }
            let evaluated = evaluator.evaluate::<String>(source)?;
            self.injected_evaluated.set(evaluated.value == "injected-import");
            Ok(DecodedGluon {
                value: RootedValue(evaluated.value),
                fingerprint: evaluated.fingerprint,
            })
        }

        fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
            Ok(format!("{:?}", config.0))
        }
    }

    fn write(path: impl AsRef<Path>, contents: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn rooted_load_uses_the_retained_tree_after_public_path_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let detached = temporary.path().join("detached");
        write(root.join("rooted.glu"), "\"retained-base\"");
        write(root.join("rooted.d/modules/value.glu"), "\"retained-import\"");
        write(root.join("rooted.d/imported.glu"), "import! \"modules/value.glu\"");
        let retained = File::open(&root).unwrap();

        fs::rename(&root, &detached).unwrap();
        write(root.join("rooted.glu"), "\"injected-base\"");
        write(root.join("rooted.d/injected.glu"), "\"injected-fragment\"");

        let loaded = load_gluon_rooted(&root, &retained, &Evaluator::default(), &RootedCodec).unwrap();
        assert_eq!(
            loaded
                .iter()
                .map(|fragment| (fragment.logical_name.as_str(), fragment.value.0.as_str()))
                .collect::<Vec<_>>(),
            [("imported", "retained-import"), ("rooted", "retained-base")]
        );
    }

    #[test]
    fn rooted_load_rejects_nested_import_directory_substitution_during_decode() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let collection = root.join("rooted.d");
        write(collection.join("modules/value.glu"), "\"retained-import\"");
        write(collection.join("imported.glu"), "import! \"modules/value.glu\"");
        let retained = File::open(&root).unwrap();
        let codec = ReplacingImportDirectoryCodec {
            collection: collection.clone(),
            detached: temporary.path().join("detached-modules"),
            replaced: Cell::new(false),
            injected_evaluated: Cell::new(false),
        };

        let error = load_gluon_rooted(&root, &retained, &Evaluator::default(), &codec).unwrap_err();

        assert!(codec.replaced.get());
        assert!(!codec.injected_evaluated.get());
        assert!(
            error.to_string().contains("evaluate Gluon fragment")
                || error.to_string().contains("enumerate Gluon fragments")
        );
        assert!(!error.to_string().contains("injected-import"));
    }
}
