// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fmt,
    fs::{File, Metadata},
    io::{self, Read},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileTypeExt, MetadataExt},
        },
    },
    path::{Component, Path, PathBuf},
    ptr::NonNull,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use astr::AStr;
use glob::Pattern;
use nix::libc::{self, S_IFDIR, S_IRGRP, S_IROTH, S_IRWXU, S_IXGRP, S_IXOTH};
use stone::{StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};
use stone_recipe::derivation::PathRuleKind;
use thiserror::Error as ThisError;

const HASH_BUFFER_BYTES: usize = 64 * 1024;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Finite resource ceilings for package-output discovery and verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionLimits {
    pub max_rules: usize,
    pub max_rule_pattern_bytes: usize,
    pub max_rule_package_bytes: usize,
    pub max_total_rule_pattern_bytes: u64,
    pub max_total_rule_package_bytes: u64,
    pub max_entries: u64,
    pub max_depth: usize,
    pub max_name_bytes: usize,
    pub max_path_bytes: usize,
    pub max_symlink_target_bytes: usize,
    pub max_total_name_bytes: u64,
    pub max_total_path_bytes: u64,
    pub max_total_symlink_target_bytes: u64,
    pub max_file_bytes: u64,
    pub max_total_regular_bytes: u64,
    pub max_duration: Duration,
}

impl Default for CollectionLimits {
    fn default() -> Self {
        Self {
            max_rules: 16_384,
            max_rule_pattern_bytes: 16 * 1024,
            max_rule_package_bytes: 4 * 1024,
            max_total_rule_pattern_bytes: 64 * MIB,
            max_total_rule_package_bytes: 16 * MIB,
            max_entries: 1_000_000,
            max_depth: 256,
            max_name_bytes: 4 * 1024,
            max_path_bytes: 64 * 1024,
            max_symlink_target_bytes: 64 * 1024,
            max_total_name_bytes: 512 * MIB,
            max_total_path_bytes: 512 * MIB,
            max_total_symlink_target_bytes: 512 * MIB,
            max_file_bytes: 64 * GIB,
            max_total_regular_bytes: 1024 * GIB,
            max_duration: Duration::from_secs(2 * 60 * 60),
        }
    }
}

#[derive(Debug)]
pub struct Rule {
    pattern: String,
    package: Arc<str>,
    kind: PathRuleKind,
    exact: Pattern,
    descendant: Pattern,
}

impl Rule {
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn kind(&self) -> PathRuleKind {
        self.kind
    }

    fn matches(&self, path: &str, metadata: &Metadata) -> bool {
        let pattern_matches = self.pattern == path || self.exact.matches(path) || self.descendant.matches(path);

        pattern_matches
            && match self.kind {
                PathRuleKind::Any => true,
                PathRuleKind::Executable => metadata.is_file() && metadata.mode() & 0o111 != 0,
                PathRuleKind::Symlink => metadata.file_type().is_symlink(),
                PathRuleKind::Special => {
                    let file_type = metadata.file_type();
                    file_type.is_char_device()
                        || file_type.is_block_device()
                        || file_type.is_fifo()
                        || file_type.is_socket()
                }
            }
    }
}

pub struct Collector {
    /// Rules stored in ascending priority order.
    rules: Vec<Rule>,
    root: PathBuf,
    limits: CollectionLimits,
    anchor: OnceLock<Arc<RootAnchor>>,
    deadline: OnceLock<Arc<Deadline>>,
    usage: Arc<Mutex<CollectionUsage>>,
    total_rule_pattern_bytes: u64,
    total_rule_package_bytes: u64,
    #[cfg(test)]
    hook: Option<Arc<dyn Fn(TestPoint, &Path) + Send + Sync>>,
}

