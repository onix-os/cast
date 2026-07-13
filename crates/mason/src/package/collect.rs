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
    witness: OnceLock<Arc<WitnessGraph>>,
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
            witness: OnceLock::new(),
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

    fn witness(&self) -> Result<Arc<WitnessGraph>, Error> {
        let anchor = self.anchor()?;
        let deadline = self.deadline();
        let candidate = Arc::new(WitnessGraph::new(
            anchor,
            self.limits,
            Arc::clone(&self.usage),
            deadline,
        ));
        let _ = self.witness.set(candidate);
        let witness = Arc::clone(self.witness.get().expect("witness graph was just initialized"));
        witness.ensure_initial_snapshot()?;
        Ok(witness)
    }

    pub(crate) fn check_deadline(&self, path: &Path) -> Result<(), Error> {
        self.deadline().check(path)
    }

    pub(crate) fn seal(&self) -> Result<SealedTree, Error> {
        let witness = self.witness()?;
        witness.seal()?;
        Ok(SealedTree { witness })
    }

    pub(crate) fn poison_inventory(&self) {
        if let Some(witness) = self.witness.get() {
            witness.poison();
        }
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
        let witness = self.witness()?;
        let relative = relative_to_root(&self.root, path)?;
        witness.require_path(&relative)?;
        self.witnessed_path_info(&witness, path, relative, hasher)
    }

    /// Authenticate a complete analyzer-generated batch before producing any
    /// path information. Missing ancestors are admitted only when they lead to
    /// one of the exact declared leaves; undeclared siblings fail the batch.
    pub fn paths(&self, paths: &[PathBuf], hasher: &mut StoneDigestWriterHasher) -> Result<Vec<PathInfo>, Error> {
        let witness = self.witness()?;
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        let mut normalized = Vec::new();
        reserve(&mut normalized, paths.len(), "generated package paths")?;
        enforce_u64_limit(
            "generated package declarations",
            self.limits.max_entries,
            paths.len() as u64,
            &self.root,
        )?;
        let declaration_usage = Arc::new(Mutex::new(CollectionUsage::default()));
        let declaration_context = CollectionContext::new(self.limits, declaration_usage, self.deadline());
        for path in paths {
            self.check_deadline(path)?;
            let relative = relative_to_root(&self.root, path)?;
            declaration_context.admit_entry(&relative, relative.components().count(), path)?;
            normalized.push(relative);
        }
        let mut order = Vec::new();
        reserve(&mut order, normalized.len(), "generated package path order")?;
        order.extend(0..normalized.len());
        order.sort_unstable_by(|left, right| normalized[*left].cmp(&normalized[*right]));
        self.check_deadline(&self.root)?;
        for pair in order.windows(2) {
            self.check_deadline(&self.root.join(&normalized[pair[0]]))?;
            if normalized[pair[0]] == normalized[pair[1]] {
                return Err(Error::DuplicateAdmission {
                    path: self.root.join(&normalized[pair[0]]),
                });
            }
        }

        let mut output = Vec::new();
        reserve(&mut output, normalized.len(), "generated path information")?;
        witness.admit_paths(&normalized)?;
        for (display_path, relative) in paths.iter().zip(normalized) {
            match self.witnessed_path_info(&witness, display_path, relative, hasher) {
                Ok(info) => output.push(info),
                Err(error) => {
                    witness.poison();
                    return Err(error);
                }
            }
        }
        Ok(output)
    }

    fn witnessed_path_info(
        &self,
        witness: &Arc<WitnessGraph>,
        display_path: &Path,
        relative: PathBuf,
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<PathInfo, Error> {
        let context = CollectionContext::detached(self.limits, self.deadline());
        let depth = relative.components().count();
        context.check_depth(depth, display_path)?;
        let (parent_relative, name) = split_parent_name(&relative, display_path)?;
        let parent_id = witness.directory_id(&parent_relative)?;
        let parent_file = witness.anchor.open_directory(&parent_relative)?;
        let parent_snapshot =
            FileSnapshot::from_metadata(&metadata(&parent_file, "stat witnessed package parent", display_path)?);
        witness.require_directory(parent_id, parent_snapshot, display_path)?;
        let parent = Arc::new(DirectoryHandle {
            file: parent_file,
            relative: parent_relative,
            display_path: display_path.parent().unwrap_or(&self.root).to_owned(),
            snapshot: parent_snapshot,
            anchor: Arc::clone(&witness.anchor),
            witness: Arc::clone(witness),
            witness_id: parent_id,
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
        let witness = self.witness()?;
        let deadline = self.deadline();
        deadline.check(&self.root)?;
        let anchor = Arc::clone(&witness.anchor);
        let traversal_usage = Arc::new(Mutex::new(CollectionUsage::default()));
        let context = CollectionContext::new(self.limits, traversal_usage, Arc::clone(&deadline));
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
        let witness_id = witness.directory_id(&relative)?;
        let directory = Arc::new(DirectoryHandle {
            file,
            relative,
            display_path,
            snapshot: FileSnapshot::from_metadata(&directory_metadata),
            anchor,
            witness: Arc::clone(&witness),
            witness_id,
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
                        let witness_id =
                            witness.child_directory_id(parent.witness_id, &name, entry_snapshot, &display_path)?;
                        reserve(&mut tasks, 1, "traversal tasks")?;
                        tasks.push(Task::Scan {
                            directory: Arc::new(DirectoryHandle {
                                file,
                                relative,
                                display_path,
                                snapshot: entry_snapshot,
                                anchor: Arc::clone(&parent.anchor),
                                witness: Arc::clone(&parent.witness),
                                witness_id,
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
        let parent_id = directory.witness.directory_id(&parent_relative)?;
        let parent = Arc::new(DirectoryHandle {
            file: parent_file,
            relative: parent_relative,
            display_path: directory.display_path.parent().unwrap_or(&self.root).to_owned(),
            snapshot: parent_snapshot,
            anchor: Arc::clone(&directory.anchor),
            witness: Arc::clone(&directory.witness),
            witness_id: parent_id,
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
enum InventoryTask {
    Scan {
        directory: Arc<DirectoryHandle>,
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
    },
}

impl InventoryTask {
    fn path(&self) -> &Path {
        match self {
            Self::Scan { directory, .. } | Self::Finalize { directory } => &directory.display_path,
            Self::Visit { parent, .. } => &parent.display_path,
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
    witness: Arc<WitnessGraph>,
    witness_id: DirectoryId,
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

type DirectoryId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WitnessPhase {
    Fresh,
    InitialSnapshot,
    AdmissionsOpen,
    Sealed,
    Poisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WitnessEntryKind {
    Regular { hash: u128 },
    Symlink { target: String },
    Special,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryWitness {
    snapshot: FileSnapshot,
    kind: WitnessEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WitnessChildKind {
    Directory(DirectoryId),
    Entry(EntryWitness),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WitnessChild {
    name: OsString,
    kind: WitnessChildKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryWitness {
    parent: Option<DirectoryId>,
    name: OsString,
    snapshot: FileSnapshot,
    children: Vec<WitnessChild>,
}

#[derive(Debug)]
struct WitnessState {
    phase: WitnessPhase,
    directories: Vec<DirectoryWitness>,
}

impl Default for WitnessState {
    fn default() -> Self {
        Self {
            phase: WitnessPhase::Fresh,
            directories: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct WitnessGraph {
    anchor: Arc<RootAnchor>,
    limits: CollectionLimits,
    usage: Arc<Mutex<CollectionUsage>>,
    deadline: Arc<Deadline>,
    state: Mutex<WitnessState>,
}

#[derive(Debug, Clone)]
pub(crate) struct SealedTree {
    witness: Arc<WitnessGraph>,
}

impl SealedTree {
    pub(crate) fn verify(&self) -> Result<(), Error> {
        self.witness.verify_sealed()
    }
}

#[derive(Debug, Default)]
struct AdmissionDelta {
    entries: u64,
    name_bytes: u64,
    path_bytes: u64,
    symlink_target_bytes: u64,
    regular_bytes: u64,
}

#[derive(Debug)]
struct DirectoryAdmission {
    id: DirectoryId,
    snapshot: FileSnapshot,
    additions: Vec<WitnessChild>,
}

#[derive(Debug)]
struct AdmissionDraft {
    existing: Vec<DirectoryAdmission>,
    new_directories: Vec<DirectoryWitness>,
    delta: AdmissionDelta,
    scan_usage: CollectionUsage,
}

#[derive(Debug)]
struct AdmissionTask {
    directory: Arc<DirectoryHandle>,
    declaration: usize,
    existing: Option<DirectoryId>,
}

#[derive(Debug)]
struct InventoryDraft {
    directories: Vec<DirectoryWitness>,
    usage: CollectionUsage,
}

#[derive(Debug)]
struct DeclaredNode {
    terminal: bool,
    children: Vec<(OsString, usize)>,
    inventory: DeclaredInventory,
}

#[derive(Debug, Clone, Copy)]
enum DeclaredInventory {
    ExistingDirectory(DirectoryId),
    ExistingEntry,
    Missing,
}

#[derive(Debug)]
struct DeclaredTrie {
    nodes: Vec<DeclaredNode>,
}

impl DeclaredTrie {
    fn new(
        paths: &[PathBuf],
        directories: &[DirectoryWitness],
        remaining_entries: u64,
        max_edges: u64,
        deadline: &Deadline,
        root: &Path,
    ) -> Result<Self, Error> {
        let mut nodes = Vec::new();
        reserve(&mut nodes, 1, "generated path declaration trie")?;
        nodes.push(DeclaredNode {
            terminal: false,
            children: Vec::new(),
            inventory: DeclaredInventory::ExistingDirectory(0),
        });
        let mut edges = 0u64;
        let mut projected_entries = max_edges
            .checked_sub(remaining_entries)
            .ok_or(Error::ArithmeticOverflow {
                resource: "remaining generated package entries",
                path: root.to_owned(),
            })?;
        for path in paths {
            deadline.check(&root.join(path))?;
            let mut node = 0;
            for component in path.components() {
                deadline.check(&root.join(path))?;
                let Component::Normal(name) = component else {
                    return Err(Error::InvalidPath {
                        path: path.clone(),
                        detail: "generated path declaration is not normalized",
                    });
                };
                let position = nodes[node]
                    .children
                    .binary_search_by(|(candidate, _)| candidate.as_os_str().cmp(name));
                node = match position {
                    Ok(position) => nodes[node].children[position].1,
                    Err(position) => {
                        let inventory = match nodes[node].inventory {
                            DeclaredInventory::ExistingDirectory(directory) => {
                                match find_child(directories, directory, name) {
                                    Some(WitnessChild {
                                        kind: WitnessChildKind::Directory(directory),
                                        ..
                                    }) => DeclaredInventory::ExistingDirectory(*directory),
                                    Some(WitnessChild {
                                        kind: WitnessChildKind::Entry(_),
                                        ..
                                    }) => DeclaredInventory::ExistingEntry,
                                    None => {
                                        projected_entries = checked_add_limit(
                                            "total entries",
                                            projected_entries,
                                            1,
                                            max_edges,
                                            &root.join(path),
                                        )?;
                                        DeclaredInventory::Missing
                                    }
                                }
                            }
                            DeclaredInventory::Missing => {
                                projected_entries = checked_add_limit(
                                    "total entries",
                                    projected_entries,
                                    1,
                                    max_edges,
                                    &root.join(path),
                                )?;
                                DeclaredInventory::Missing
                            }
                            DeclaredInventory::ExistingEntry => {
                                return Err(changed(
                                    &root.join(path),
                                    "generated path traverses a witnessed non-directory",
                                ));
                            }
                        };
                        edges = checked_add_limit(
                            "generated path declaration trie edges",
                            edges,
                            1,
                            max_edges,
                            &root.join(path),
                        )?;
                        let child = nodes.len();
                        reserve(&mut nodes, 1, "generated path declaration trie")?;
                        nodes.push(DeclaredNode {
                            terminal: false,
                            children: Vec::new(),
                            inventory,
                        });
                        nodes[node]
                            .children
                            .try_reserve(1)
                            .map_err(|source| Error::Allocation {
                                resource: "generated path declaration edges",
                                requested: 1,
                                detail: source.to_string(),
                            })?;
                        nodes[node].children.insert(position, (name.to_owned(), child));
                        child
                    }
                };
            }
            nodes[node].terminal = true;
        }
        deadline.check(root)?;
        Ok(Self { nodes })
    }

    fn child(&self, node: usize, name: &OsStr) -> Option<usize> {
        self.nodes[node]
            .children
            .binary_search_by(|(candidate, _)| candidate.as_os_str().cmp(name))
            .ok()
            .map(|position| self.nodes[node].children[position].1)
    }
}

impl WitnessGraph {
    fn new(
        anchor: Arc<RootAnchor>,
        limits: CollectionLimits,
        usage: Arc<Mutex<CollectionUsage>>,
        deadline: Arc<Deadline>,
    ) -> Self {
        Self {
            anchor,
            limits,
            usage,
            deadline,
            state: Mutex::new(WitnessState::default()),
        }
    }

    fn ensure_initial_snapshot(self: &Arc<Self>) -> Result<(), Error> {
        {
            let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
            match state.phase {
                WitnessPhase::AdmissionsOpen | WitnessPhase::Sealed => return Ok(()),
                WitnessPhase::Fresh => state.phase = WitnessPhase::InitialSnapshot,
                WitnessPhase::InitialSnapshot => {
                    return Err(Error::InvalidInventoryPhase {
                        operation: "start initial package inventory",
                        phase: "initial-snapshot",
                    });
                }
                WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            }
        }

        let snapshot = self.snapshot_inventory();
        let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match snapshot {
            Ok(draft) => {
                *self.usage.lock().map_err(|_| Error::StatePoisoned)? = draft.usage;
                state.directories = draft.directories;
                state.phase = WitnessPhase::AdmissionsOpen;
                Ok(())
            }
            Err(error) => {
                state.phase = WitnessPhase::Poisoned;
                Err(error)
            }
        }
    }

    fn seal(self: &Arc<Self>) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::AdmissionsOpen => {}
            WitnessPhase::Sealed => {
                drop(state);
                return self.verify_sealed();
            }
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "seal package inventory",
                    phase: phase.name(),
                });
            }
        }
        let result = self
            .snapshot_inventory()
            .and_then(|actual| self.compare_complete_draft(&state, &actual));
        match result {
            Ok(()) => {
                state.phase = WitnessPhase::Sealed;
                Ok(())
            }
            Err(error) => {
                state.phase = WitnessPhase::Poisoned;
                Err(error)
            }
        }
    }

    fn verify_sealed(self: &Arc<Self>) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::Sealed => {}
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "verify sealed package inventory",
                    phase: phase.name(),
                });
            }
        }
        let result = self
            .snapshot_inventory()
            .and_then(|actual| self.compare_complete_draft(&state, &actual));
        if result.is_err() {
            state.phase = WitnessPhase::Poisoned;
        }
        result
    }

    fn poison(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = WitnessPhase::Poisoned;
        }
    }

    fn compare_complete_draft(&self, state: &WitnessState, actual: &InventoryDraft) -> Result<(), Error> {
        compare_exact_inventory(
            &state.directories,
            &actual.directories,
            &self.anchor.path,
            &self.deadline,
        )?;
        let expected_usage = self.usage.lock().map_err(|_| Error::StatePoisoned)?;
        if *expected_usage != actual.usage {
            return Err(changed(
                &self.anchor.path,
                "complete witnessed package resource usage changed",
            ));
        }
        Ok(())
    }

    fn directory_id(&self, relative: &Path) -> Result<DirectoryId, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed directory")?;
        lookup_directory(&state.directories, relative).ok_or_else(|| Error::UnwitnessedPath {
            path: self.anchor.path.join(relative),
        })
    }

    fn require_path(&self, relative: &Path) -> Result<(), Error> {
        let (parent, name) = split_parent_name(relative, &self.anchor.path.join(relative))?;
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed package path")?;
        let parent = lookup_directory(&state.directories, &parent).ok_or_else(|| Error::UnwitnessedPath {
            path: self.anchor.path.join(relative),
        })?;
        find_child(&state.directories, parent, &name)
            .map(|_| ())
            .ok_or_else(|| Error::UnwitnessedPath {
                path: self.anchor.path.join(relative),
            })
    }

    fn contains_regular(&self, relative: &Path) -> Result<bool, Error> {
        let (parent, name) = split_parent_name(relative, &self.anchor.path.join(relative))?;
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::AdmissionsOpen => {}
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "query package inventory",
                    phase: phase.name(),
                });
            }
        }
        let Some(parent) = lookup_directory(&state.directories, &parent) else {
            return Ok(false);
        };
        Ok(find_child(&state.directories, parent, &name).is_some_and(|child| {
            matches!(
                &child.kind,
                WitnessChildKind::Entry(EntryWitness {
                    kind: WitnessEntryKind::Regular { .. },
                    ..
                })
            )
        }))
    }

    fn child_directory_id(
        &self,
        parent: DirectoryId,
        name: &OsStr,
        snapshot: FileSnapshot,
        path: &Path,
    ) -> Result<DirectoryId, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed child directory")?;
        let child = find_child(&state.directories, parent, name)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        let id = match child.kind.clone() {
            WitnessChildKind::Directory(id) => id,
            WitnessChildKind::Entry(_) => {
                return Err(changed(path, "witnessed entry changed type to a directory"));
            }
        };
        if state.directories[id].snapshot != snapshot {
            return Err(changed(path, "witnessed directory identity or metadata changed"));
        }
        Ok(id)
    }

    fn require_directory(&self, id: DirectoryId, snapshot: FileSnapshot, path: &Path) -> Result<(), Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "verify witnessed directory")?;
        let expected = state
            .directories
            .get(id)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        if expected.snapshot == snapshot {
            Ok(())
        } else {
            Err(changed(path, "witnessed directory identity or metadata changed"))
        }
    }

    fn directory_identity(&self, id: DirectoryId) -> Result<NodeIdentity, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "read witnessed directory identity")?;
        state
            .directories
            .get(id)
            .map(|directory| directory.snapshot.node)
            .ok_or_else(|| Error::UnwitnessedPath {
                path: self.anchor.path.clone(),
            })
    }

    fn entry_path(&self, parent: DirectoryId, name: &OsStr) -> Result<PathBuf, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed entry path")?;
        let mut relative = directory_relative(&state.directories, parent, &self.anchor.path)?;
        relative.push(name);
        Ok(self.anchor.path.join(relative))
    }

    fn open_directory(&self, id: DirectoryId, display_path: &Path) -> Result<File, Error> {
        let (relative, identity) = {
            let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
            require_usable_phase(&state, "open witnessed directory")?;
            let directory = state.directories.get(id).ok_or_else(|| Error::UnwitnessedPath {
                path: display_path.to_owned(),
            })?;
            (
                directory_relative(&state.directories, id, &self.anchor.path)?,
                directory.snapshot.node,
            )
        };
        let file = self.anchor.open_directory(&relative)?;
        let actual = NodeIdentity::from_metadata(&metadata(&file, "stat witnessed directory", display_path)?);
        if actual != identity {
            return Err(changed(display_path, "witnessed directory was replaced"));
        }
        Ok(file)
    }

    #[allow(clippy::too_many_arguments)]
    fn restate_regular(
        self: &Arc<Self>,
        parent: DirectoryId,
        name: &OsStr,
        old_snapshot: FileSnapshot,
        new_snapshot: FileSnapshot,
        hash: u128,
        path: &Path,
    ) -> Result<(), Error> {
        self.deadline.check(path)?;
        let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::AdmissionsOpen => {}
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "restate analyzed regular file",
                    phase: phase.name(),
                });
            }
        }
        let expected = find_child(&state.directories, parent, name)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        let WitnessChildKind::Entry(expected) = &expected.kind else {
            return Err(changed(path, "analyzed path was witnessed as a directory"));
        };
        if expected.snapshot != old_snapshot || !matches!(expected.kind, WitnessEntryKind::Regular { .. }) {
            return Err(changed(path, "analyzed path has a stale regular-file witness"));
        }
        if old_snapshot.node != new_snapshot.node || old_snapshot.links != 1 || new_snapshot.links != 1 {
            return Err(changed(
                path,
                "analyzed regular file transition was not an in-place single-link mutation",
            ));
        }

        let parent_relative = directory_relative(&state.directories, parent, &self.anchor.path)?;
        let parent_snapshot = state.directories[parent].snapshot;
        let parent_file = self.anchor.open_directory(&parent_relative)?;
        require_snapshot(
            path,
            parent_snapshot,
            &metadata(&parent_file, "stat restated package parent", path)?,
        )?;
        let parent_handle = DirectoryHandle {
            file: parent_file,
            relative: parent_relative,
            display_path: path.parent().unwrap_or(&self.anchor.path).to_owned(),
            snapshot: parent_snapshot,
            anchor: Arc::clone(&self.anchor),
            witness: Arc::clone(self),
            witness_id: parent,
        };
        let rescan_usage = Arc::new(Mutex::new(CollectionUsage::default()));
        let rescan = CollectionContext::new(self.limits, rescan_usage, Arc::clone(&self.deadline));
        let names = read_directory_names(&parent_handle, &rescan)?;
        let expected_names = &state.directories[parent].children;
        if names.len() != expected_names.len()
            || names
                .iter()
                .zip(expected_names)
                .any(|(actual, expected)| actual != &expected.name)
        {
            return Err(changed(path, "analyzed file parent membership changed"));
        }

        let mut usage = self.usage.lock().map_err(|_| Error::StatePoisoned)?;
        let mut updated_usage = usage.clone();
        updated_usage.regular_bytes = updated_usage
            .regular_bytes
            .checked_sub(old_snapshot.size)
            .and_then(|bytes| bytes.checked_add(new_snapshot.size))
            .ok_or(Error::ArithmeticOverflow {
                resource: "total regular file bytes",
                path: path.to_owned(),
            })?;
        enforce_u64_limit(
            "total regular file bytes",
            self.limits.max_total_regular_bytes,
            updated_usage.regular_bytes,
            path,
        )?;
        let position = state.directories[parent]
            .children
            .binary_search_by(|child| child.name.as_os_str().cmp(name))
            .map_err(|_| Error::UnwitnessedPath { path: path.to_owned() })?;
        state.directories[parent].children[position].kind = WitnessChildKind::Entry(EntryWitness {
            snapshot: new_snapshot,
            kind: WitnessEntryKind::Regular { hash },
        });
        *usage = updated_usage;
        Ok(())
    }
}

