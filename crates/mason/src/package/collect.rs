// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::{File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use nix::libc::{self, S_IFDIR, S_IRGRP, S_IROTH, S_IRWXU, S_IXGRP, S_IXOTH};
use stone::StoneDigestWriterHasher;
#[cfg(test)]
use stone::StonePayloadLayoutFile;
use stone_recipe::derivation::PathRuleKind;
mod error;
mod filesystem;
mod inventory;
mod mutation;
mod publication;
mod routing;
mod traversal;
mod verified_path;

pub use error::Error;
use error::TestPoint;
pub(crate) use inventory::SealedTree;
use inventory::{
    AdmissionDelta, AdmissionDraft, DirectoryId, DirectoryWitness, EntryWitness, WitnessChild, WitnessChildKind,
    WitnessEntryKind, WitnessGraph, WitnessPhase, WitnessState,
};
use traversal::{DirectoryHandle, FileSnapshot, NodeIdentity, RootAnchor, Task};
pub use verified_path::PathInfo;
// Preserve the established crate-visible type path for inferred reader values.
#[allow(unused_imports)]
pub(crate) use verified_path::VerifiedFileReader;
pub(crate) use verified_path::VerifiedPath;
use verified_path::{VerifiedKind, layout_from_metadata};

use filesystem::{
    CollectionContext, CollectionUsage, Deadline, c_name, changed, checked_add_limit, copy_os_string, copy_string,
    directory_relative, enforce_u64_limit, enforce_usize_limit, find_child, is_supported_special, join_relative,
    metadata, open_entry, open_entry_handle, read_directory_names, read_symlink_handle, relative_to_root,
    require_snapshot, reserve, split_parent_name, stable_directory_snapshot, unsupported_file_type_kind,
    verify_directory_collection, verify_entry_collection,
};

pub(crate) use publication::{GeneratedArtifact, GeneratedTimes};
pub use routing::Rule;
pub(crate) use routing::ProjectedPathKind;
pub(crate) use verified_path::WitnessedTargetState;

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
        let compiled = Rule::compile(pattern_owned, descendant_owned, kind)?;
        let package = if let Some(rule) = self.rules.iter().find(|rule| rule.package.as_ref() == package) {
            Arc::clone(&rule.package)
        } else {
            Arc::<str>::from(copy_string(package, "rule package bytes")?)
        };
        reserve(&mut self.rules, 1, "collection rules")?;
        self.rules.push(compiled.bind_package(package));
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

    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
            return Err(Error::UnsupportedFileType {
                path: display_path,
                kind: unsupported_file_type_kind(&file_type),
            });
        } else {
            return Err(Error::UnsupportedFileType {
                path: display_path,
                kind: unsupported_file_type_kind(&file_type),
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

#[cfg(test)]
mod tests;