impl fmt::Debug for Collector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Collector")
            .field("rules", &self.rules)
            .field("root", &self.root)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Collector {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::new_with_limits(root, CollectionLimits::default())
    }

    pub fn new_with_limits(root: impl Into<PathBuf>, limits: CollectionLimits) -> Self {
        Self {
            rules: Vec::new(),
            root: root.into(),
            limits,
            anchor: OnceLock::new(),
            deadline: OnceLock::new(),
            usage: Arc::new(Mutex::new(CollectionUsage::default())),
            total_rule_pattern_bytes: 0,
            total_rule_package_bytes: 0,
            #[cfg(test)]
            hook: None,
        }
    }

    pub fn add_rule(&mut self, pattern: &str, package: &str, kind: PathRuleKind) -> Result<(), Error> {
        enforce_usize_limit(
            "collection rules",
            self.limits.max_rules,
            self.rules.len().checked_add(1).ok_or(Error::ArithmeticOverflow {
                resource: "collection rules",
                path: self.root.clone(),
            })?,
            &self.root,
        )?;
        enforce_usize_limit(
            "rule pattern bytes",
            self.limits.max_rule_pattern_bytes,
            pattern.len(),
            &self.root,
        )?;
        enforce_usize_limit(
            "rule package bytes",
            self.limits.max_rule_package_bytes,
            package.len(),
            &self.root,
        )?;
        let total_pattern = checked_add_limit(
            "total rule pattern bytes",
            self.total_rule_pattern_bytes,
            u64::try_from(pattern.len()).map_err(|_| Error::ArithmeticOverflow {
                resource: "total rule pattern bytes",
                path: self.root.clone(),
            })?,
            self.limits.max_total_rule_pattern_bytes,
            &self.root,
        )?;
        let total_package = checked_add_limit(
            "total rule package bytes",
            self.total_rule_package_bytes,
            u64::try_from(package.len()).map_err(|_| Error::ArithmeticOverflow {
                resource: "total rule package bytes",
                path: self.root.clone(),
            })?,
            self.limits.max_total_rule_package_bytes,
            &self.root,
        )?;
        let pattern_owned = copy_string(pattern, "rule pattern bytes")?;
        let mut descendant_owned = copy_string(pattern.strip_suffix('/').unwrap_or(pattern), "rule pattern bytes")?;
        descendant_owned.try_reserve(3).map_err(|source| Error::Allocation {
            resource: "descendant rule pattern bytes",
            requested: 3,
            detail: source.to_string(),
        })?;
        descendant_owned.push_str("/**");
        let exact = Pattern::new(&pattern_owned).map_err(|source| Error::InvalidRulePattern {
            pattern: pattern_owned.clone(),
            detail: source.to_string(),
        })?;
        let descendant = Pattern::new(&descendant_owned).map_err(|source| Error::InvalidRulePattern {
            pattern: descendant_owned,
            detail: source.to_string(),
        })?;
        let package = if let Some(rule) = self.rules.iter().find(|rule| rule.package.as_ref() == package) {
            Arc::clone(&rule.package)
        } else {
            Arc::<str>::from(copy_string(package, "rule package bytes")?)
        };
        reserve(&mut self.rules, 1, "collection rules")?;
        self.rules.push(Rule {
            pattern: pattern_owned,
            package,
            kind,
            exact,
            descendant,
        });
        self.total_rule_pattern_bytes = total_pattern;
        self.total_rule_package_bytes = total_package;
        Ok(())
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    fn deadline(&self) -> Arc<Deadline> {
        Arc::clone(
            self.deadline
                .get_or_init(|| Arc::new(Deadline::new(self.limits.max_duration))),
        )
    }

    fn anchor(&self) -> Result<Arc<RootAnchor>, Error> {
        if let Some(anchor) = self.anchor.get() {
            anchor.verify_path_node()?;
            return Ok(Arc::clone(anchor));
        }

        let opened = Arc::new(RootAnchor::open(&self.root)?);
        let _ = self.anchor.set(Arc::clone(&opened));
        let anchor = self.anchor.get().map_or(opened, Arc::clone);
        anchor.verify_path_node()?;
        Ok(anchor)
    }

    fn matching_package(
        &self,
        context: &CollectionContext,
        path: &str,
        metadata: &Metadata,
        display_path: &Path,
    ) -> Result<Option<Arc<str>>, Error> {
        for rule in self.rules.iter().rev() {
            context.check_time(display_path)?;
            if rule.matches(path, metadata) {
                context.check_time(display_path)?;
                return Ok(Some(Arc::clone(&rule.package)));
            }
        }
        context.check_time(display_path)?;
        Ok(None)
    }

    /// Produce a verified [`PathInfo`] for a path beneath this collector's root.
    pub fn path(&self, path: &Path, hasher: &mut StoneDigestWriterHasher) -> Result<PathInfo, Error> {
        let deadline = self.deadline();
        deadline.check(path)?;
        let anchor = self.anchor()?;
        let relative = relative_to_root(&self.root, path)?;
        let depth = relative.components().count();
        let context = CollectionContext::new(self.limits, Arc::clone(&self.usage), Arc::clone(&deadline));
        context.admit_entry(&relative, depth, path)?;

        let (parent_relative, name) = split_parent_name(&relative, path)?;
        let parent_file = anchor.open_directory(&parent_relative)?;
        let parent_snapshot = FileSnapshot::from_metadata(&metadata(&parent_file, "stat package parent", path)?);
        let parent = Arc::new(DirectoryHandle {
            file: parent_file,
            relative: parent_relative,
            display_path: path.parent().unwrap_or(&self.root).to_owned(),
            snapshot: parent_snapshot,
            anchor,
        });

        self.visit_path(&context, parent, name, relative, depth, hasher)
    }

    /// Enumerate the output tree with descriptor-anchored, deterministic,
    /// iterative traversal.
    pub fn enumerate_paths(
        &self,
        subdir: Option<(PathBuf, Metadata)>,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<Vec<PathInfo>, Error> {
        let deadline = self.deadline();
        deadline.check(&self.root)?;
        let anchor = self.anchor()?;
        let context = CollectionContext::new(self.limits, Arc::clone(&self.usage), Arc::clone(&deadline));
        let (relative, display_path, include_directory, supplied_metadata) = match subdir {
            Some((path, metadata)) => (relative_to_root(&self.root, &path)?, path, true, Some(metadata)),
            None => (PathBuf::new(), self.root.clone(), false, None),
        };
        let depth = relative.components().count();
        if include_directory {
            context.admit_entry(&relative, depth, &display_path)?;
        } else {
            context.check_depth(depth, &display_path)?;
        }

        let file = anchor.open_directory(&relative)?;
        let directory_metadata = metadata(&file, "stat package directory", &display_path)?;
        if let Some(supplied) = supplied_metadata
            && FileSnapshot::from_metadata(&supplied) != FileSnapshot::from_metadata(&directory_metadata)
        {
            return Err(changed(
                &display_path,
                "supplied directory identity or metadata no longer matches",
            ));
        }
        let directory = Arc::new(DirectoryHandle {
            file,
            relative,
            display_path,
            snapshot: FileSnapshot::from_metadata(&directory_metadata),
            anchor,
        });

        let mut output = Vec::new();
        let mut tasks = Vec::new();
        reserve(&mut tasks, 1, "traversal tasks")?;
        tasks.push(Task::Scan {
            directory,
            include_directory,
            depth,
        });

        while let Some(task) = tasks.pop() {
            context.check_time(task.path())?;
            match task {
                Task::Scan {
                    directory,
                    include_directory,
                    depth,
                } => {
                    let names = read_directory_names(&directory, &context)?;
                    let output_start = output.len();
                    let task_count = names.len().checked_add(1).ok_or(Error::ArithmeticOverflow {
                        resource: "traversal tasks",
                        path: directory.display_path.clone(),
                    })?;
                    let child_depth = depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                        resource: "path depth",
                        path: directory.display_path.clone(),
                    })?;
                    reserve(&mut tasks, task_count, "traversal tasks")?;
                    tasks.push(Task::Finalize {
                        directory: Arc::clone(&directory),
                        include_directory,
                        output_start,
                    });
                    for name in names.into_iter().rev() {
                        let relative = join_relative(&directory.relative, &name);
                        tasks.push(Task::Visit {
                            parent: Arc::clone(&directory),
                            name,
                            relative,
                            depth: child_depth,
                        });
                    }
                }
                Task::Visit {
                    parent,
                    name,
                    relative,
                    depth,
                } => {
                    let display_path = self.root.join(&relative);
                    let handle = open_entry_handle(&parent.file, &name, &display_path)?;
                    let entry_metadata = metadata(&handle, "stat package entry", &display_path)?;
                    let entry_snapshot = FileSnapshot::from_metadata(&entry_metadata);
                    self.fire_hook(TestPoint::AfterEntryHandle, &display_path);

                    if entry_metadata.file_type().is_dir() {
                        let file = open_entry(
                            &parent.file,
                            &name,
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                            &display_path,
                        )?;
                        let opened = metadata(&file, "stat opened package directory", &display_path)?;
                        require_snapshot(&display_path, entry_snapshot, &opened)?;
                        verify_directory_collection(&parent)?;
                        self.fire_hook(TestPoint::AfterDirectoryOpen, &display_path);
                        reserve(&mut tasks, 1, "traversal tasks")?;
                        tasks.push(Task::Scan {
                            directory: Arc::new(DirectoryHandle {
                                file,
                                relative,
                                display_path,
                                snapshot: entry_snapshot,
                                anchor: Arc::clone(&parent.anchor),
                            }),
                            include_directory: true,
                            depth,
                        });
                    } else {
                        let info = self.path_info_from_handle(
                            &context,
                            parent,
                            handle,
                            entry_metadata,
                            entry_snapshot,
                            name,
                            relative,
                            depth,
                            hasher,
                        )?;
                        reserve(&mut output, 1, "collected paths")?;
                        output.push(info);
                    }
                }
                Task::Finalize {
                    directory,
                    include_directory,
                    output_start,
                } => {
                    verify_directory_collection(&directory)?;
                    if include_directory {
                        const REGULAR_DIR_MODE: u32 = S_IFDIR | S_IROTH | S_IXOTH | S_IRGRP | S_IXGRP | S_IRWXU;
                        let is_special = directory.snapshot.mode != REGULAR_DIR_MODE;
                        if output.len() == output_start || is_special {
                            let info = self.directory_path_info(&context, directory)?;
                            reserve(&mut output, 1, "collected paths")?;
                            output.push(info);
                        }
                    }
                }
            }
        }

        Ok(output)
    }

    fn visit_path(
        &self,
        context: &CollectionContext,
        parent: Arc<DirectoryHandle>,
        name: OsString,
        relative: PathBuf,
        depth: usize,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<PathInfo, Error> {
        let display_path = self.root.join(&relative);
        let handle = open_entry_handle(&parent.file, &name, &display_path)?;
        let entry_metadata = metadata(&handle, "stat package entry", &display_path)?;
        let entry_snapshot = FileSnapshot::from_metadata(&entry_metadata);
        self.fire_hook(TestPoint::AfterEntryHandle, &display_path);

        if entry_metadata.file_type().is_dir() {
            let directory = open_entry(
                &parent.file,
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                &display_path,
            )?;
            require_snapshot(
                &display_path,
                entry_snapshot,
                &metadata(&directory, "stat package directory", &display_path)?,
            )?;
            verify_directory_collection(&parent)?;
            let package = self.package_for(context, &relative, &entry_metadata, &display_path)?;
            let layout = layout_from_metadata(&relative, &entry_metadata, None, None)?;
            Ok(PathInfo::verified(
                display_path,
                relative,
                layout,
                entry_metadata.len(),
                package,
                VerifiedPath::new(
                    &parent,
                    name,
                    entry_snapshot,
                    VerifiedKind::Directory,
                    self.limits,
                    Arc::clone(&self.usage),
                    Arc::clone(&context.deadline),
                ),
            ))
        } else {
            self.path_info_from_handle(
                context,
                parent,
                handle,
                entry_metadata,
                entry_snapshot,
                name,
                relative,
                depth,
                hasher,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn path_info_from_handle(
        &self,
        context: &CollectionContext,
        parent: Arc<DirectoryHandle>,
        handle: File,
        entry_metadata: Metadata,
        entry_snapshot: FileSnapshot,
        name: OsString,
        relative: PathBuf,
        depth: usize,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<PathInfo, Error> {
        let display_path = self.root.join(&relative);
        context.check_depth(depth, &display_path)?;
        let package = self.package_for(context, &relative, &entry_metadata, &display_path)?;
        let file_type = entry_metadata.file_type();

        let (layout, kind) = if file_type.is_symlink() {
            let target = read_symlink_handle(&handle, &display_path, context)?;
            verify_entry_collection(&parent, &name, entry_snapshot, &display_path)?;
            (
                layout_from_metadata(&relative, &entry_metadata, Some(&target), None)?,
                VerifiedKind::Symlink { target },
            )
        } else if file_type.is_file() {
            context.admit_regular(entry_metadata.len(), &display_path)?;
            let hash = self.hash_regular(context, &parent, &name, entry_snapshot, &display_path, hasher)?;
            (
                layout_from_metadata(&relative, &entry_metadata, None, Some(hash))?,
                VerifiedKind::Regular { hash },
            )
        } else if is_supported_special(&file_type) {
            verify_entry_collection(&parent, &name, entry_snapshot, &display_path)?;
            (
                layout_from_metadata(&relative, &entry_metadata, None, None)?,
                VerifiedKind::Special,
            )
        } else {
            return Err(Error::UnsupportedFileType {
                path: display_path,
                kind: "unknown special inode",
            });
        };

        Ok(PathInfo::verified(
            display_path,
            relative,
            layout,
            entry_metadata.len(),
            package,
            VerifiedPath::new(
                &parent,
                name,
                entry_snapshot,
                kind,
                self.limits,
                Arc::clone(&self.usage),
                Arc::clone(&context.deadline),
            ),
        ))
    }

    fn directory_path_info(
        &self,
        context: &CollectionContext,
        directory: Arc<DirectoryHandle>,
    ) -> Result<PathInfo, Error> {
        let directory_metadata = metadata(
            &directory.file,
            "stat collected package directory",
            &directory.display_path,
        )?;
        require_snapshot(&directory.display_path, directory.snapshot, &directory_metadata)?;
        let package = self.package_for(
            context,
            &directory.relative,
            &directory_metadata,
            &directory.display_path,
        )?;
        let layout = layout_from_metadata(&directory.relative, &directory_metadata, None, None)?;
        let (parent_relative, name) = split_parent_name(&directory.relative, &directory.display_path)?;
        let parent_file = directory.anchor.open_directory(&parent_relative)?;
        let parent_snapshot = FileSnapshot::from_metadata(&metadata(
            &parent_file,
            "stat collected directory parent",
            &directory.display_path,
        )?);
        let parent = Arc::new(DirectoryHandle {
            file: parent_file,
            relative: parent_relative,
            display_path: directory.display_path.parent().unwrap_or(&self.root).to_owned(),
            snapshot: parent_snapshot,
            anchor: Arc::clone(&directory.anchor),
        });

        Ok(PathInfo::verified(
            directory.display_path.clone(),
            directory.relative.clone(),
            layout,
            directory_metadata.len(),
            package,
            VerifiedPath::new(
                &parent,
                name,
                directory.snapshot,
                VerifiedKind::Directory,
                self.limits,
                Arc::clone(&self.usage),
                Arc::clone(&context.deadline),
            ),
        ))
    }

    fn package_for(
        &self,
        context: &CollectionContext,
        relative: &Path,
        metadata: &Metadata,
        display_path: &Path,
    ) -> Result<Arc<str>, Error> {
        let target_path = Path::new("/").join(relative);
        let target = target_path.to_str().ok_or_else(|| Error::NonUtf8Path {
            path: display_path.to_owned(),
        })?;
        self.matching_package(context, target, metadata, display_path)?
            .ok_or_else(|| Error::NoMatchingRule {
                path: display_path.to_owned(),
            })
    }

    fn hash_regular(
        &self,
        context: &CollectionContext,
        parent: &DirectoryHandle,
        name: &OsStr,
        expected: FileSnapshot,
        display_path: &Path,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<u128, Error> {
        let mut file = open_entry(
            &parent.file,
            name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            display_path,
        )?;
        let opened = metadata(&file, "stat opened package file", display_path)?;
        if !opened.file_type().is_file() {
            return Err(changed(
                display_path,
                "entry stopped being a regular file before hashing",
            ));
        }
        require_snapshot(display_path, expected, &opened)?;
        self.fire_hook(TestPoint::AfterRegularOpen, display_path);

        hasher.reset();
        let mut buffer = [0u8; HASH_BUFFER_BYTES];
        let mut bytes = 0u64;
        loop {
            context.check_time(display_path)?;
            let read = file.read(&mut buffer).map_err(|source| Error::Io {
                operation: "hash package file",
                path: display_path.to_owned(),
                source,
            })?;
            if read == 0 {
                break;
            }
            bytes = bytes.checked_add(read as u64).ok_or(Error::ArithmeticOverflow {
                resource: "regular file bytes",
                path: display_path.to_owned(),
            })?;
            if bytes > expected.size {
                return Err(changed(display_path, "regular file grew while it was being hashed"));
            }
            hasher.update(&buffer[..read]);
        }
        if bytes != expected.size {
            return Err(changed(
                display_path,
                "regular file size changed while it was being hashed",
            ));
        }

        self.fire_hook(TestPoint::AfterRegularHash, display_path);
        require_snapshot(
            display_path,
            expected,
            &metadata(&file, "restat hashed package file", display_path)?,
        )?;
        verify_entry_collection(parent, name, expected, display_path)?;
        Ok(hasher.digest128())
    }

    #[cfg(test)]
    fn fire_hook(&self, point: TestPoint, path: &Path) {
        if let Some(hook) = &self.hook {
            hook(point, path);
        }
    }

    #[cfg(not(test))]
    fn fire_hook(&self, _point: TestPoint, _path: &Path) {}
}

#[derive(Debug)]
enum Task {
    Scan {
        directory: Arc<DirectoryHandle>,
        include_directory: bool,
        depth: usize,
    },
    Visit {
        parent: Arc<DirectoryHandle>,
        name: OsString,
        relative: PathBuf,
        depth: usize,
    },
    Finalize {
        directory: Arc<DirectoryHandle>,
        include_directory: bool,
        output_start: usize,
    },
}

impl Task {
    fn path(&self) -> &Path {
        match self {
            Task::Scan { directory, .. } | Task::Finalize { directory, .. } => &directory.display_path,
            Task::Visit { parent, .. } => &parent.display_path,
        }
    }
}

#[derive(Debug)]
struct DirectoryHandle {
    file: File,
    relative: PathBuf,
    display_path: PathBuf,
    snapshot: FileSnapshot,
    anchor: Arc<RootAnchor>,
}

#[derive(Debug)]
struct RootAnchor {
    file: File,
    path: PathBuf,
    identity: NodeIdentity,
}

impl RootAnchor {
    fn open(path: &Path) -> Result<Self, Error> {
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            path,
        )?;
        let metadata = metadata(&file, "stat package root", path)?;
        if !metadata.file_type().is_dir() {
            return Err(Error::UnsupportedFileType {
                path: path.to_owned(),
                kind: "non-directory root",
            });
        }
        Ok(Self {
            file,
            path: path.to_owned(),
            identity: NodeIdentity::from_metadata(&metadata),
        })
    }

    fn verify_path_node(&self) -> Result<(), Error> {
        let reopened = openat2_file(
            libc::AT_FDCWD,
            self.path.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &self.path,
        )?;
        let current = NodeIdentity::from_metadata(&metadata(&reopened, "verify package root", &self.path)?);
        if current == self.identity {
            Ok(())
        } else {
            Err(changed(&self.path, "package root was replaced"))
        }
    }

    fn open_directory(&self, relative: &Path) -> Result<File, Error> {
        self.verify_path_node()?;
        let path = if relative.as_os_str().is_empty() {
            OsStr::new(".")
        } else {
            relative.as_os_str()
        };
        let display_path = self.path.join(relative);
        openat2_file(
            self.file.as_raw_fd(),
            path.as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &display_path,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeIdentity {
    device: u64,
    inode: u64,
}

impl NodeIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    node: NodeIdentity,
    size: u64,
    ctime: i64,
    ctime_nsec: i64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
}

impl FileSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            node: NodeIdentity::from_metadata(metadata),
            size: metadata.len(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
        }
    }
}

#[derive(Debug, Clone)]
enum VerifiedKind {
    Regular { hash: u128 },
    Symlink { target: String },
    Directory,
    Special,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedPath {
    anchor: Arc<RootAnchor>,
    parent_relative: PathBuf,
    parent_identity: NodeIdentity,
    name: OsString,
    snapshot: FileSnapshot,
    kind: VerifiedKind,
    limits: CollectionLimits,
    usage: Arc<Mutex<CollectionUsage>>,
    deadline: Arc<Deadline>,
}

impl VerifiedPath {
    fn new(
        parent: &DirectoryHandle,
        name: OsString,
        snapshot: FileSnapshot,
        kind: VerifiedKind,
        limits: CollectionLimits,
        usage: Arc<Mutex<CollectionUsage>>,
        deadline: Arc<Deadline>,
    ) -> Self {
        Self {
            anchor: Arc::clone(&parent.anchor),
            parent_relative: parent.relative.clone(),
            parent_identity: parent.snapshot.node,
            name,
            snapshot,
            kind,
            limits,
            usage,
            deadline,
        }
    }

    fn display_path(&self) -> PathBuf {
        self.anchor.path.join(&self.parent_relative).join(&self.name)
    }

    fn open_parent(&self, operation: &'static str) -> Result<File, Error> {
        let path = self.display_path();
        self.deadline.check(&path)?;
        self.anchor.verify_path_node()?;
        let parent = self.anchor.open_directory(&self.parent_relative)?;
        let parent_metadata = metadata(&parent, operation, &path)?;
        if NodeIdentity::from_metadata(&parent_metadata) != self.parent_identity {
            return Err(changed(&path, "package entry parent was replaced"));
        }
        self.deadline.check(&path)?;
        Ok(parent)
    }

    fn verify(&self) -> Result<(), Error> {
        let path = self.display_path();
        let parent = self.open_parent("verify package entry parent")?;
        let handle = open_entry_handle(&parent, &self.name, &path)?;
        let current = metadata(&handle, "verify collected package entry", &path)?;
        require_snapshot(&path, self.snapshot, &current)?;
        match &self.kind {
            VerifiedKind::Regular { .. } if !current.file_type().is_file() => {
                Err(changed(&path, "collected regular file changed type"))
            }
            VerifiedKind::Symlink { target } => {
                if !current.file_type().is_symlink() {
                    return Err(changed(&path, "collected symlink changed type"));
                }
                let context = CollectionContext::detached(self.limits, Arc::clone(&self.deadline));
                let current_target = read_symlink_handle(&handle, &path, &context)?;
                if &current_target == target {
                    Ok(())
                } else {
                    Err(changed(&path, "collected symlink target changed"))
                }
            }
            VerifiedKind::Directory if !current.file_type().is_dir() => {
                Err(changed(&path, "collected directory changed type"))
            }
            VerifiedKind::Special if !is_supported_special(&current.file_type()) => {
                Err(changed(&path, "collected special entry changed type"))
            }
            _ => Ok(()),
        }
    }

    fn open_regular(&self) -> Result<VerifiedFileReader, Error> {
        let path = self.display_path();
        let expected_hash = match self.kind {
            VerifiedKind::Regular { hash } => hash,
            _ => return Err(Error::UnverifiedContent { path }),
        };
        self.verify()?;
        let parent = self.open_parent("open verified package parent")?;
        let parent_metadata = metadata(&parent, "open verified package parent", &path)?;
        if NodeIdentity::from_metadata(&parent_metadata) != self.parent_identity {
            return Err(changed(&path, "package file parent was replaced before emission"));
        }
        let file = open_entry(
            &parent,
            &self.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            &path,
        )?;
        let opened = metadata(&file, "stat verified package content", &path)?;
        if !opened.file_type().is_file() {
            return Err(changed(&path, "package content stopped being a regular file"));
        }
        require_snapshot(&path, self.snapshot, &opened)?;
        Ok(VerifiedFileReader {
            file,
            parent,
            anchor: Arc::clone(&self.anchor),
            parent_relative: self.parent_relative.clone(),
            parent_identity: self.parent_identity,
            name: self.name.clone(),
            path,
            expected: self.snapshot,
            expected_hash,
            hasher: StoneDigestWriterHasher::new(),
            bytes: 0,
            deadline: Arc::clone(&self.deadline),
            exceeded: false,
        })
    }

    fn restat_regular(
        &mut self,
        layout: &mut StonePayloadLayoutRecord,
        size: &mut u64,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<(), Error> {
        let path = self.display_path();
        if !matches!(self.kind, VerifiedKind::Regular { .. }) {
            return Err(Error::UnverifiedContent { path });
        }
        let parent = self.open_parent("restat package parent")?;
        let file = open_entry_handle(&parent, &self.name, &path)?;
        let current_metadata = metadata(&file, "restat analyzed package file", &path)?;
        if !current_metadata.file_type().is_file() {
            return Err(changed(&path, "analyzed package path is no longer a regular file"));
        }
        let new_snapshot = FileSnapshot::from_metadata(&current_metadata);
        enforce_u64_limit(
            "regular file bytes",
            self.limits.max_file_bytes,
            new_snapshot.size,
            &path,
        )?;

        let context = CollectionContext::detached(self.limits, Arc::clone(&self.deadline));
        let mut opened = open_entry(
            &parent,
            &self.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            &path,
        )?;
        require_snapshot(
            &path,
            new_snapshot,
            &metadata(&opened, "stat analyzed package file", &path)?,
        )?;
        hasher.reset();
        let mut buffer = [0u8; HASH_BUFFER_BYTES];
        let mut bytes_read = 0u64;
        loop {
            context.check_time(&path)?;
            let read = opened.read(&mut buffer).map_err(|source| Error::Io {
                operation: "rehash analyzed package file",
                path: path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            bytes_read = bytes_read.checked_add(read as u64).ok_or(Error::ArithmeticOverflow {
                resource: "regular file bytes",
                path: path.clone(),
            })?;
            if bytes_read > new_snapshot.size {
                return Err(changed(&path, "analyzed package file grew while rehashing"));
            }
            hasher.update(&buffer[..read]);
        }
        if bytes_read != new_snapshot.size {
            return Err(changed(&path, "analyzed package file changed size while rehashing"));
        }
        require_snapshot(
            &path,
            new_snapshot,
            &metadata(&opened, "verify analyzed package file", &path)?,
        )?;
        let reopened = open_entry_handle(&parent, &self.name, &path)?;
        require_snapshot(
            &path,
            new_snapshot,
            &metadata(&reopened, "verify analyzed package path", &path)?,
        )?;

        replace_accounted_regular(&self.usage, self.snapshot.size, new_snapshot.size, self.limits, &path)?;
        let hash = hasher.digest128();
        let target = match &layout.file {
            StonePayloadLayoutFile::Regular(_, target) => target.clone(),
            _ => return Err(Error::UnverifiedContent { path }),
        };
        layout.uid = current_metadata.uid();
        layout.gid = current_metadata.gid();
        layout.mode = current_metadata.mode();
        layout.file = StonePayloadLayoutFile::Regular(hash, target);
        *size = new_snapshot.size;
        self.snapshot = new_snapshot;
        self.kind = VerifiedKind::Regular { hash };
        Ok(())
    }
}

#[derive(Debug)]
pub struct PathInfo {
    pub path: PathBuf,
    pub target_path: PathBuf,
    pub layout: StonePayloadLayoutRecord,
    pub size: u64,
    pub package: Arc<str>,
    pub(crate) verified: Option<VerifiedPath>,
}

impl PathInfo {
    fn verified(
        path: PathBuf,
        relative: PathBuf,
        layout: StonePayloadLayoutRecord,
        size: u64,
        package: Arc<str>,
        verified: VerifiedPath,
    ) -> Self {
        Self {
            path,
            target_path: Path::new("/").join(relative),
            layout,
            size,
            package,
            verified: Some(verified),
        }
    }

    pub fn restat(&mut self, hasher: &mut StoneDigestWriterHasher) -> Result<(), Error> {
        self.verified
            .as_mut()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .restat_regular(&mut self.layout, &mut self.size, hasher)
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .verify()
    }

    pub(crate) fn open_verified(&self) -> Result<VerifiedFileReader, Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .open_regular()
    }

    pub fn is_file(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Regular(..))
    }

    pub fn file_hash(&self) -> Option<u128> {
        if let StonePayloadLayoutFile::Regular(hash, _) = &self.layout.file {
            Some(*hash)
        } else {
            None
        }
    }

    pub fn file_name(&self) -> &str {
        self.target_path
            .file_name()
            .and_then(|path| path.to_str())
            .unwrap_or_default()
    }

    pub fn has_component(&self, component: &str) -> bool {
        self.target_path
            .components()
            .any(|path| path.as_os_str() == OsStr::new(component))
    }
}

pub(crate) struct VerifiedFileReader {
    file: File,
    parent: File,
    anchor: Arc<RootAnchor>,
    parent_relative: PathBuf,
    parent_identity: NodeIdentity,
    name: OsString,
    path: PathBuf,
    expected: FileSnapshot,
    expected_hash: u128,
    hasher: StoneDigestWriterHasher,
    bytes: u64,
    deadline: Arc<Deadline>,
    exceeded: bool,
}

impl VerifiedFileReader {
    pub(crate) fn finish(self) -> Result<(), Error> {
        self.deadline.check(&self.path)?;
        if self.exceeded || self.bytes != self.expected.size {
            return Err(Error::ContentLengthChanged {
                path: self.path,
                expected: self.expected.size,
                actual: self.bytes.saturating_add(u64::from(self.exceeded)),
            });
        }
        let actual_hash = self.hasher.digest128();
        if actual_hash != self.expected_hash {
            return Err(Error::ContentHashChanged {
                path: self.path,
                expected: self.expected_hash,
                actual: actual_hash,
            });
        }
        require_snapshot(
            &self.path,
            self.expected,
            &metadata(&self.file, "verify emitted package file", &self.path)?,
        )?;
        if NodeIdentity::from_metadata(&metadata(&self.parent, "verify emitted package parent", &self.path)?)
            != self.parent_identity
        {
            return Err(changed(&self.path, "package file parent changed during emission"));
        }
        self.anchor.verify_path_node()?;
        let parent = self.anchor.open_directory(&self.parent_relative)?;
        if NodeIdentity::from_metadata(&metadata(&parent, "reopen emitted package parent", &self.path)?)
            != self.parent_identity
        {
            return Err(changed(&self.path, "package file parent was replaced during emission"));
        }
        let reopened = open_entry_handle(&parent, &self.name, &self.path)?;
        require_snapshot(
            &self.path,
            self.expected,
            &metadata(&reopened, "reopen emitted package file", &self.path)?,
        )
    }
}

impl Read for VerifiedFileReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.deadline.check(&self.path) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, error));
        }
        if self.bytes == self.expected.size {
            let mut probe = [0u8; 1];
            return match self.file.read(&mut probe)? {
                0 => Ok(0),
                _ => {
                    self.exceeded = true;
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "package file grew during verified emission",
                    ))
                }
            };
        }

        let remaining = self.expected.size - self.bytes;
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = self.file.read(&mut buffer[..allowed])?;
        if read != 0 {
            self.bytes = self
                .bytes
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "package byte count overflow"))?;
            self.hasher.update(&buffer[..read]);
        }
        Ok(read)
    }
}