impl WitnessPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::InitialSnapshot => "initial-snapshot",
            Self::AdmissionsOpen => "admissions-open",
            Self::Sealed => "sealed",
            Self::Poisoned => "poisoned",
        }
    }
}

impl WitnessGraph {
    fn snapshot_inventory(self: &Arc<Self>) -> Result<InventoryDraft, Error> {
        self.deadline.check(&self.anchor.path)?;
        self.anchor.verify_path_node()?;
        let draft_usage = Arc::new(Mutex::new(CollectionUsage::default()));
        let context = CollectionContext::new(self.limits, Arc::clone(&draft_usage), Arc::clone(&self.deadline));
        let root_file = self.anchor.open_directory(Path::new(""))?;
        let root_metadata = metadata(&root_file, "stat package inventory root", &self.anchor.path)?;
        let root_snapshot = FileSnapshot::from_metadata(&root_metadata);
        let mut directories = Vec::new();
        reserve(&mut directories, 1, "package inventory directories")?;
        directories.push(DirectoryWitness {
            parent: None,
            name: OsString::new(),
            snapshot: root_snapshot,
            children: Vec::new(),
        });
        let root = Arc::new(DirectoryHandle {
            file: root_file,
            relative: PathBuf::new(),
            display_path: self.anchor.path.clone(),
            snapshot: root_snapshot,
            anchor: Arc::clone(&self.anchor),
            witness: Arc::clone(self),
            witness_id: 0,
        });
        let mut tasks = Vec::new();
        reserve(&mut tasks, 1, "package inventory traversal tasks")?;
        tasks.push(InventoryTask::Scan {
            directory: root,
            depth: 0,
        });
        let mut hasher = StoneDigestWriterHasher::new();

        while let Some(task) = tasks.pop() {
            context.check_time(task.path())?;
            match task {
                InventoryTask::Scan { directory, depth } => {
                    let names = read_directory_names(&directory, &context)?;
                    let child_depth = depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                        resource: "path depth",
                        path: directory.display_path.clone(),
                    })?;
                    let task_count = names.len().checked_add(1).ok_or(Error::ArithmeticOverflow {
                        resource: "package inventory traversal tasks",
                        path: directory.display_path.clone(),
                    })?;
                    reserve(&mut tasks, task_count, "package inventory traversal tasks")?;
                    tasks.push(InventoryTask::Finalize {
                        directory: Arc::clone(&directory),
                    });
                    for name in names.into_iter().rev() {
                        let relative = join_relative(&directory.relative, &name);
                        tasks.push(InventoryTask::Visit {
                            parent: Arc::clone(&directory),
                            name,
                            relative,
                            depth: child_depth,
                        });
                    }
                }
                InventoryTask::Visit {
                    parent,
                    name,
                    relative,
                    depth,
                } => {
                    let display_path = self.anchor.path.join(&relative);
                    if name.to_str().is_none() {
                        return Err(Error::NonUtf8Path { path: display_path });
                    }
                    context.check_depth(depth, &display_path)?;
                    let handle = open_entry_handle(&parent.file, &name, &display_path)?;
                    let entry_metadata = metadata(&handle, "stat package inventory entry", &display_path)?;
                    let entry_snapshot = FileSnapshot::from_metadata(&entry_metadata);
                    if entry_metadata.file_type().is_dir() {
                        let file = open_entry(
                            &parent.file,
                            &name,
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                            &display_path,
                        )?;
                        require_snapshot(
                            &display_path,
                            entry_snapshot,
                            &metadata(&file, "stat opened inventory directory", &display_path)?,
                        )?;
                        verify_directory_collection(&parent)?;
                        let id = directories.len();
                        reserve(&mut directories, 1, "package inventory directories")?;
                        directories.push(DirectoryWitness {
                            parent: Some(parent.witness_id),
                            name: name.clone(),
                            snapshot: entry_snapshot,
                            children: Vec::new(),
                        });
                        let children = &mut directories[parent.witness_id].children;
                        reserve(children, 1, "package inventory children")?;
                        children.push(WitnessChild {
                            name: name.clone(),
                            kind: WitnessChildKind::Directory(id),
                        });
                        reserve(&mut tasks, 1, "package inventory traversal tasks")?;
                        tasks.push(InventoryTask::Scan {
                            directory: Arc::new(DirectoryHandle {
                                file,
                                relative,
                                display_path,
                                snapshot: entry_snapshot,
                                anchor: Arc::clone(&self.anchor),
                                witness: Arc::clone(self),
                                witness_id: id,
                            }),
                            depth,
                        });
                    } else {
                        let file_type = entry_metadata.file_type();
                        let kind = if file_type.is_symlink() {
                            let target = read_symlink_handle(&handle, &display_path, &context)?;
                            verify_entry_collection(&parent, &name, entry_snapshot, &display_path)?;
                            WitnessEntryKind::Symlink { target }
                        } else if file_type.is_file() {
                            context.admit_regular(entry_snapshot.size, &display_path)?;
                            let hash = hash_inventory_regular(
                                &context,
                                &parent,
                                &name,
                                entry_snapshot,
                                &display_path,
                                &mut hasher,
                            )?;
                            WitnessEntryKind::Regular { hash }
                        } else if is_supported_special(&file_type) {
                            verify_entry_collection(&parent, &name, entry_snapshot, &display_path)?;
                            WitnessEntryKind::Special
                        } else {
                            return Err(Error::UnsupportedFileType {
                                path: display_path,
                                kind: "unknown special inode",
                            });
                        };
                        let children = &mut directories[parent.witness_id].children;
                        reserve(children, 1, "package inventory children")?;
                        children.push(WitnessChild {
                            name,
                            kind: WitnessChildKind::Entry(EntryWitness {
                                snapshot: entry_snapshot,
                                kind,
                            }),
                        });
                    }
                }
                InventoryTask::Finalize { directory } => {
                    verify_directory_collection(&directory)?;
                    let rescan_usage = Arc::new(Mutex::new(CollectionUsage::default()));
                    let rescan = CollectionContext::new(self.limits, rescan_usage, Arc::clone(&self.deadline));
                    let actual_names = read_directory_names(&directory, &rescan)?;
                    let children = &directories[directory.witness_id].children;
                    if actual_names.len() != children.len()
                        || actual_names
                            .iter()
                            .zip(children)
                            .any(|(actual, expected)| actual != &expected.name)
                    {
                        return Err(changed(
                            &directory.display_path,
                            "directory membership changed while inventory was captured",
                        ));
                    }
                }
            }
        }
        self.anchor.verify_path_node()?;
        let usage = draft_usage.lock().map_err(|_| Error::StatePoisoned)?.clone();
        Ok(InventoryDraft { directories, usage })
    }

    fn admit_paths(self: &Arc<Self>, paths: &[PathBuf]) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::AdmissionsOpen => {}
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "admit generated package paths",
                    phase: phase.name(),
                });
            }
        }
        let usage_before = match self.usage.lock() {
            Ok(usage) => usage.clone(),
            Err(_) => {
                state.phase = WitnessPhase::Poisoned;
                return Err(Error::StatePoisoned);
            }
        };
        let remaining_entries = match self.limits.max_entries.checked_sub(usage_before.entries) {
            Some(remaining) => remaining,
            None => {
                state.phase = WitnessPhase::Poisoned;
                return Err(Error::ArithmeticOverflow {
                    resource: "remaining generated package entries",
                    path: self.anchor.path.clone(),
                });
            }
        };
        let trie = match DeclaredTrie::new(
            paths,
            &state.directories,
            remaining_entries,
            self.limits.max_entries,
            &self.deadline,
            &self.anchor.path,
        ) {
            Ok(trie) => trie,
            Err(error) => {
                state.phase = WitnessPhase::Poisoned;
                return Err(error);
            }
        };
        let draft = match self.build_admission(&state, &trie) {
            Ok(draft) => draft,
            Err(error) => {
                state.phase = WitnessPhase::Poisoned;
                return Err(error);
            }
        };
        let mut usage = self.usage.lock().map_err(|_| Error::StatePoisoned)?;
        if *usage != usage_before {
            state.phase = WitnessPhase::Poisoned;
            return Err(changed(
                &self.anchor.path,
                "package collection usage changed during generated admission",
            ));
        }
        if let Err(error) = validate_usage(&draft.scan_usage, self.limits, &self.anchor.path) {
            state.phase = WitnessPhase::Poisoned;
            return Err(error);
        }
        let updated_usage = match usage_after_admission(&usage, &draft.delta, self.limits, &self.anchor.path) {
            Ok(usage) => usage,
            Err(error) => {
                state.phase = WitnessPhase::Poisoned;
                return Err(error);
            }
        };
        if let Err(error) = reserve_admission_commit(&mut state.directories, &draft) {
            state.phase = WitnessPhase::Poisoned;
            return Err(error);
        }
        commit_admission(&mut state.directories, draft);
        *usage = updated_usage;
        Ok(())
    }

    fn build_admission(self: &Arc<Self>, state: &WitnessState, trie: &DeclaredTrie) -> Result<AdmissionDraft, Error> {
        self.deadline.check(&self.anchor.path)?;
        let scan_usage = Arc::new(Mutex::new(CollectionUsage::default()));
        let context = CollectionContext::new(self.limits, Arc::clone(&scan_usage), Arc::clone(&self.deadline));
        let root_file = self.anchor.open_directory(Path::new(""))?;
        let root_metadata = metadata(&root_file, "stat package admission root", &self.anchor.path)?;
        let root_snapshot = FileSnapshot::from_metadata(&root_metadata);
        if !stable_directory_snapshot(state.directories[0].snapshot, root_snapshot) {
            return Err(changed(&self.anchor.path, "package root changed before admission"));
        }
        let root = Arc::new(DirectoryHandle {
            file: root_file,
            relative: PathBuf::new(),
            display_path: self.anchor.path.clone(),
            snapshot: root_snapshot,
            anchor: Arc::clone(&self.anchor),
            witness: Arc::clone(self),
            witness_id: 0,
        });
        let mut draft = AdmissionDraft {
            existing: Vec::new(),
            new_directories: Vec::new(),
            delta: AdmissionDelta::default(),
            scan_usage: CollectionUsage::default(),
        };
        let mut tasks = Vec::new();
        reserve(&mut tasks, 1, "package admission traversal tasks")?;
        tasks.push(AdmissionTask {
            directory: root,
            declaration: 0,
            existing: Some(0),
        });
        let mut hasher = StoneDigestWriterHasher::new();
        let base_directory_count = state.directories.len();

        while let Some(task) = tasks.pop() {
            context.check_time(&task.directory.display_path)?;
            let expected_children = task
                .existing
                .map(|id| state.directories[id].children.as_slice())
                .unwrap_or_default();
            let names = read_directory_names(&task.directory, &context)?;
            let update_index = if let Some(id) = task.existing {
                reserve(&mut draft.existing, 1, "package admission directory updates")?;
                draft.existing.push(DirectoryAdmission {
                    id,
                    snapshot: task.directory.snapshot,
                    additions: Vec::new(),
                });
                Some(draft.existing.len() - 1)
            } else {
                None
            };
            let mut expected_index = 0;

            for name in &names {
                context.check_time(&task.directory.display_path)?;
                if let Some(expected) = expected_children.get(expected_index)
                    && expected.name < *name
                {
                    return Err(changed(
                        &task.directory.display_path.join(&expected.name),
                        "witnessed package entry disappeared during admission",
                    ));
                }
                let expected = expected_children
                    .get(expected_index)
                    .filter(|expected| expected.name == *name);
                if expected.is_some() {
                    expected_index += 1;
                }
                let declaration = trie.child(task.declaration, name);
                if let Some(declaration) = declaration
                    && trie.nodes[declaration].terminal
                    && expected.is_some()
                {
                    return Err(Error::ExistingAdmission {
                        path: task.directory.display_path.join(name),
                    });
                }
                let relative = join_relative(&task.directory.relative, name);
                let display_path = self.anchor.path.join(&relative);
                let handle = open_entry_handle(&task.directory.file, name, &display_path)?;
                let entry_metadata = metadata(&handle, "stat package admission entry", &display_path)?;
                let entry_snapshot = FileSnapshot::from_metadata(&entry_metadata);

                match expected {
                    Some(expected) => match &expected.kind {
                        WitnessChildKind::Directory(expected_id) => {
                            if !entry_metadata.file_type().is_dir() {
                                return Err(changed(
                                    &display_path,
                                    "witnessed directory changed type during admission",
                                ));
                            }
                            let declared_descendants = declaration
                                .map(|id| !trie.nodes[id].children.is_empty())
                                .unwrap_or(false);
                            if declared_descendants {
                                if !stable_directory_snapshot(state.directories[*expected_id].snapshot, entry_snapshot)
                                {
                                    return Err(changed(
                                        &display_path,
                                        "witnessed directory identity or permissions changed during admission",
                                    ));
                                }
                                let file = open_entry(
                                    &task.directory.file,
                                    name,
                                    libc::O_RDONLY
                                        | libc::O_DIRECTORY
                                        | libc::O_CLOEXEC
                                        | libc::O_NOFOLLOW
                                        | libc::O_NONBLOCK,
                                    &display_path,
                                )?;
                                require_snapshot(
                                    &display_path,
                                    entry_snapshot,
                                    &metadata(&file, "stat admitted child directory", &display_path)?,
                                )?;
                                reserve(&mut tasks, 1, "package admission traversal tasks")?;
                                tasks.push(AdmissionTask {
                                    directory: Arc::new(DirectoryHandle {
                                        file,
                                        relative,
                                        display_path,
                                        snapshot: entry_snapshot,
                                        anchor: Arc::clone(&self.anchor),
                                        witness: Arc::clone(self),
                                        witness_id: *expected_id,
                                    }),
                                    declaration: declaration.expect("declared descendants have a trie node"),
                                    existing: Some(*expected_id),
                                });
                            } else if state.directories[*expected_id].snapshot != entry_snapshot {
                                return Err(changed(
                                    &display_path,
                                    "unrelated witnessed directory changed during admission",
                                ));
                            }
                        }
                        WitnessChildKind::Entry(expected_entry) => {
                            if declaration.is_some_and(|id| !trie.nodes[id].children.is_empty()) {
                                return Err(changed(
                                    &display_path,
                                    "generated path traverses a witnessed non-directory",
                                ));
                            }
                            let actual = capture_entry_witness(
                                &context,
                                &task.directory,
                                name,
                                &handle,
                                &entry_metadata,
                                entry_snapshot,
                                &display_path,
                                &mut hasher,
                            )?;
                            if &actual != expected_entry {
                                return Err(changed(&display_path, "witnessed sibling changed during admission"));
                            }
                        }
                    },
                    None => {
                        let declaration = declaration.ok_or_else(|| {
                            changed(
                                &display_path,
                                "an undeclared sibling appeared during generated admission",
                            )
                        })?;
                        let child = if entry_metadata.file_type().is_dir() {
                            let id = base_directory_count.checked_add(draft.new_directories.len()).ok_or(
                                Error::ArithmeticOverflow {
                                    resource: "package inventory directories",
                                    path: display_path.clone(),
                                },
                            )?;
                            let file = open_entry(
                                &task.directory.file,
                                name,
                                libc::O_RDONLY
                                    | libc::O_DIRECTORY
                                    | libc::O_CLOEXEC
                                    | libc::O_NOFOLLOW
                                    | libc::O_NONBLOCK,
                                &display_path,
                            )?;
                            require_snapshot(
                                &display_path,
                                entry_snapshot,
                                &metadata(&file, "stat generated package directory", &display_path)?,
                            )?;
                            reserve(&mut draft.new_directories, 1, "generated package directories")?;
                            draft.new_directories.push(DirectoryWitness {
                                parent: Some(task.directory.witness_id),
                                name: name.clone(),
                                snapshot: entry_snapshot,
                                children: Vec::new(),
                            });
                            reserve(&mut tasks, 1, "package admission traversal tasks")?;
                            tasks.push(AdmissionTask {
                                directory: Arc::new(DirectoryHandle {
                                    file,
                                    relative: relative.clone(),
                                    display_path: display_path.clone(),
                                    snapshot: entry_snapshot,
                                    anchor: Arc::clone(&self.anchor),
                                    witness: Arc::clone(self),
                                    witness_id: id,
                                }),
                                declaration,
                                existing: None,
                            });
                            WitnessChild {
                                name: name.clone(),
                                kind: WitnessChildKind::Directory(id),
                            }
                        } else {
                            if !trie.nodes[declaration].terminal || !trie.nodes[declaration].children.is_empty() {
                                return Err(changed(
                                    &display_path,
                                    "generated non-directory does not match an exact declared leaf",
                                ));
                            }
                            WitnessChild {
                                name: name.clone(),
                                kind: WitnessChildKind::Entry(capture_entry_witness(
                                    &context,
                                    &task.directory,
                                    name,
                                    &handle,
                                    &entry_metadata,
                                    entry_snapshot,
                                    &display_path,
                                    &mut hasher,
                                )?),
                            }
                        };
                        add_admission_delta_for_relative(&relative, &child, &display_path, &mut draft.delta)?;
                        if let Some(update_index) = update_index {
                            reserve(
                                &mut draft.existing[update_index].additions,
                                1,
                                "generated package child edges",
                            )?;
                            draft.existing[update_index].additions.push(child);
                        } else {
                            let local_id = task
                                .directory
                                .witness_id
                                .checked_sub(base_directory_count)
                                .ok_or_else(|| changed(&display_path, "invalid generated directory identity"))?;
                            let children = &mut draft.new_directories[local_id].children;
                            reserve(children, 1, "generated package child edges")?;
                            children.push(child);
                        }
                    }
                }
            }
            if let Some(missing) = expected_children.get(expected_index) {
                return Err(changed(
                    &task.directory.display_path.join(&missing.name),
                    "witnessed package entry disappeared during admission",
                ));
            }
            for (declared, _) in &trie.nodes[task.declaration].children {
                if names.binary_search(declared).is_err() {
                    return Err(changed(
                        &task.directory.display_path.join(declared),
                        "declared generated package path was not created",
                    ));
                }
            }

            let rescan_usage = Arc::new(Mutex::new(CollectionUsage::default()));
            let rescan = CollectionContext::new(self.limits, rescan_usage, Arc::clone(&self.deadline));
            let rescanned = read_directory_names(&task.directory, &rescan)?;
            if rescanned != names {
                return Err(changed(
                    &task.directory.display_path,
                    "directory membership changed during generated admission",
                ));
            }
            if let Some(update_index) = update_index {
                let update = &mut draft.existing[update_index];
                let expected_snapshot = state.directories[update.id].snapshot;
                if update.additions.is_empty() {
                    if task.directory.snapshot != expected_snapshot {
                        return Err(changed(
                            &task.directory.display_path,
                            "unmodified parent metadata changed during generated admission",
                        ));
                    }
                    update.snapshot = expected_snapshot;
                } else if !stable_directory_snapshot(expected_snapshot, task.directory.snapshot) {
                    return Err(changed(
                        &task.directory.display_path,
                        "generated parent identity or permissions changed",
                    ));
                }
            }
        }
        draft.scan_usage = scan_usage.lock().map_err(|_| Error::StatePoisoned)?.clone();
        Ok(draft)
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
    witness: Arc<WitnessGraph>,
    parent_id: DirectoryId,
    name: OsString,
    snapshot: FileSnapshot,
    kind: VerifiedKind,
    limits: CollectionLimits,
    deadline: Arc<Deadline>,
}

