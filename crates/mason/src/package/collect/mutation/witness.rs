use super::super::{CollectionLimits, EntryWitness, RootAnchor};
use super::{
    verification::{require_exact_membership, require_exact_snapshot},
    *,
};

pub(super) struct ReplacementWitness<'a> {
    state: MutexGuard<'a, WitnessState>,
    usage: MutexGuard<'a, CollectionUsage>,
    anchor: &'a RootAnchor,
    limits: CollectionLimits,
    pub(super) deadline: &'a Deadline,
    parent: DirectoryId,
    position: usize,
    expected_parent: FileSnapshot,
    pub(super) expected_file: FileSnapshot,
    pub(super) expected_hash: u128,
    base_regular_bytes: u64,
    cleanup_timeout: Duration,
}

impl<'a> ReplacementWitness<'a> {
    pub(super) fn begin(
        witness: &'a WitnessGraph,
        verified: &VerifiedPath,
        parent: &File,
        expected_hash: u128,
        cleanup_timeout: Duration,
        path: &Path,
    ) -> Result<Self, Error> {
        witness.deadline.check(path)?;
        let state = witness.state.lock().map_err(|_| Error::StatePoisoned)?;
        match state.phase {
            WitnessPhase::AdmissionsOpen => {}
            WitnessPhase::Poisoned => return Err(Error::InventoryPoisoned),
            phase => {
                return Err(Error::InvalidInventoryPhase {
                    operation: "replace witnessed regular file",
                    phase: phase.name(),
                });
            }
        }
        let directory = state
            .directories
            .get(verified.parent_id)
            .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
        let position = directory
            .children
            .binary_search_by(|child| child.name.as_os_str().cmp(&verified.name))
            .map_err(|_| Error::UnwitnessedPath { path: path.to_owned() })?;
        let WitnessChildKind::Entry(entry) = &directory.children[position].kind else {
            return Err(changed(
                path,
                "regular-file replacement target was witnessed as a directory",
            ));
        };
        let WitnessEntryKind::Regular { hash } = &entry.kind else {
            return Err(changed(path, "regular-file replacement target lacks a regular witness"));
        };
        if entry.snapshot != verified.snapshot || *hash != expected_hash || entry.snapshot.links != 1 {
            return Err(changed(path, "regular-file replacement target has a stale witness"));
        }
        let expected_parent = directory.snapshot;
        require_exact_snapshot(
            path,
            expected_parent,
            &metadata(parent, "authenticate regular-file replacement parent", path)?,
            "regular-file replacement parent changed before staging",
        )?;
        let usage = witness.usage.lock().map_err(|_| Error::StatePoisoned)?;
        let base_regular_bytes =
            usage
                .regular_bytes
                .checked_sub(verified.snapshot.size)
                .ok_or(Error::ArithmeticOverflow {
                    resource: "total regular file bytes",
                    path: path.to_owned(),
                })?;
        witness.deadline.check(path)?;
        Ok(Self {
            state,
            usage,
            anchor: witness.anchor.as_ref(),
            limits: witness.limits,
            deadline: &witness.deadline,
            parent: verified.parent_id,
            position,
            expected_parent,
            expected_file: verified.snapshot,
            expected_hash,
            base_regular_bytes,
            cleanup_timeout,
        })
    }

    pub(super) fn cleanup_deadline(&self) -> Deadline {
        Deadline::new(self.cleanup_timeout)
    }

    pub(super) fn projected_regular_bytes(&self, replacement_bytes: u64, path: &Path) -> Result<u64, Error> {
        enforce_u64_limit(
            "regular file bytes",
            self.limits.max_file_bytes,
            replacement_bytes,
            path,
        )?;
        let projected = self
            .base_regular_bytes
            .checked_add(replacement_bytes)
            .ok_or(Error::ArithmeticOverflow {
                resource: "total regular file bytes",
                path: path.to_owned(),
            })?;
        enforce_u64_limit(
            "total regular file bytes",
            self.limits.max_total_regular_bytes,
            projected,
            path,
        )?;
        Ok(projected)
    }

    pub(super) fn require_original(
        &self,
        parent: &File,
        original: &File,
        name: &OsStr,
        expected: FileSnapshot,
        temporary: Option<&OsStr>,
        path: &Path,
    ) -> Result<(), Error> {
        self.deadline.check(path)?;
        self.require_membership(parent, temporary, path)?;
        require_exact_snapshot(
            path,
            expected,
            &metadata(original, "reauthenticate retained original regular file", path)?,
            "retained original regular file changed while staging",
        )?;
        let handle = open_entry_handle(parent, name, path)?;
        require_exact_snapshot(
            path,
            expected,
            &metadata(&handle, "reauthenticate named original regular file", path)?,
            "named original regular file changed while staging",
        )?;
        self.deadline.check(path)
    }