fn layout_from_metadata(
    relative: &Path,
    metadata: &Metadata,
    symlink_target: Option<&str>,
    regular_hash: Option<u128>,
) -> Result<StonePayloadLayoutRecord, Error> {
    let full_target_path = Path::new("/").join(relative);
    let target_path = full_target_path.strip_prefix("/usr").unwrap_or(&full_target_path);
    let target: AStr = target_path
        .to_str()
        .ok_or_else(|| Error::NonUtf8Path {
            path: Path::new("/").join(relative),
        })?
        .into();
    let file_type = metadata.file_type();
    let file = if file_type.is_symlink() {
        let source = symlink_target.ok_or_else(|| Error::TreeChanged {
            path: full_target_path.clone(),
            detail: "symlink target was not captured",
        })?;
        StonePayloadLayoutFile::Symlink(source.into(), target)
    } else if file_type.is_dir() {
        StonePayloadLayoutFile::Directory(target)
    } else if file_type.is_char_device() {
        StonePayloadLayoutFile::CharacterDevice(target)
    } else if file_type.is_block_device() {
        StonePayloadLayoutFile::BlockDevice(target)
    } else if file_type.is_fifo() {
        StonePayloadLayoutFile::Fifo(target)
    } else if file_type.is_socket() {
        StonePayloadLayoutFile::Socket(target)
    } else if file_type.is_file() {
        StonePayloadLayoutFile::Regular(
            regular_hash.ok_or_else(|| Error::TreeChanged {
                path: full_target_path.clone(),
                detail: "regular file hash was not captured",
            })?,
            target,
        )
    } else {
        return Err(Error::UnsupportedFileType {
            path: full_target_path,
            kind: "unknown special inode",
        });
    };

    Ok(StonePayloadLayoutRecord {
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode(),
        tag: 0,
        file,
    })
}