impl VerifiedPath {
    fn new(
        parent: &DirectoryHandle,
        name: OsString,
        snapshot: FileSnapshot,
        kind: VerifiedKind,
        limits: CollectionLimits,
        deadline: Arc<Deadline>,
    ) -> Self {
        Self {
            anchor: Arc::clone(&parent.anchor),
            witness: Arc::clone(&parent.witness),
            parent_id: parent.witness_id,
            name,
            snapshot,
            kind,
            limits,
            deadline,
        }
    }

    fn display_path(&self) -> Result<PathBuf, Error> {
        self.witness.entry_path(self.parent_id, &self.name)
    }

    fn open_parent(&self, operation: &'static str) -> Result<File, Error> {
        let path = self.display_path()?;
        self.deadline.check(&path)?;
        self.anchor.verify_path_node()?;
        let parent = self.witness.open_directory(self.parent_id, &path)?;
        let parent_metadata = metadata(&parent, operation, &path)?;
        if NodeIdentity::from_metadata(&parent_metadata) != self.witness.directory_identity(self.parent_id)? {
            return Err(changed(&path, "package entry parent was replaced"));
        }
        self.deadline.check(&path)?;
        Ok(parent)
    }

    fn verify(&self) -> Result<(), Error> {
        let path = self.display_path()?;
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
        let path = self.display_path()?;
        let expected_hash = match self.kind {
            VerifiedKind::Regular { hash } => hash,
            _ => return Err(Error::UnverifiedContent { path }),
        };
        self.verify()?;
        let parent = self.open_parent("open verified package parent")?;
        let parent_metadata = metadata(&parent, "open verified package parent", &path)?;
        let parent_identity = self.witness.directory_identity(self.parent_id)?;
        if NodeIdentity::from_metadata(&parent_metadata) != parent_identity {
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
            witness: Arc::clone(&self.witness),
            parent_id: self.parent_id,
            parent_identity,
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
        let path = self.display_path()?;
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
        if new_snapshot.node != self.snapshot.node {
            self.witness.poison();
            return Err(changed(
                &path,
                "analyzed regular file was replaced without an authenticated transition",
            ));
        }
        if self.snapshot.links != 1 || new_snapshot.links != 1 {
            self.witness.poison();
            return Err(changed(
                &path,
                "in-place analyzer mutation of multiply-linked files is not supported",
            ));
        }
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

        let hash = hasher.digest128();
        let target = match &layout.file {
            StonePayloadLayoutFile::Regular(_, target) => target.clone(),
            _ => return Err(Error::UnverifiedContent { path }),
        };
        let new_layout = StonePayloadLayoutRecord {
            uid: current_metadata.uid(),
            gid: current_metadata.gid(),
            mode: current_metadata.mode(),
            tag: layout.tag,
            file: StonePayloadLayoutFile::Regular(hash, target),
        };
        if let Err(error) =
            self.witness
                .restate_regular(self.parent_id, &self.name, self.snapshot, new_snapshot, hash, &path)
        {
            self.witness.poison();
            return Err(error);
        }
        *layout = new_layout;
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
        let verified = self.verified.as_mut().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        let result = verified.restat_regular(&mut self.layout, &mut self.size, hasher);
        if result.is_err() {
            verified.witness.poison();
        }
        result
    }

    pub(crate) fn check_deadline(&self) -> Result<(), Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .deadline
            .check(&self.path)
    }