    pub(super) fn require_membership(
        &self,
        parent: &File,
        temporary: Option<&OsStr>,
        path: &Path,
    ) -> Result<FileSnapshot, Error> {
        self.require_membership_until(parent, temporary, self.deadline, path)
    }

    pub(super) fn require_membership_until(
        &self,
        parent: &File,
        temporary: Option<&OsStr>,
        deadline: &Deadline,
        path: &Path,
    ) -> Result<FileSnapshot, Error> {
        let snapshot = require_exact_membership(
            parent,
            &self.state.directories[self.parent].children,
            temporary,
            deadline,
            path,
        )?;
        if !stable_directory_snapshot(self.expected_parent, snapshot) {
            return Err(changed(
                path,
                "regular-file replacement parent identity or metadata changed",
            ));
        }
        Ok(snapshot)
    }

    /// Reopen the complete witnessed directory chain from the live root path
    /// and prove that it terminates at the exact retained parent descriptor.
    /// Checking only the final inode would miss an ancestor which was replaced
    /// while the original directory stayed reachable through another name.
    pub(super) fn require_anchored_parent(
        &self,
        parent: &File,
        deadline: &Deadline,
        path: &Path,
    ) -> Result<FileSnapshot, Error> {
        deadline.check(path)?;
        self.anchor.verify_path_node()?;
        let relative = directory_relative(&self.state.directories, self.parent, &self.anchor.path)?;
        let mut id = 0usize;
        let mut anchored = self.anchor.open_directory(Path::new(""))?;

        authenticate_anchored_directory(&self.state, id, self.parent, self.expected_parent, &anchored, path)?;
        for component in relative.components() {
            deadline.check(path)?;
            let Component::Normal(name) = component else {
                return Err(changed(path, "witnessed parent path stopped being normalized"));
            };
            let child = find_child(&self.state.directories, id, name)
                .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?;
            let WitnessChildKind::Directory(child_id) = &child.kind else {
                return Err(changed(path, "witnessed parent ancestor changed to a non-directory"));
            };
            anchored = open_entry(
                &anchored,
                name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                path,
            )?;
            id = *child_id;
            authenticate_anchored_directory(&self.state, id, self.parent, self.expected_parent, &anchored, path)?;
        }
        if id != self.parent {
            return Err(changed(
                path,
                "witnessed parent lineage terminated at the wrong directory",
            ));
        }

        let anchored_snapshot = FileSnapshot::from_metadata(&metadata(
            &anchored,
            "reauthenticate anchored regular-file replacement parent",
            path,
        )?);
        let retained_snapshot = FileSnapshot::from_metadata(&metadata(
            parent,
            "reauthenticate retained regular-file replacement parent",
            path,
        )?);
        if anchored_snapshot != retained_snapshot {
            return Err(changed(
                path,
                "regular-file replacement parent detached from its witnessed path",
            ));
        }
        deadline.check(path)?;
        Ok(retained_snapshot)
    }

    pub(super) fn commit_replacement(
        &mut self,
        replacement: FileSnapshot,
        parent: FileSnapshot,
        hash: u128,
        projected_regular_bytes: u64,
    ) {
        self.state.directories[self.parent].snapshot = parent;
        self.state.directories[self.parent].children[self.position].kind = WitnessChildKind::Entry(EntryWitness {
            snapshot: replacement,
            kind: WitnessEntryKind::Regular { hash },
        });
        self.usage.regular_bytes = projected_regular_bytes;
    }

    pub(super) fn commit_rollback(&mut self, original: FileSnapshot, parent: FileSnapshot) {
        self.state.directories[self.parent].snapshot = parent;
        self.state.directories[self.parent].children[self.position].kind = WitnessChildKind::Entry(EntryWitness {
            snapshot: original,
            kind: WitnessEntryKind::Regular {
                hash: self.expected_hash,
            },
        });
    }

    pub(super) fn poison(&mut self) {
        self.state.phase = WitnessPhase::Poisoned;
    }
}

fn authenticate_anchored_directory(
    state: &WitnessState,
    id: DirectoryId,
    parent: DirectoryId,
    expected_parent: FileSnapshot,
    directory: &File,
    path: &Path,
) -> Result<(), Error> {
    let expected = state
        .directories
        .get(id)
        .ok_or_else(|| Error::UnwitnessedPath { path: path.to_owned() })?
        .snapshot;
    let current = FileSnapshot::from_metadata(&metadata(
        directory,
        "reauthenticate regular-file replacement ancestor",
        path,
    )?);
    let authenticated = if id == parent {
        stable_directory_snapshot(expected_parent, current)
    } else {
        expected == current
    };
    if authenticated {
        Ok(())
    } else {
        Err(changed(
            path,
            "regular-file replacement ancestor changed while the transaction was open",
        ))
    }
}