#[derive(Debug, Default)]
struct CollectionUsage {
    entries: u64,
    name_bytes: u64,
    path_bytes: u64,
    symlink_target_bytes: u64,
    regular_bytes: u64,
}

struct CollectionContext {
    limits: CollectionLimits,
    usage: Option<Arc<Mutex<CollectionUsage>>>,
    deadline: Arc<Deadline>,
}

impl CollectionContext {
    fn new(limits: CollectionLimits, usage: Arc<Mutex<CollectionUsage>>, deadline: Arc<Deadline>) -> Self {
        Self {
            limits,
            usage: Some(usage),
            deadline,
        }
    }

    fn detached(limits: CollectionLimits, deadline: Arc<Deadline>) -> Self {
        Self {
            limits,
            usage: None,
            deadline,
        }
    }

    fn check_time(&self, path: &Path) -> Result<(), Error> {
        self.deadline.check(path)
    }

    fn check_depth(&self, depth: usize, path: &Path) -> Result<(), Error> {
        enforce_usize_limit("path depth", self.limits.max_depth, depth, path)
    }

    fn admit_entry(&self, relative: &Path, depth: usize, display_path: &Path) -> Result<(), Error> {
        self.check_time(display_path)?;
        self.check_depth(depth, display_path)?;
        let path_bytes = relative.as_os_str().as_bytes().len();
        let name_bytes = relative
            .file_name()
            .map(|name| name.as_bytes().len())
            .unwrap_or_default();
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, display_path)?;
        enforce_usize_limit("entry path bytes", self.limits.max_path_bytes, path_bytes, display_path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        update_entry_usage(&mut usage, self.limits, name_bytes, path_bytes, display_path)?;
        Ok(())
    }