    pub(crate) fn remaining_time(&self) -> Result<Duration, Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .deadline
            .remaining(&self.path)
    }

    pub(crate) fn inventory_contains_regular_target(&self, target: &Path) -> Result<bool, Error> {
        let verified = self.verified.as_ref().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        verified.deadline.check(target)?;
        let relative = target.strip_prefix("/").unwrap_or(target);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Ok(false);
        }
        verified.witness.contains_regular(relative)
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
    witness: Arc<WitnessGraph>,
    parent_id: DirectoryId,
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
        let parent = self.witness.open_directory(self.parent_id, &self.path)?;
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
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

    fn remaining(&self, path: &Path) -> Result<Duration, Error> {
        let elapsed = self.started.elapsed();
        if elapsed >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(self.limit - elapsed)
        }
    }
}

fn hash_inventory_regular(
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
    let opened = metadata(&file, "stat inventory package file", display_path)?;
    if !opened.file_type().is_file() {
        return Err(changed(
            display_path,
            "inventory entry stopped being a regular file before hashing",
        ));
    }
    require_snapshot(display_path, expected, &opened)?;
    hasher.reset();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut bytes = 0u64;
    loop {
        context.check_time(display_path)?;
        let read = file.read(&mut buffer).map_err(|source| Error::Io {
            operation: "hash inventory package file",
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
            return Err(changed(display_path, "inventory regular file grew while hashing"));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected.size {
        return Err(changed(
            display_path,
            "inventory regular file changed size while hashing",
        ));
    }
    require_snapshot(
        display_path,
        expected,
        &metadata(&file, "restat inventory package file", display_path)?,
    )?;
    verify_entry_collection(parent, name, expected, display_path)?;
    Ok(hasher.digest128())
}

#[allow(clippy::too_many_arguments)]
fn capture_entry_witness(
    context: &CollectionContext,
    parent: &DirectoryHandle,
    name: &OsStr,
    handle: &File,
    entry_metadata: &Metadata,
    snapshot: FileSnapshot,
    path: &Path,
    hasher: &mut StoneDigestWriterHasher,
) -> Result<EntryWitness, Error> {
    let file_type = entry_metadata.file_type();
    let kind = if file_type.is_symlink() {
        let target = read_symlink_handle(handle, path, context)?;
        verify_entry_collection(parent, name, snapshot, path)?;
        WitnessEntryKind::Symlink { target }
    } else if file_type.is_file() {
        context.admit_regular(snapshot.size, path)?;
        WitnessEntryKind::Regular {
            hash: hash_inventory_regular(context, parent, name, snapshot, path, hasher)?,
        }
    } else if is_supported_special(&file_type) {
        verify_entry_collection(parent, name, snapshot, path)?;
        WitnessEntryKind::Special
    } else {
        return Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: "unknown special inode",
        });
    };
    Ok(EntryWitness { snapshot, kind })
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

fn require_usable_phase(state: &WitnessState, operation: &'static str) -> Result<(), Error> {
    match state.phase {
        WitnessPhase::AdmissionsOpen | WitnessPhase::Sealed => Ok(()),
        WitnessPhase::Poisoned => Err(Error::InventoryPoisoned),
        phase => Err(Error::InvalidInventoryPhase {
            operation,
            phase: phase.name(),
        }),
    }
}

fn find_child<'a>(directories: &'a [DirectoryWitness], parent: DirectoryId, name: &OsStr) -> Option<&'a WitnessChild> {
    let children = &directories.get(parent)?.children;
    children
        .binary_search_by(|child| child.name.as_os_str().cmp(name))
        .ok()
        .map(|position| &children[position])
}

