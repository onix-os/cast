use std::{
    ffi::{OsStr, OsString},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use nix::libc;
use stone::StoneDigestWriterHasher;

use super::super::{
    CollectionLimits, Error,
    filesystem::{
        CollectionContext, CollectionUsage, Deadline, add_admission_delta_for_relative, capture_entry_witness, changed,
        commit_admission, compare_exact_inventory, directory_relative, find_child, hash_inventory_regular,
        is_supported_special, join_relative, lookup_directory, metadata, open_entry, open_entry_handle,
        read_directory_names, read_symlink_handle, require_snapshot, require_usable_phase, reserve,
        reserve_admission_commit, split_parent_name, stable_directory_snapshot, usage_after_admission, validate_usage,
        verify_directory_collection, verify_entry_collection,
    },
    traversal::{DirectoryHandle, FileSnapshot, InventoryTask, NodeIdentity, RootAnchor},
};
use super::model::{
    AdmissionDelta, AdmissionDraft, AdmissionTask, DeclaredTrie, DirectoryAdmission, DirectoryId, DirectoryWitness,
    EntryWitness, InventoryDraft, WitnessChild, WitnessChildKind, WitnessEntryKind, WitnessGraph, WitnessPhase,
    WitnessState,
};

impl WitnessGraph {
    pub(in crate::package::collect) fn new(
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

    pub(in crate::package::collect) fn ensure_initial_snapshot(self: &Arc<Self>) -> Result<(), Error> {
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

    pub(in crate::package::collect) fn seal(self: &Arc<Self>) -> Result<(), Error> {
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

    pub(in crate::package::collect) fn verify_sealed(self: &Arc<Self>) -> Result<(), Error> {
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

    pub(in crate::package::collect) fn poison(&self) {
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

    pub(in crate::package::collect) fn directory_id(&self, relative: &Path) -> Result<DirectoryId, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed directory")?;
        lookup_directory(&state.directories, relative).ok_or_else(|| Error::UnwitnessedPath {
            path: self.anchor.path.join(relative),
        })
    }

    pub(in crate::package::collect) fn require_path(&self, relative: &Path) -> Result<(), Error> {
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

    pub(in crate::package::collect) fn contains_regular(&self, relative: &Path) -> Result<bool, Error> {
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

    pub(in crate::package::collect) fn contains_symlink(&self, relative: &Path) -> Result<bool, Error> {
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
                    kind: WitnessEntryKind::Symlink { .. },
                    ..
                })
            )
        }))
    }

    pub(in crate::package::collect) fn child_directory_id(
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

    pub(in crate::package::collect) fn require_rewitnessed_directory(
        &self,
        parent: DirectoryId,
        name: &OsStr,
        collected: FileSnapshot,
        current: FileSnapshot,
        path: &Path,
    ) -> Result<(), Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "verify rewitnessed directory")?;
        let child = find_child(&state.directories, parent, name)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        let WitnessChildKind::Directory(directory) = &child.kind else {
            return Err(changed(path, "collected directory changed to a non-directory entry"));
        };
        let expected = state
            .directories
            .get(*directory)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        if stable_directory_snapshot(collected, current) && expected.snapshot == current {
            Ok(())
        } else {
            Err(changed(
                path,
                "collected directory changed without an authenticated transition",
            ))
        }
    }

    pub(in crate::package::collect) fn require_directory(
        &self,
        id: DirectoryId,
        snapshot: FileSnapshot,
        path: &Path,
    ) -> Result<(), Error> {
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

    pub(in crate::package::collect) fn directory_identity(&self, id: DirectoryId) -> Result<NodeIdentity, Error> {
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

    pub(in crate::package::collect) fn entry_path(&self, parent: DirectoryId, name: &OsStr) -> Result<PathBuf, Error> {
        let state = self.state.lock().map_err(|_| Error::StatePoisoned)?;
        require_usable_phase(&state, "resolve witnessed entry path")?;
        let mut relative = directory_relative(&state.directories, parent, &self.anchor.path)?;
        relative.push(name);
        Ok(self.anchor.path.join(relative))
    }

    pub(in crate::package::collect) fn open_directory(
        &self,
        id: DirectoryId,
        display_path: &Path,
    ) -> Result<File, Error> {
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
}

impl WitnessPhase {
    pub(in crate::package::collect) fn name(self) -> &'static str {
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

    pub(in crate::package::collect) fn admit_paths(self: &Arc<Self>, paths: &[PathBuf]) -> Result<(), Error> {
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