    fn admit_regular(&self, bytes: u64, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_u64_limit("regular file bytes", self.limits.max_file_bytes, bytes, path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        usage.regular_bytes = checked_add_limit(
            "total regular file bytes",
            usage.regular_bytes,
            bytes,
            self.limits.max_total_regular_bytes,
            path,
        )?;
        Ok(())
    }

    fn admit_symlink_target(&self, bytes: usize, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_usize_limit(
            "symlink target bytes",
            self.limits.max_symlink_target_bytes,
            bytes,
            path,
        )?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        usage.symlink_target_bytes = checked_add_limit(
            "total symlink target bytes",
            usage.symlink_target_bytes,
            bytes as u64,
            self.limits.max_total_symlink_target_bytes,
            path,
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct Deadline {
    started: Instant,
    limit: Duration,
}

impl Deadline {
    fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    fn check(&self, path: &Path) -> Result<(), Error> {
        if self.started.elapsed() >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(())
        }
    }
}

fn replace_accounted_regular(
    usage: &Mutex<CollectionUsage>,
    old: u64,
    new: u64,
    limits: CollectionLimits,
    path: &Path,
) -> Result<(), Error> {
    let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
    let without_old = usage.regular_bytes.checked_sub(old).ok_or(Error::ArithmeticOverflow {
        resource: "total regular file bytes",
        path: path.to_owned(),
    })?;
    let updated = without_old.checked_add(new).ok_or(Error::ArithmeticOverflow {
        resource: "total regular file bytes",
        path: path.to_owned(),
    })?;
    enforce_u64_limit(
        "total regular file bytes",
        limits.max_total_regular_bytes,
        updated,
        path,
    )?;
    usage.regular_bytes = updated;
    Ok(())
}

fn read_directory_names(directory: &DirectoryHandle, context: &CollectionContext) -> Result<Vec<OsString>, Error> {
    verify_directory_collection(directory)?;
    let cursor = open_entry(
        &directory.file,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        &directory.display_path,
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: descriptor is a fresh owned directory descriptor. fdopendir
    // consumes it on success; on failure it remains ours and is closed below.
    let stream = unsafe { libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe { libc::close(descriptor) };
        return Err(Error::Io {
            operation: "open package directory stream",
            path: directory.display_path.clone(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        context.check_time(&directory.display_path)?;
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::Io {
                operation: "enumerate package directory",
                path: directory.display_path.clone(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let relative_len = directory
            .relative
            .as_os_str()
            .as_bytes()
            .len()
            .checked_add(usize::from(!directory.relative.as_os_str().is_empty()))
            .and_then(|length| length.checked_add(bytes.len()))
            .ok_or(Error::ArithmeticOverflow {
                resource: "entry path bytes",
                path: directory.display_path.clone(),
            })?;
        let display_path = directory.display_path.join(OsStr::from_bytes(bytes));
        let depth = directory
            .relative
            .components()
            .count()
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow {
                resource: "path depth",
                path: display_path.clone(),
            })?;
        context.admit_entry_bytes(bytes.len(), relative_len, depth, &display_path)?;
        reserve(&mut names, 1, "directory entry names")?;
        names.push(copy_os_string(bytes, &display_path)?);
    }
    names.sort_unstable();
    context.check_time(&directory.display_path)?;
    verify_directory_collection(directory)?;
    Ok(names)
}

impl CollectionContext {
    fn admit_entry_bytes(
        &self,
        name_bytes: usize,
        path_bytes: usize,
        depth: usize,
        display_path: &Path,
    ) -> Result<(), Error> {
        self.check_time(display_path)?;
        self.check_depth(depth, display_path)?;
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, display_path)?;
        enforce_usize_limit("entry path bytes", self.limits.max_path_bytes, path_bytes, display_path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        update_entry_usage(&mut usage, self.limits, name_bytes, path_bytes, display_path)?;
        Ok(())
    }
}

fn update_entry_usage(
    usage: &mut CollectionUsage,
    limits: CollectionLimits,
    name_bytes: usize,
    path_bytes: usize,
    display_path: &Path,
) -> Result<(), Error> {
    let name_bytes = u64::try_from(name_bytes).map_err(|_| Error::ArithmeticOverflow {
        resource: "total entry name bytes",
        path: display_path.to_owned(),
    })?;
    let path_bytes = u64::try_from(path_bytes).map_err(|_| Error::ArithmeticOverflow {
        resource: "total entry path bytes",
        path: display_path.to_owned(),
    })?;
    let entries = checked_add_limit("total entries", usage.entries, 1, limits.max_entries, display_path)?;
    let names = checked_add_limit(
        "total entry name bytes",
        usage.name_bytes,
        name_bytes,
        limits.max_total_name_bytes,
        display_path,
    )?;
    let paths = checked_add_limit(
        "total entry path bytes",
        usage.path_bytes,
        path_bytes,
        limits.max_total_path_bytes,
        display_path,
    )?;
    usage.entries = entries;
    usage.name_bytes = names;
    usage.path_bytes = paths;
    Ok(())
}

struct DirectoryStream(NonNull<libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

fn verify_directory_collection(directory: &DirectoryHandle) -> Result<(), Error> {
    require_snapshot(
        &directory.display_path,
        directory.snapshot,
        &metadata(
            &directory.file,
            "verify package directory descriptor",
            &directory.display_path,
        )?,
    )?;
    let reopened = directory.anchor.open_directory(&directory.relative)?;
    require_snapshot(
        &directory.display_path,
        directory.snapshot,
        &metadata(&reopened, "verify package directory path", &directory.display_path)?,
    )
}

fn verify_entry_collection(
    parent: &DirectoryHandle,
    name: &OsStr,
    expected: FileSnapshot,
    path: &Path,
) -> Result<(), Error> {
    verify_directory_collection(parent)?;
    let reopened = open_entry_handle(&parent.file, name, path)?;
    require_snapshot(path, expected, &metadata(&reopened, "verify package entry path", path)?)
}

fn read_symlink_handle(handle: &File, path: &Path, context: &CollectionContext) -> Result<String, Error> {
    let capacity = context
        .limits
        .max_symlink_target_bytes
        .checked_add(1)
        .ok_or(Error::ArithmeticOverflow {
            resource: "symlink target bytes",
            path: path.to_owned(),
        })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|source| Error::Allocation {
        resource: "symlink target bytes",
        requested: capacity,
        detail: source.to_string(),
    })?;
    bytes.resize(capacity, 0);
    // Linux readlinkat with an empty path reads the symlink pinned by an
    // O_PATH|O_NOFOLLOW descriptor, rather than a replaceable pathname.
    // SAFETY: the descriptor is live and bytes is writable for capacity bytes.
    let read = unsafe { libc::readlinkat(handle.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if read == -1 {
        return Err(Error::Io {
            operation: "read package symlink target",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ArithmeticOverflow {
        resource: "symlink target bytes",
        path: path.to_owned(),
    })?;
    context.admit_symlink_target(read, path)?;
    bytes.truncate(read);
    String::from_utf8(bytes).map_err(|_| Error::NonUtf8SymlinkTarget { path: path.to_owned() })
}

fn open_entry_handle(parent: &File, name: &OsStr, path: &Path) -> Result<File, Error> {
    open_entry(
        parent,
        name,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        path,
    )
}

fn open_entry(parent: &File, name: &OsStr, flags: i32, path: &Path) -> Result<File, Error> {
    let name = c_name(name, path)?;
    // SAFETY: name is NUL-terminated, parent is live, and successful openat
    // returns a fresh descriptor owned below.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0) };
    if descriptor == -1 {
        return Err(Error::Io {
            operation: "open package tree entry",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, resolve: u64, display_path: &Path) -> Result<File, Error> {
    let path_c = CString::new(path).map_err(|_| Error::InvalidPath {
        path: display_path.to_owned(),
        detail: "path contains a NUL byte",
    })?;
    // SAFETY: all-zero open_how is valid before the public fields are set.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    // SAFETY: path_c and how remain live; successful openat2 returns a fresh
    // descriptor owned below.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path_c.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(Error::Io {
            operation: "open descriptor-anchored package path",
            path: display_path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let descriptor = RawFd::try_from(result).map_err(|_| Error::ArithmeticOverflow {
        resource: "file descriptor",
        path: display_path.to_owned(),
    })?;
    // SAFETY: successful openat2 returned a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

fn c_name(name: &OsStr, path: &Path) -> Result<CString, Error> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(Error::InvalidPath {
            path: path.to_owned(),
            detail: "entry name is not one normal path component",
        });
    }
    CString::new(name.as_bytes()).map_err(|_| Error::InvalidPath {
        path: path.to_owned(),
        detail: "entry name contains a NUL byte",
    })
}

fn relative_to_root(root: &Path, path: &Path) -> Result<PathBuf, Error> {
    let relative = path.strip_prefix(root).map_err(|_| Error::OutsideRoot {
        root: root.to_owned(),
        path: path.to_owned(),
    })?;
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(component) => {
                if component.to_str().is_none() {
                    return Err(Error::NonUtf8Path { path: path.to_owned() });
                }
                normalized.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::InvalidPath {
                    path: path.to_owned(),
                    detail: "path is not a normalized relative descendant",
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(Error::InvalidPath {
            path: path.to_owned(),
            detail: "collector root itself is not a package entry",
        });
    }
    Ok(normalized)
}

fn split_parent_name(relative: &Path, display_path: &Path) -> Result<(PathBuf, OsString), Error> {
    let name = relative.file_name().ok_or_else(|| Error::InvalidPath {
        path: display_path.to_owned(),
        detail: "package entry has no file name",
    })?;
    Ok((
        relative.parent().unwrap_or_else(|| Path::new("")).to_owned(),
        name.to_owned(),
    ))
}

fn join_relative(parent: &Path, name: &OsStr) -> PathBuf {
    let mut relative = parent.to_owned();
    relative.push(name);
    relative
}

fn copy_os_string(bytes: &[u8], path: &Path) -> Result<OsString, Error> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|source| Error::Allocation {
            resource: "directory entry name bytes",
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    owned.extend_from_slice(bytes);
    let _ = path;
    Ok(OsString::from_vec(owned))
}

fn copy_string(value: &str, resource: &'static str) -> Result<String, Error> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|source| Error::Allocation {
            resource,
            requested: value.len(),
            detail: source.to_string(),
        })?;
    owned.push_str(value);
    Ok(owned)
}

fn reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        detail: source.to_string(),
    })
}

fn metadata(file: &File, operation: &'static str, path: &Path) -> Result<Metadata, Error> {
    file.metadata().map_err(|source| Error::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn require_snapshot(path: &Path, expected: FileSnapshot, metadata: &Metadata) -> Result<(), Error> {
    let actual = FileSnapshot::from_metadata(metadata);
    if actual == expected {
        Ok(())
    } else {
        Err(changed(path, "entry identity or metadata changed"))
    }
}

fn changed(path: &Path, detail: &'static str) -> Error {
    Error::TreeChanged {
        path: path.to_owned(),
        detail,
    }
}

fn is_supported_special(file_type: &std::fs::FileType) -> bool {
    file_type.is_char_device() || file_type.is_block_device() || file_type.is_fifo() || file_type.is_socket()
}

fn enforce_usize_limit(resource: &'static str, limit: usize, actual: usize, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit: limit as u64,
            actual: actual as u64,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn enforce_u64_limit(resource: &'static str, limit: u64, actual: u64, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit,
            actual,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn checked_add_limit(
    resource: &'static str,
    current: u64,
    additional: u64,
    limit: u64,
    path: &Path,
) -> Result<u64, Error> {
    let actual = current.checked_add(additional).ok_or(Error::ArithmeticOverflow {
        resource,
        path: path.to_owned(),
    })?;
    enforce_u64_limit(resource, limit, actual, path)?;
    Ok(actual)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestPoint {
    AfterEntryHandle,
    AfterDirectoryOpen,
    AfterRegularOpen,
    AfterRegularHash,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum TestPoint {
    AfterEntryHandle,
    AfterDirectoryOpen,
    AfterRegularOpen,
    AfterRegularHash,
}

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("no matching path rule for {path}", path = path.display())]
    NoMatchingRule { path: PathBuf },
    #[error("package path {path} is not valid UTF-8", path = path.display())]
    NonUtf8Path { path: PathBuf },
    #[error("symlink target at {path} is not valid UTF-8", path = path.display())]
    NonUtf8SymlinkTarget { path: PathBuf },
    #[error("invalid collection rule pattern {pattern:?}: {detail}")]
    InvalidRulePattern { pattern: String, detail: String },
    #[error("package path {path} is outside collector root {root}", path = path.display(), root = root.display())]
    OutsideRoot { root: PathBuf, path: PathBuf },
    #[error("invalid package path {path}: {detail}", path = path.display())]
    InvalidPath { path: PathBuf, detail: &'static str },
    #[error("unsupported package entry {path}: {kind}", path = path.display())]
    UnsupportedFileType { path: PathBuf, kind: &'static str },
    #[error("{resource} {actual} exceeds limit {limit} at {path}", path = path.display())]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
        path: PathBuf,
    },
    #[error("collection exceeded {limit:?} while processing {path}", path = path.display())]
    DurationExceeded { path: PathBuf, limit: Duration },
    #[error("package tree changed at {path}: {detail}", path = path.display())]
    TreeChanged { path: PathBuf, detail: &'static str },
    #[error("package content at {path} lacks verified collection identity", path = path.display())]
    UnverifiedContent { path: PathBuf },
    #[error("package content length changed at {path}: expected {expected}, got {actual}", path = path.display())]
    ContentLengthChanged { path: PathBuf, expected: u64, actual: u64 },
    #[error("package content hash changed at {path}: expected {expected:032x}, got {actual:032x}", path = path.display())]
    ContentHashChanged {
        path: PathBuf,
        expected: u128,
        actual: u128,
    },
    #[error("arithmetic overflow for {resource} at {path}", path = path.display())]
    ArithmeticOverflow { resource: &'static str, path: PathBuf },
    #[error("failed to reserve {requested} units for {resource}: {detail}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[error("collection accounting lock was poisoned")]
    StatePoisoned,
    #[error("{operation} failed for {path}", path = path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        fs::Permissions,
        io::Read as _,
        os::unix::{
            fs::{PermissionsExt, symlink},
            net::UnixListener,
        },
        sync::atomic::{AtomicBool, Ordering},
    };

    use fs_err as fs;

    use super::*;

    fn add_rule(collector: &mut Collector, pattern: &str, package: &str, kind: PathRuleKind) {
        collector.add_rule(pattern, package, kind).unwrap();
    }

    fn all_collector(root: &Path) -> Collector {
        let mut collector = Collector::new(root);
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        collector
    }

    fn collector_with_limits(root: &Path, limits: CollectionLimits) -> Collector {
        let mut collector = Collector::new_with_limits(root, limits);
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        collector
    }

    fn write_file(root: &Path, relative: &str, mode: u32) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"payload").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn raw_glob_candidates_and_reverse_rule_precedence_select_the_output() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "usr/share/[literal]", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "fallback", PathRuleKind::Any);
        add_rule(
            &mut collector,
            &Pattern::escape("/usr/share/[literal]"),
            "lower-priority",
            PathRuleKind::Any,
        );
        add_rule(
            &mut collector,
            &Pattern::escape("/usr/share/[literal]"),
            "highest-priority",
            PathRuleKind::Any,
        );

        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        assert_eq!(info.target_path, Path::new("/usr/share/[literal]"));
        assert_eq!(info.package.as_ref(), "highest-priority");
    }

    #[test]
    fn rule_limits_accept_n_reject_n_plus_one_and_compile_once() {
        let root = tempfile::tempdir().unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_rules = 2;
        limits.max_rule_pattern_bytes = 4;
        limits.max_rule_package_bytes = 3;
        limits.max_total_rule_pattern_bytes = 8;
        limits.max_total_rule_package_bytes = 6;
        let mut collector = Collector::new_with_limits(root.path(), limits);
        collector.add_rule("/a", "out", PathRuleKind::Any).unwrap();
        collector.add_rule("/b", "out", PathRuleKind::Any).unwrap();
        assert!(Arc::ptr_eq(&collector.rules[0].package, &collector.rules[1].package));
        assert!(matches!(
            collector.add_rule("/c", "out", PathRuleKind::Any),
            Err(Error::LimitExceeded {
                resource: "collection rules",
                limit: 2,
                actual: 3,
                ..
            })
        ));

        let mut limits = CollectionLimits::default();
        limits.max_rule_pattern_bytes = 3;
        let mut collector = Collector::new_with_limits(root.path(), limits);
        collector.add_rule("abc", "p", PathRuleKind::Any).unwrap();
        assert!(matches!(
            collector.add_rule("abcd", "p", PathRuleKind::Any),
            Err(Error::LimitExceeded {
                resource: "rule pattern bytes",
                limit: 3,
                actual: 4,
                ..
            })
        ));

        let mut limits = CollectionLimits::default();
        limits.max_rule_package_bytes = 3;
        let mut collector = Collector::new_with_limits(root.path(), limits);
        collector.add_rule("*", "pkg", PathRuleKind::Any).unwrap();
        assert!(matches!(
            collector.add_rule("*", "pkgs", PathRuleKind::Any),
            Err(Error::LimitExceeded {
                resource: "rule package bytes",
                limit: 3,
                actual: 4,
                ..
            })
        ));

        let mut limits = CollectionLimits::default();
        limits.max_total_rule_pattern_bytes = 2;
        limits.max_total_rule_package_bytes = 2;
        let mut collector = Collector::new_with_limits(root.path(), limits);
        collector.add_rule("a", "p", PathRuleKind::Any).unwrap();
        collector.add_rule("b", "q", PathRuleKind::Any).unwrap();
        assert!(matches!(
            collector.add_rule("c", "r", PathRuleKind::Any),
            Err(Error::LimitExceeded {
                resource: "total rule pattern bytes",
                limit: 2,
                actual: 3,
                ..
            })
        ));

        let mut limits = CollectionLimits::default();
        limits.max_total_rule_package_bytes = 2;
        let mut collector = Collector::new_with_limits(root.path(), limits);
        collector.add_rule("a", "p", PathRuleKind::Any).unwrap();
        collector.add_rule("b", "q", PathRuleKind::Any).unwrap();
        assert!(matches!(
            collector.add_rule("c", "r", PathRuleKind::Any),
            Err(Error::LimitExceeded {
                resource: "total rule package bytes",
                limit: 2,
                actual: 3,
                ..
            })
        ));

        let mut collector = Collector::new(root.path());
        assert!(matches!(
            collector.add_rule("[", "out", PathRuleKind::Any),
            Err(Error::InvalidRulePattern { .. })
        ));
    }

    #[test]
    fn non_utf8_paths_and_symlink_targets_are_rejected_without_lossy_layouts() {
        let root = tempfile::tempdir().unwrap();
        let invalid_name = OsString::from_vec(vec![b'f', 0x80]);
        fs::write(root.path().join(&invalid_name), b"data").unwrap();
        assert!(matches!(
            all_collector(root.path()).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::NonUtf8Path { .. })
        ));

        let root = tempfile::tempdir().unwrap();
        let invalid_target = OsString::from_vec(vec![b't', 0x80]);
        symlink(&invalid_target, root.path().join("link")).unwrap();
        assert!(matches!(
            all_collector(root.path()).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::NonUtf8SymlinkTarget { .. })
        ));
    }

    #[test]
    fn every_path_reuses_the_single_interned_package_label() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..128 {
            fs::write(root.path().join(format!("file-{index:03}")), b"x").unwrap();
        }
        let paths = all_collector(root.path())
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let package = &paths.first().unwrap().package;

        assert_eq!(paths.len(), 128);
        assert!(paths.iter().all(|path| Arc::ptr_eq(package, &path.package)));
    }

    #[test]
    fn executable_rules_require_a_regular_file_with_an_execute_bit() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_file(root.path(), "usr/bin/tool", 0o751);
        let regular = write_file(root.path(), "usr/bin/data", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);
        let mut hasher = StoneDigestWriterHasher::new();

        assert_eq!(
            collector.path(&executable, &mut hasher).unwrap().package.as_ref(),
            "executables"
        );
        assert_eq!(collector.path(&regular, &mut hasher).unwrap().package.as_ref(), "out");
    }

    #[test]
    fn symlink_rules_use_lstat_and_enumeration_does_not_follow_linked_directories() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        write_file(external.path(), "nested/file", 0o644);
        let linked_dir = root.path().join("linked-dir");
        symlink(external.path().join("nested"), &linked_dir).unwrap();
        let broken = root.path().join("broken");
        symlink(root.path().join("missing"), &broken).unwrap();

        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/*", "links", PathRuleKind::Symlink);
        let paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();

        assert_eq!(
            paths.iter().map(|path| path.target_path.as_path()).collect::<Vec<_>>(),
            [Path::new("/broken"), Path::new("/linked-dir")]
        );
        assert!(paths.iter().all(|path| path.package.as_ref() == "links"));
        assert!(
            paths
                .iter()
                .all(|path| matches!(path.layout.file, StonePayloadLayoutFile::Symlink(..)))
        );
    }

    #[test]
    fn special_rules_match_unix_domain_sockets() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("run/service.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to create Unix socket fixture: {error}"),
        };
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "*", "out", PathRuleKind::Any);
        add_rule(&mut collector, "/run/*", "special", PathRuleKind::Special);

        let info = collector.path(&socket, &mut StoneDigestWriterHasher::new()).unwrap();
        assert_eq!(info.package.as_ref(), "special");
        assert!(matches!(info.layout.file, StonePayloadLayoutFile::Socket(..)));
    }

    #[test]
    fn collection_has_no_implicit_fallback_output() {
        let root = tempfile::tempdir().unwrap();
        let regular = write_file(root.path(), "usr/bin/data", 0o644);
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);

        assert!(matches!(
            collector.path(&regular, &mut StoneDigestWriterHasher::new()),
            Err(Error::NoMatchingRule { .. })
        ));
    }

    #[test]
    fn entry_depth_name_path_file_and_aggregate_limits_accept_n_and_reject_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("d")).unwrap();
        fs::write(root.path().join("d/a"), b"1234").unwrap();
        fs::write(root.path().join("b"), b"56").unwrap();

        let mut limits = CollectionLimits::default();
        limits.max_entries = 3;
        limits.max_depth = 2;
        limits.max_name_bytes = 1;
        limits.max_path_bytes = 3;
        limits.max_total_name_bytes = 3;
        limits.max_total_path_bytes = 5;
        limits.max_file_bytes = 4;
        limits.max_total_regular_bytes = 6;
        collector_with_limits(root.path(), limits)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();

        for (resource, mutate) in [
            ("total entries", 0usize),
            ("path depth", 1),
            ("entry name bytes", 2),
            ("entry path bytes", 3),
            ("regular file bytes", 4),
            ("total regular file bytes", 5),
            ("total entry name bytes", 6),
            ("total entry path bytes", 7),
        ] {
            let mut rejected = limits;
            match mutate {
                0 => rejected.max_entries -= 1,
                1 => rejected.max_depth -= 1,
                2 => rejected.max_name_bytes = 0,
                3 => rejected.max_path_bytes -= 1,
                4 => rejected.max_file_bytes -= 1,
                5 => rejected.max_total_regular_bytes -= 1,
                6 => rejected.max_total_name_bytes -= 1,
                7 => rejected.max_total_path_bytes -= 1,
                _ => unreachable!(),
            }
            let error = collector_with_limits(root.path(), rejected)
                .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
                .unwrap_err();
            assert!(
                matches!(error, Error::LimitExceeded { resource: actual, .. } if actual == resource),
                "expected {resource}, got {error:?}"
            );
        }
    }

    #[test]
    fn symlink_target_limit_accepts_n_and_rejects_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        symlink("12345678", root.path().join("link")).unwrap();
        symlink("1234", root.path().join("link2")).unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_symlink_target_bytes = 8;
        limits.max_total_symlink_target_bytes = 12;
        collector_with_limits(root.path(), limits)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        limits.max_symlink_target_bytes = 7;
        assert!(matches!(
            collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "symlink target bytes",
                limit: 7,
                actual: 8,
                ..
            })
        ));
        limits.max_symlink_target_bytes = 8;
        limits.max_total_symlink_target_bytes = 11;
        assert!(matches!(
            collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "total symlink target bytes",
                limit: 11,
                actual: 12,
                ..
            })
        ));
    }

    #[test]
    fn traversal_is_deterministic_iterative_and_preserves_empty_special_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("z"), b"z").unwrap();
        fs::write(root.path().join("a"), b"a").unwrap();
        let mut current = root.path().to_owned();
        for _ in 0..300 {
            current.push("d");
            fs::create_dir(&current).unwrap();
        }
        fs::write(current.join("leaf"), b"leaf").unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        fs::create_dir(root.path().join("special")).unwrap();
        fs::set_permissions(root.path().join("special"), Permissions::from_mode(0o700)).unwrap();

        let mut limits = CollectionLimits::default();
        limits.max_depth = 512;
        limits.max_path_bytes = 1024;
        let first = collector_with_limits(root.path(), limits)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let second = collector_with_limits(root.path(), limits)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let first_paths = first.iter().map(|path| path.target_path.clone()).collect::<Vec<_>>();
        let second_paths = second.iter().map(|path| path.target_path.clone()).collect::<Vec<_>>();
        assert_eq!(first_paths, second_paths);
        assert_eq!(first_paths.first().unwrap(), Path::new("/a"));
        assert!(first_paths.contains(&PathBuf::from("/empty")));
        assert!(first_paths.contains(&PathBuf::from("/special")));
        assert!(first_paths.iter().any(|path| path.ends_with("leaf")));
    }

    #[test]
    fn collection_deadline_is_finite() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"data").unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_duration = Duration::ZERO;
        assert!(matches!(
            collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::DurationExceeded { .. })
        ));
    }

    #[test]
    fn shared_deadline_survives_collection_and_expires_before_emission() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let mut limits = CollectionLimits::default();
        limits.max_duration = Duration::from_millis(100);
        let collector = collector_with_limits(root.path(), limits);
        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();

        std::thread::sleep(Duration::from_millis(150));

        assert!(matches!(info.open_verified(), Err(Error::DurationExceeded { .. })));
    }

    #[test]
    fn sparse_large_file_is_hashed_at_n_and_rejected_at_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large");
        let bytes = 2 * MIB;
        File::create(&path).unwrap().set_len(bytes).unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_file_bytes = bytes;
        limits.max_total_regular_bytes = bytes;
        collector_with_limits(root.path(), limits)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();

        limits.max_file_bytes = bytes - 1;
        assert!(matches!(
            collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "regular file bytes",
                limit,
                actual,
                ..
            }) if limit == bytes - 1 && actual == bytes
        ));
    }

    #[test]
    fn returned_paths_do_not_retain_one_descriptor_per_directory() {
        const CHILD_ENV: &str = "MASON_COLLECTOR_FD_REGRESSION_CHILD";
        const TEST_NAME: &str = "package::collect::tests::returned_paths_do_not_retain_one_descriptor_per_directory";
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([TEST_NAME, "--exact", "--test-threads=1"])
                .env(CHILD_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated descriptor regression failed: {status}");
            return;
        }

        let root = tempfile::tempdir().unwrap();
        for index in 0..256 {
            let directory = root.path().join(format!("d{index:03}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("file"), b"x").unwrap();
        }
        let descriptor_count = || fs::read_dir("/proc/self/fd").unwrap().count();
        let before = descriptor_count();
        let collector = all_collector(root.path());
        let paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let after = descriptor_count();

        assert_eq!(paths.iter().filter(|path| path.is_file()).count(), 256);
        assert!(
            after <= before + 8,
            "collected paths retained {} unexpected descriptors",
            after.saturating_sub(before)
        );
    }

    #[test]
    fn verified_reader_rejects_in_place_changes_during_content_emission() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, vec![b'a'; HASH_BUFFER_BYTES * 2]).unwrap();
        let collector = all_collector(root.path());
        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        let mut reader = info.open_verified().unwrap();
        let mut prefix = vec![0u8; HASH_BUFFER_BYTES];
        reader.read_exact(&mut prefix).unwrap();

        fs::write(&path, vec![b'b'; HASH_BUFFER_BYTES * 2]).unwrap();
        let mut suffix = Vec::new();
        reader.read_to_end(&mut suffix).unwrap();

        assert!(matches!(
            reader.finish(),
            Err(Error::ContentHashChanged { .. } | Error::TreeChanged { .. })
        ));
    }

    #[test]
    fn replacing_the_collector_root_invalidates_collected_paths() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        fs::create_dir(&root).unwrap();
        let path = write_file(&root, "file", 0o644);
        let collector = all_collector(&root);
        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        fs::rename(&root, base.path().join("old-root")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file"), b"payload").unwrap();

        assert!(matches!(info.verify_unchanged(), Err(Error::TreeChanged { .. })));
    }

    #[test]
    fn in_place_change_replacement_directory_swap_and_fifo_masquerade_are_rejected() {
        fn run_race(action: impl Fn(&Path) + Send + Sync + 'static, point: TestPoint) -> Error {
            let root = tempfile::tempdir().unwrap();
            fs::write(root.path().join("file"), b"original").unwrap();
            let fired = Arc::new(AtomicBool::new(false));
            let fired_hook = Arc::clone(&fired);
            let mut collector = all_collector(root.path());
            collector.hook = Some(Arc::new(move |actual, path| {
                if actual == point && !fired_hook.swap(true, Ordering::SeqCst) {
                    action(path);
                }
            }));
            let error = collector
                .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
                .unwrap_err();
            assert!(fired.load(Ordering::SeqCst));
            error
        }

        assert!(matches!(
            run_race(
                |path| fs::write(path, b"changed!").unwrap(),
                TestPoint::AfterRegularOpen
            ),
            Error::TreeChanged { .. }
        ));
        assert!(matches!(
            run_race(
                |path| {
                    let old = path.with_extension("old");
                    fs::rename(path, old).unwrap();
                    fs::write(path, b"replacement").unwrap();
                },
                TestPoint::AfterRegularHash
            ),
            Error::TreeChanged { .. }
        ));
        assert!(matches!(
            run_race(
                |path| {
                    fs::remove_file(path).unwrap();
                    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
                    // SAFETY: name is a live NUL-terminated pathname.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                },
                TestPoint::AfterEntryHandle
            ),
            Error::TreeChanged { .. }
        ));

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        fs::write(root.path().join("dir/file"), b"data").unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_hook = Arc::clone(&fired);
        let root_path = root.path().to_owned();
        let mut collector = all_collector(root.path());
        collector.hook = Some(Arc::new(move |point, path| {
            if point == TestPoint::AfterDirectoryOpen
                && path.ends_with("dir")
                && !fired_hook.swap(true, Ordering::SeqCst)
            {
                fs::rename(path, root_path.join("old-dir")).unwrap();
                fs::create_dir(path).unwrap();
            }
        }));
        assert!(matches!(
            collector.enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
            Err(Error::TreeChanged { .. })
        ));
    }

    #[test]
    fn collected_file_replacement_is_rejected_before_verified_emission() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let collector = all_collector(root.path());
        let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        fs::rename(&path, root.path().join("old")).unwrap();
        fs::write(&path, b"payload").unwrap();

        assert!(matches!(info.open_verified(), Err(Error::TreeChanged { .. })));
    }
}