fn lookup_directory(directories: &[DirectoryWitness], relative: &Path) -> Option<DirectoryId> {
    let mut id = 0;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        let child = find_child(directories, id, name)?;
        let WitnessChildKind::Directory(child_id) = &child.kind else {
            return None;
        };
        id = *child_id;
    }
    Some(id)
}

fn directory_relative(
    directories: &[DirectoryWitness],
    mut id: DirectoryId,
    display_root: &Path,
) -> Result<PathBuf, Error> {
    let mut lineage = Vec::new();
    loop {
        let directory = directories.get(id).ok_or_else(|| Error::UnwitnessedPath {
            path: display_root.to_owned(),
        })?;
        let Some(parent) = directory.parent else {
            break;
        };
        reserve(&mut lineage, 1, "witnessed directory lineage")?;
        lineage.push(directory.name.as_os_str());
        id = parent;
    }
    let mut relative = PathBuf::new();
    for name in lineage.into_iter().rev() {
        relative.push(name);
    }
    Ok(relative)
}

fn compare_exact_inventory(
    expected: &[DirectoryWitness],
    actual: &[DirectoryWitness],
    root: &Path,
    deadline: &Deadline,
) -> Result<(), Error> {
    if expected.is_empty() || actual.is_empty() {
        return Err(changed(root, "complete witnessed package inventory root changed"));
    }
    let mut tasks = Vec::new();
    reserve(&mut tasks, 1, "package inventory comparison tasks")?;
    tasks.push((0usize, 0usize));
    while let Some((expected_id, actual_id)) = tasks.pop() {
        deadline.check(root)?;
        let expected_directory = expected
            .get(expected_id)
            .ok_or_else(|| changed(root, "invalid witnessed directory edge"))?;
        let actual_directory = actual
            .get(actual_id)
            .ok_or_else(|| changed(root, "invalid scanned directory edge"))?;
        if expected_directory.snapshot != actual_directory.snapshot
            || expected_directory.children.len() != actual_directory.children.len()
        {
            return Err(changed(root, "complete witnessed package directory changed"));
        }
        for (expected_child, actual_child) in expected_directory.children.iter().zip(&actual_directory.children) {
            deadline.check(root)?;
            if expected_child.name != actual_child.name {
                return Err(changed(root, "complete witnessed package membership changed"));
            }
            match (&expected_child.kind, &actual_child.kind) {
                (WitnessChildKind::Directory(expected), WitnessChildKind::Directory(actual)) => {
                    reserve(&mut tasks, 1, "package inventory comparison tasks")?;
                    tasks.push((*expected, *actual));
                }
                (WitnessChildKind::Entry(expected), WitnessChildKind::Entry(actual)) if expected == actual => {}
                _ => return Err(changed(root, "complete witnessed package entry changed")),
            }
        }
    }
    Ok(())
}

fn stable_directory_snapshot(expected: FileSnapshot, actual: FileSnapshot) -> bool {
    expected.node == actual.node
        && expected.mode == actual.mode
        && expected.uid == actual.uid
        && expected.gid == actual.gid
}

fn add_admission_delta_for_relative(
    relative: &Path,
    child: &WitnessChild,
    display_path: &Path,
    delta: &mut AdmissionDelta,
) -> Result<(), Error> {
    delta.entries = delta.entries.checked_add(1).ok_or(Error::ArithmeticOverflow {
        resource: "generated package entries",
        path: display_path.to_owned(),
    })?;
    delta.name_bytes =
        delta
            .name_bytes
            .checked_add(child.name.as_bytes().len() as u64)
            .ok_or(Error::ArithmeticOverflow {
                resource: "generated package entry name bytes",
                path: display_path.to_owned(),
            })?;
    delta.path_bytes = delta
        .path_bytes
        .checked_add(relative.as_os_str().as_bytes().len() as u64)
        .ok_or(Error::ArithmeticOverflow {
            resource: "generated package entry path bytes",
            path: display_path.to_owned(),
        })?;
    if let WitnessChildKind::Entry(entry) = &child.kind {
        match &entry.kind {
            WitnessEntryKind::Regular { .. } => {
                delta.regular_bytes =
                    delta
                        .regular_bytes
                        .checked_add(entry.snapshot.size)
                        .ok_or(Error::ArithmeticOverflow {
                            resource: "generated package regular bytes",
                            path: display_path.to_owned(),
                        })?;
            }
            WitnessEntryKind::Symlink { target } => {
                delta.symlink_target_bytes =
                    delta
                        .symlink_target_bytes
                        .checked_add(target.len() as u64)
                        .ok_or(Error::ArithmeticOverflow {
                            resource: "generated package symlink target bytes",
                            path: display_path.to_owned(),
                        })?;
            }
            WitnessEntryKind::Special => {}
        }
    }
    Ok(())
}

fn usage_after_admission(
    usage: &CollectionUsage,
    delta: &AdmissionDelta,
    limits: CollectionLimits,
    path: &Path,
) -> Result<CollectionUsage, Error> {
    let mut updated = usage.clone();
    updated.entries = checked_add_limit(
        "total entries",
        updated.entries,
        delta.entries,
        limits.max_entries,
        path,
    )?;
    updated.name_bytes = checked_add_limit(
        "total entry name bytes",
        updated.name_bytes,
        delta.name_bytes,
        limits.max_total_name_bytes,
        path,
    )?;
    updated.path_bytes = checked_add_limit(
        "total entry path bytes",
        updated.path_bytes,
        delta.path_bytes,
        limits.max_total_path_bytes,
        path,
    )?;
    updated.symlink_target_bytes = checked_add_limit(
        "total symlink target bytes",
        updated.symlink_target_bytes,
        delta.symlink_target_bytes,
        limits.max_total_symlink_target_bytes,
        path,
    )?;
    updated.regular_bytes = checked_add_limit(
        "total regular file bytes",
        updated.regular_bytes,
        delta.regular_bytes,
        limits.max_total_regular_bytes,
        path,
    )?;
    Ok(updated)
}

fn validate_usage(usage: &CollectionUsage, limits: CollectionLimits, path: &Path) -> Result<(), Error> {
    enforce_u64_limit("total entries", limits.max_entries, usage.entries, path)?;
    enforce_u64_limit(
        "total entry name bytes",
        limits.max_total_name_bytes,
        usage.name_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total entry path bytes",
        limits.max_total_path_bytes,
        usage.path_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total symlink target bytes",
        limits.max_total_symlink_target_bytes,
        usage.symlink_target_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total regular file bytes",
        limits.max_total_regular_bytes,
        usage.regular_bytes,
        path,
    )
}

fn reserve_admission_commit(directories: &mut Vec<DirectoryWitness>, draft: &AdmissionDraft) -> Result<(), Error> {
    reserve(
        directories,
        draft.new_directories.len(),
        "generated package directories",
    )?;
    for update in &draft.existing {
        reserve(
            &mut directories[update.id].children,
            update.additions.len(),
            "generated package child edges",
        )?;
    }
    Ok(())
}

fn commit_admission(directories: &mut Vec<DirectoryWitness>, draft: AdmissionDraft) {
    for update in draft.existing {
        let directory = &mut directories[update.id];
        directory.snapshot = update.snapshot;
        for child in update.additions {
            let position = directory
                .children
                .binary_search_by(|candidate| candidate.name.cmp(&child.name))
                .expect_err("generated child was proven absent before commit");
            directory.children.insert(position, child);
        }
    }
    directories.extend(draft.new_directories);
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
    #[error("generated package path {path} was declared more than once", path = path.display())]
    DuplicateAdmission { path: PathBuf },
    #[error("generated package path {path} was already present in the initial inventory", path = path.display())]
    ExistingAdmission { path: PathBuf },
    #[error("cannot {operation} while package inventory is {phase}")]
    InvalidInventoryPhase {
        operation: &'static str,
        phase: &'static str,
    },
    #[error("package inventory is poisoned by an incomplete or failed transition")]
    InventoryPoisoned,
    #[error("package path {path} is not present in the authenticated inventory", path = path.display())]
    UnwitnessedPath { path: PathBuf },
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

    #[test]
    fn complete_inventory_witnesses_ignored_entries_and_an_empty_root() {
        let root = tempfile::tempdir().unwrap();
        let included = write_file(root.path(), "included", 0o644);
        let ignored = write_file(root.path(), "ignored", 0o644);
        let collector = all_collector(root.path());
        let paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        assert_eq!(paths.len(), 2);
        drop(paths.into_iter().find(|path| path.path == ignored));
        fs::write(&ignored, b"changed").unwrap();
        assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
        assert!(included.exists());

        let root = tempfile::tempdir().unwrap();
        let collector = all_collector(root.path());
        assert!(
            collector
                .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
                .unwrap()
                .is_empty()
        );
        fs::write(root.path().join("late"), b"late").unwrap();
        assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
    }

    #[test]
    fn generated_admission_is_exact_batched_and_allows_only_declared_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let collector = all_collector(root.path());
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let first = write_file(root.path(), "new/a", 0o644);
        let second = write_file(root.path(), "new/b", 0o644);
        let paths = collector
            .paths(&[first.clone(), second.clone()], &mut StoneDigestWriterHasher::new())
            .unwrap();
        assert_eq!(paths.len(), 2);
        collector.seal().unwrap().verify().unwrap();

        let root = tempfile::tempdir().unwrap();
        let collector = all_collector(root.path());
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let declared = write_file(root.path(), "nested/generated/leaf", 0o644);
        write_file(root.path(), "nested/generated/extra", 0o644);
        assert!(matches!(
            collector.paths(&[declared], &mut StoneDigestWriterHasher::new()),
            Err(Error::TreeChanged { .. })
        ));
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn generated_admission_rejects_existing_terminals_and_duplicate_declarations() {
        let root = tempfile::tempdir().unwrap();
        let existing = write_file(root.path(), "existing", 0o644);
        let collector = all_collector(root.path());
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        assert!(matches!(
            collector.paths(&[existing], &mut StoneDigestWriterHasher::new()),
            Err(Error::ExistingAdmission { .. })
        ));

        let root = tempfile::tempdir().unwrap();
        let collector = all_collector(root.path());
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let generated = write_file(root.path(), "generated", 0o644);
        assert!(matches!(
            collector.paths(&[generated.clone(), generated], &mut StoneDigestWriterHasher::new()),
            Err(Error::DuplicateAdmission { .. })
        ));

        let root = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(root.path());
        add_rule(&mut collector, "/initial", "out", PathRuleKind::Any);
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let generated = write_file(root.path(), "generated", 0o644);
        assert!(matches!(
            collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
            Err(Error::NoMatchingRule { .. })
        ));
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn every_complete_rescan_enforces_exact_aggregate_limits() {
        let root = tempfile::tempdir().unwrap();
        write_file(root.path(), "one", 0o644);
        write_file(root.path(), "two", 0o644);
        let mut limits = CollectionLimits::default();
        limits.max_entries = 2;
        let collector = collector_with_limits(root.path(), limits);
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        write_file(root.path(), "one", 0o644);
        let mut limits = CollectionLimits::default();
        limits.max_entries = 1;
        let collector = collector_with_limits(root.path(), limits);
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let generated = write_file(root.path(), "two", 0o644);
        assert!(matches!(
            collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "total entries",
                limit: 1,
                actual: 2,
                ..
            })
        ));
    }

    #[test]
    fn declaration_trie_accepts_exact_remaining_entries_and_rejects_n_plus_one_before_admission() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("a/b")).unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_entries = 3;
        let collector = collector_with_limits(root.path(), limits);
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let generated = write_file(root.path(), "a/b/c", 0o644);
        collector
            .paths(&[generated], &mut StoneDigestWriterHasher::new())
            .unwrap();
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("a/b")).unwrap();
        let collector = collector_with_limits(root.path(), limits);
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let generated = write_file(root.path(), "a/b/c/d", 0o644);
        assert!(matches!(
            collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "total entries",
                limit: 3,
                actual: 4,
                ..
            })
        ));
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn strict_restat_accepts_same_inode_and_rejects_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let collector = all_collector(root.path());
        let mut info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        fs::write(&path, b"mutated in place").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        info.restat(&mut StoneDigestWriterHasher::new()).unwrap();
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let collector = all_collector(root.path());
        let mut info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        fs::rename(&path, root.path().join("old")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(matches!(
            info.restat(&mut StoneDigestWriterHasher::new()),
            Err(Error::TreeChanged { .. })
        ));
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn restat_accepts_the_exact_file_limit_and_every_failure_poisons() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let mut limits = CollectionLimits::default();
        limits.max_file_bytes = 8;
        limits.max_total_regular_bytes = 8;
        let collector = collector_with_limits(root.path(), limits);
        let mut info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        fs::write(&path, b"12345678").unwrap();
        info.restat(&mut StoneDigestWriterHasher::new()).unwrap();
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        limits.max_file_bytes = 7;
        limits.max_total_regular_bytes = 7;
        let collector = collector_with_limits(root.path(), limits);
        let mut info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        fs::write(&path, b"12345678").unwrap();
        assert!(matches!(
            info.restat(&mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "regular file bytes",
                limit: 7,
                actual: 8,
                ..
            })
        ));
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn sealed_inventory_rejects_late_add_delete_metadata_and_replacement() {
        for action in 0..4 {
            let root = tempfile::tempdir().unwrap();
            let path = write_file(root.path(), "file", 0o644);
            let collector = all_collector(root.path());
            collector
                .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
                .unwrap();
            let sealed = collector.seal().unwrap();
            match action {
                0 => fs::write(root.path().join("added"), b"added").unwrap(),
                1 => fs::remove_file(&path).unwrap(),
                2 => fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap(),
                3 => {
                    fs::rename(&path, root.path().join("old")).unwrap();
                    fs::write(&path, b"payload").unwrap();
                }
                _ => unreachable!(),
            }
            assert!(matches!(sealed.verify(), Err(Error::TreeChanged { .. })));
        }
    }

    #[test]
    fn sealed_phase_rejects_admission_and_restat_transitions() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let collector = all_collector(root.path());
        let mut info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        collector.seal().unwrap();
        let generated = write_file(root.path(), "generated", 0o644);
        assert!(matches!(
            collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
            Err(Error::InvalidInventoryPhase { phase: "sealed", .. })
        ));

        fs::write(&path, b"same inode mutation").unwrap();
        assert!(matches!(
            info.restat(&mut StoneDigestWriterHasher::new()),
            Err(Error::InvalidInventoryPhase { phase: "sealed", .. })
        ));
    }

    #[test]
    fn regular_target_lookup_is_inventory_only_and_phase_checked() {
        let root = tempfile::tempdir().unwrap();
        let regular = write_file(root.path(), "usr/lib/pkgconfig/lib.pc", 0o644);
        symlink("lib.pc", root.path().join("usr/lib/pkgconfig/link.pc")).unwrap();
        let collector = all_collector(root.path());
        let paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let info = paths
            .iter()
            .find(|info| info.target_path == Path::new("/usr/lib/pkgconfig/lib.pc"))
            .unwrap();
        assert!(
            info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc"))
                .unwrap()
        );
        assert!(
            !info
                .inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/link.pc"))
                .unwrap()
        );
        assert!(
            !info
                .inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/missing.pc"))
                .unwrap()
        );
        fs::remove_file(regular).unwrap();
        assert!(
            info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc"))
                .unwrap()
        );
        assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
        assert!(matches!(
            info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc")),
            Err(Error::InventoryPoisoned)
        ));
    }
}
