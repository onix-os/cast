//! Bounded deletion after atomic per-entry detachment into a retained private directory.

use std::{
    ffi::{CStr, CString, OsString},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStringExt as _, fs::MetadataExt as _},
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    time::Instant,
};

use crate::linux_fs::{
    chmod_path_descriptor_until, controlled_resolution, openat2_file_until, renameat2_noreplace_once,
};

use super::{
    ArchivedStatePruneError, ArchivedStatePruneFaultPoint, ArchivedStatePruneLimits, RetainedDirectory,
    error::prune_io,
    fault_injection::{before_child_unlink, checkpoint},
};

const DELETE_DIRECTORY_NAME: &CStr = c"delete";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const DIRECTORY_ENTRY_BATCH: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EntryIdentity {
    device: u64,
    inode: u64,
    kind: u32,
}

#[derive(Debug)]
struct DetachedNode {
    file: std::fs::File,
    private_name: CString,
    private_path: PathBuf,
    source_parent: std::fs::File,
    source_parent_path: PathBuf,
    identity: EntryIdentity,
    depth: usize,
    directory: bool,
    move_synced: bool,
    drained: bool,
    unlinked: bool,
    unlink_synced: bool,
}

/// In-process retry state for one wrapper deletion.
///
/// Process death deliberately leaves the entire state-prune quarantine as
/// fail-closed evidence. Reopening it safely requires durable prune intent and
/// is outside this slice; no ambient private name is adopted.
#[derive(Debug)]
pub(super) struct DeletionPlan {
    scratch: RetainedDirectory,
    nodes: Vec<DetachedNode>,
    next_private_name: usize,
    wrapper_drained: bool,
    wrapper_detached: bool,
    scratch_unlinked: bool,
    scratch_sync_complete: bool,
}

/// Aggregate in-process retry budget for one prune batch.
///
/// The deadline is cooperative: it is checked around filesystem operations,
/// but Linux offers no safe cancellation for a blocking `readdir`, `unlink`,
/// or `fsync`. A wedged kernel/filesystem syscall can therefore exceed the
/// wall-clock limit before returning; this code makes no hard-wall claim.
#[derive(Debug)]
pub(super) struct DeleteBudget {
    limits: ArchivedStatePruneLimits,
    deadline: Instant,
    entries: usize,
    name_bytes: usize,
    operations: usize,
}

impl DeleteBudget {
    pub(super) fn new(limits: ArchivedStatePruneLimits) -> Self {
        Self {
            limits,
            deadline: Instant::now() + limits.time,
            entries: 0,
            name_bytes: 0,
            operations: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn usage(&self) -> (usize, Instant) {
        (self.operations, self.deadline)
    }

    fn check(&self, path: &Path) -> Result<(), ArchivedStatePruneError> {
        if Instant::now() > self.deadline {
            Err(ArchivedStatePruneError::Deadline { path: path.to_owned() })
        } else {
            Ok(())
        }
    }

    fn operation(&mut self, path: &Path) -> Result<(), ArchivedStatePruneError> {
        self.check(path)?;
        self.operations = self.operations.saturating_add(1);
        if self.operations > self.limits.operations {
            return Err(ArchivedStatePruneError::Boundary {
                boundary: "operation-count",
                limit: self.limits.operations,
                path: path.to_owned(),
            });
        }
        Ok(())
    }

    fn entry(&mut self, path: &Path, name: &[u8]) -> Result<(), ArchivedStatePruneError> {
        self.operation(path)?;
        self.entries = self.entries.saturating_add(1);
        if self.entries > self.limits.entries {
            return Err(ArchivedStatePruneError::Boundary {
                boundary: "entry-count",
                limit: self.limits.entries,
                path: path.to_owned(),
            });
        }
        self.name_bytes = self.name_bytes.saturating_add(name.len());
        if self.name_bytes > self.limits.name_bytes {
            return Err(ArchivedStatePruneError::Boundary {
                boundary: "name-byte-count",
                limit: self.limits.name_bytes,
                path: path.to_owned(),
            });
        }
        Ok(())
    }
}

impl DeletionPlan {
    pub(super) fn prepare(slot: &RetainedDirectory) -> Result<Self, ArchivedStatePruneError> {
        let path = slot.path.join(DELETE_DIRECTORY_NAME.to_string_lossy().as_ref());
        let scratch =
            RetainedDirectory::create_private_child(slot, DELETE_DIRECTORY_NAME, path.clone()).map_err(|source| {
                ArchivedStatePruneError::Identity {
                    state: 0,
                    path,
                    source: Box::new(source),
                }
            })?;
        scratch
            .sync("sync private archived-state deletion directory")
            .map_err(|source| ArchivedStatePruneError::Identity {
                state: 0,
                path: scratch.path.clone(),
                source: Box::new(source),
            })?;
        slot.sync("sync archived-state prune slot after deletion-directory creation")
            .map_err(|source| ArchivedStatePruneError::Identity {
                state: 0,
                path: slot.path.clone(),
                source: Box::new(source),
            })?;
        Ok(Self {
            scratch,
            nodes: Vec::new(),
            next_private_name: 0,
            wrapper_drained: false,
            wrapper_detached: false,
            scratch_unlinked: false,
            scratch_sync_complete: false,
        })
    }

    pub(super) fn run(
        &mut self,
        slot: &RetainedDirectory,
        wrapper: &RetainedDirectory,
        wrapper_name: &CStr,
        budget: &mut DeleteBudget,
    ) -> Result<(), ArchivedStatePruneError> {
        self.resume_pending_nodes(budget)?;
        if !self.wrapper_drained {
            self.drain_directory(&wrapper.file, &wrapper.path, 0, budget)?;
            sync_directory(&wrapper.file, &wrapper.path, budget)?;
            self.wrapper_drained = true;
        }

        if !self.wrapper_detached {
            let retained_before = self.nodes.len();
            let detached = self.detach_into_scratch(
                &slot.file,
                &slot.path,
                wrapper_name,
                &wrapper.file,
                &wrapper.path,
                0,
                true,
                true,
                budget,
            );
            if self.nodes.len() > retained_before {
                // The exact wrapper move applied even if a required sync in
                // its durable suffix failed. Retry must never reopen the now
                // absent public slot name.
                self.wrapper_detached = true;
            }
            detached?;
        }
        self.resume_pending_nodes(budget)?;
        self.retire_scratch(slot, budget)
    }

    fn drain_directory(
        &mut self,
        directory: &std::fs::File,
        path: &Path,
        depth: usize,
        budget: &mut DeleteBudget,
    ) -> Result<(), ArchivedStatePruneError> {
        if depth > budget.limits.depth {
            return Err(ArchivedStatePruneError::Boundary {
                boundary: "depth",
                limit: budget.limits.depth,
                path: path.to_owned(),
            });
        }
        loop {
            let names = directory_entry_batch(directory, path, budget)?;
            if names.is_empty() {
                return Ok(());
            }
            for name in names {
                let child_path = path.join(OsString::from_vec(name.to_bytes().to_vec()));
                let child = open_required_entry(directory, &name, &child_path, budget)?;
                let child_identity = identity(&child, &child_path)?;
                if child_identity.device != self.scratch.witness.device {
                    return Err(ArchivedStatePruneError::MountedEntry { path: child_path });
                }
                let is_directory = child_identity.kind == nix::libc::S_IFDIR;
                if is_directory && depth == budget.limits.depth {
                    return Err(ArchivedStatePruneError::Boundary {
                        boundary: "depth",
                        limit: budget.limits.depth,
                        path: child_path,
                    });
                }
                if is_directory {
                    // Permission changes are permitted only in RowsRemoved.
                    // Apply them through the exact retained inode before the
                    // rename as some filesystems require searchable directory
                    // mode even when the parent dirfds are already retained.
                    make_directory_readable(&child, &child_path, budget)?;
                }
                self.detach_into_scratch(
                    directory,
                    path,
                    &name,
                    &child,
                    &child_path,
                    depth + 1,
                    is_directory,
                    !is_directory,
                    budget,
                )?;
                self.process_last_node(budget)?;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn detach_into_scratch(
        &mut self,
        source_parent: &std::fs::File,
        source_parent_path: &Path,
        source_name: &CStr,
        retained: &std::fs::File,
        source_path: &Path,
        depth: usize,
        directory: bool,
        already_drained: bool,
        budget: &mut DeleteBudget,
    ) -> Result<(), ArchivedStatePruneError> {
        if self.nodes.len() >= budget.limits.retained_nodes {
            return Err(ArchivedStatePruneError::Boundary {
                boundary: "retained-node-count",
                limit: budget.limits.retained_nodes,
                path: source_path.to_owned(),
            });
        }
        let expected = identity(retained, source_path)?;
        if expected.device != self.scratch.witness.device || (expected.kind == nix::libc::S_IFDIR) != directory {
            return Err(ArchivedStatePruneError::MountedEntry {
                path: source_path.to_owned(),
            });
        }
        let private_name = self.next_available_private_name(budget)?;
        let private_path = self.scratch.path.join(private_name.to_string_lossy().as_ref());
        let retained_file = retained
            .try_clone()
            .map_err(|source| prune_io("retain prune entry before atomic detach", source_path, source))?;
        let source_parent_file = source_parent.try_clone().map_err(|source| {
            prune_io(
                "retain changed prune parent before atomic detach",
                source_parent_path,
                source,
            )
        })?;

        // Final source identity check immediately precedes the atomic detach.
        // The test hook lives in the real check/syscall window: a substituted
        // foreign inode is moved no-replace into the private directory, then
        // detected and preserved there rather than unlinked.
        let named = open_required_entry(source_parent, source_name, source_path, budget)?;
        if identity(&named, source_path)? != expected {
            return Err(ArchivedStatePruneError::EntryChanged {
                path: source_path.to_owned(),
            });
        }
        before_child_unlink();
        let syscall = renameat2_noreplace_once(source_parent, source_name, &self.scratch.file, &private_name);
        let source = observe_name(source_parent, source_name, expected, source_path, budget)?;
        let destination = observe_name(&self.scratch.file, &private_name, expected, &private_path, budget)?;
        match (source, destination) {
            (NameState::Absent, NameState::Exact) => {}
            (NameState::Exact, NameState::Absent) => {
                return Err(prune_io(
                    "atomically detach retained prune entry",
                    source_path,
                    syscall
                        .err()
                        .unwrap_or_else(|| io::Error::other("rename reported success without moving entry")),
                ));
            }
            _ => {
                return Err(ArchivedStatePruneError::EntryChanged {
                    path: source_path.to_owned(),
                });
            }
        }

        self.nodes.push(DetachedNode {
            file: retained_file,
            private_name,
            private_path,
            source_parent: source_parent_file,
            source_parent_path: source_parent_path.to_owned(),
            identity: expected,
            depth,
            directory,
            move_synced: false,
            drained: already_drained,
            unlinked: false,
            unlink_synced: false,
        });
        let index = self.nodes.len() - 1;
        self.resume_node_move_suffix(index, budget)
    }

    fn resume_pending_nodes(&mut self, budget: &mut DeleteBudget) -> Result<(), ArchivedStatePruneError> {
        while !self.nodes.is_empty() {
            self.process_last_node(budget)?;
        }
        Ok(())
    }

    fn process_last_node(&mut self, budget: &mut DeleteBudget) -> Result<(), ArchivedStatePruneError> {
        let index = self
            .nodes
            .len()
            .checked_sub(1)
            .ok_or_else(|| ArchivedStatePruneError::Boundary {
                boundary: "retained-node-count",
                limit: budget.limits.retained_nodes,
                path: self.scratch.path.clone(),
            })?;
        self.resume_node_move_suffix(index, budget)?;
        if self.nodes[index].directory && !self.nodes[index].drained {
            let (file, path, depth) = {
                let node = &self.nodes[index];
                (
                    node.file
                        .try_clone()
                        .map_err(|source| prune_io("clone detached directory", &node.private_path, source))?,
                    node.private_path.clone(),
                    node.depth,
                )
            };
            make_directory_readable(&file, &path, budget)?;
            let readable = open_retained_directory(&file, &path, budget)?;
            self.drain_directory(&readable, &path, depth, budget)?;
            sync_directory(&readable, &path, budget)?;
            self.nodes[index].drained = true;
        }
        self.unlink_private_node(index, budget)?;
        self.nodes.pop();
        Ok(())
    }

    fn resume_node_move_suffix(
        &mut self,
        index: usize,
        budget: &mut DeleteBudget,
    ) -> Result<(), ArchivedStatePruneError> {
        if self.nodes[index].move_synced {
            return Ok(());
        }
        let node = &self.nodes[index];
        if observe_name(
            &self.scratch.file,
            &node.private_name,
            node.identity,
            &node.private_path,
            budget,
        )? != NameState::Exact
        {
            return Err(ArchivedStatePruneError::EntryChanged {
                path: node.private_path.clone(),
            });
        }
        sync_directory(&node.source_parent, &node.source_parent_path, budget)?;
        sync_directory(&self.scratch.file, &self.scratch.path, budget)?;
        self.nodes[index].move_synced = true;
        Ok(())
    }

    fn unlink_private_node(&mut self, index: usize, budget: &mut DeleteBudget) -> Result<(), ArchivedStatePruneError> {
        if !self.nodes[index].unlinked {
            let node = &self.nodes[index];
            if node.directory && !node.drained {
                return Err(ArchivedStatePruneError::EntryChanged {
                    path: node.private_path.clone(),
                });
            }
            if observe_name(
                &self.scratch.file,
                &node.private_name,
                node.identity,
                &node.private_path,
                budget,
            )? != NameState::Exact
            {
                return Err(ArchivedStatePruneError::EntryChanged {
                    path: node.private_path.clone(),
                });
            }
            budget.operation(&node.private_path)?;
            let flags = if node.directory { nix::libc::AT_REMOVEDIR } else { 0 };
            // This name is inside the freshly created, retained 0700 deletion
            // directory and was populated only by the exact no-replace move
            // above. Mutable public/final names are never unlinked directly.
            // SAFETY: scratch and the private single-component name are live.
            let syscall = if unsafe {
                nix::libc::unlinkat(self.scratch.file.as_raw_fd(), node.private_name.as_ptr(), flags)
            } == 0
            {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            };
            match observe_name(
                &self.scratch.file,
                &node.private_name,
                node.identity,
                &node.private_path,
                budget,
            )? {
                NameState::Absent => self.nodes[index].unlinked = true,
                NameState::Exact => {
                    return Err(prune_io(
                        "unlink atomically detached private prune entry",
                        &node.private_path,
                        syscall
                            .err()
                            .unwrap_or_else(|| io::Error::other("unlink reported success but private name remains")),
                    ));
                }
                NameState::Foreign => {
                    return Err(ArchivedStatePruneError::NameReoccupied {
                        path: node.private_path.clone(),
                    });
                }
            }
            checkpoint(ArchivedStatePruneFaultPoint::AfterChildUnlink)?;
        }
        if !self.nodes[index].unlink_synced {
            checkpoint(ArchivedStatePruneFaultPoint::BeforeChangedParentSync)?;
            sync_directory(&self.scratch.file, &self.scratch.path, budget)?;
            self.nodes[index].unlink_synced = true;
            checkpoint(ArchivedStatePruneFaultPoint::AfterChangedParentSync)?;
        }
        Ok(())
    }

    fn retire_scratch(
        &mut self,
        slot: &RetainedDirectory,
        budget: &mut DeleteBudget,
    ) -> Result<(), ArchivedStatePruneError> {
        if !self.scratch_unlinked {
            remove_exact_private_directory(slot, DELETE_DIRECTORY_NAME, &self.scratch, budget)?;
            self.scratch_unlinked = true;
        }
        if !self.scratch_sync_complete {
            sync_directory(&slot.file, &slot.path, budget)?;
            self.scratch_sync_complete = true;
        }
        Ok(())
    }

    fn next_available_private_name(&mut self, budget: &mut DeleteBudget) -> Result<CString, ArchivedStatePruneError> {
        loop {
            let index = self.next_private_name;
            self.next_private_name = self.next_private_name.saturating_add(1);
            let name = CString::new(format!("entry-{index:016x}")).expect("hexadecimal private name has no NUL");
            let path = self.scratch.path.join(name.to_string_lossy().as_ref());
            match open_optional_entry(&self.scratch.file, &name, &path, budget)? {
                None => return Ok(name),
                Some(_) if index < budget.limits.operations => {}
                Some(_) => {
                    return Err(ArchivedStatePruneError::Boundary {
                        boundary: "private-name-attempts",
                        limit: budget.limits.operations,
                        path,
                    });
                }
            }
        }
    }
}

pub(super) fn remove_exact_private_directory(
    parent: &RetainedDirectory,
    name: &CStr,
    directory: &RetainedDirectory,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    let expected = identity(&directory.file, &directory.path)?;
    let entries = directory_entry_batch(&directory.file, &directory.path, budget)?;
    if !entries.is_empty() {
        return Err(ArchivedStatePruneError::EntryChanged {
            path: directory.path.clone(),
        });
    }
    match observe_name(&parent.file, name, expected, &directory.path, budget)? {
        NameState::Absent => return finish_private_directory_unlink(parent, budget),
        NameState::Exact => {}
        NameState::Foreign => {
            return Err(ArchivedStatePruneError::EntryChanged {
                path: directory.path.clone(),
            });
        }
    }
    budget.operation(&directory.path)?;
    // The directory was freshly created and retained by this session; unlike
    // package/public names it is not adopted from ambient namespace evidence.
    // SAFETY: parent and the private name remain live.
    let syscall =
        if unsafe { nix::libc::unlinkat(parent.file.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    match observe_name(&parent.file, name, expected, &directory.path, budget)? {
        NameState::Absent => finish_private_directory_unlink(parent, budget),
        NameState::Exact => Err(prune_io(
            "remove exact private prune directory",
            &directory.path,
            syscall
                .err()
                .unwrap_or_else(|| io::Error::other("rmdir reported success but private name remains")),
        )),
        NameState::Foreign => Err(ArchivedStatePruneError::NameReoccupied {
            path: directory.path.clone(),
        }),
    }
}

fn finish_private_directory_unlink(
    parent: &RetainedDirectory,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    checkpoint(ArchivedStatePruneFaultPoint::AfterPrivateDirectoryUnlink)?;
    sync_directory(&parent.file, &parent.path, budget)
}

fn make_directory_readable(
    file: &std::fs::File,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    budget.operation(path)?;
    let metadata = file
        .metadata()
        .map_err(|source| prune_io("inspect detached directory mode", path, source))?;
    let mode = metadata.mode() & 0o7777 | PRIVATE_DIRECTORY_MODE;
    chmod_path_descriptor_until(file, mode, budget.deadline)
        .map_err(|source| prune_io("make detached directory owner-accessible", path, source))
}

fn open_retained_directory(
    file: &std::fs::File,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<std::fs::File, ArchivedStatePruneError> {
    budget.operation(path)?;
    let expected = identity(file, path)?;
    let readable = openat2_file_until(
        file.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        budget.deadline,
    )
    .map_err(|source| prune_io("open atomically detached directory", path, source))?;
    if identity(&readable, path)? != expected {
        return Err(ArchivedStatePruneError::EntryChanged { path: path.to_owned() });
    }
    Ok(readable)
}

fn directory_entry_batch(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<Vec<CString>, ArchivedStatePruneError> {
    budget.operation(path)?;
    let cursor = openat2_file_until(
        directory.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        budget.deadline,
    )
    .map_err(|source| prune_io("open prune directory cursor", path, source))?;
    let descriptor = std::os::fd::IntoRawFd::into_raw_fd(cursor);
    // SAFETY: fdopendir consumes descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe { nix::libc::close(descriptor) };
        return Err(prune_io("enumerate prune directory", path, source));
    };
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        budget.check(path)?;
        // SAFETY: errno is thread-local and readdir uses null for EOF/error.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(prune_io(
                "enumerate prune directory",
                path,
                io::Error::from_raw_os_error(errno),
            ));
        }
        // SAFETY: returned name is NUL terminated until the next readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        budget.entry(path, name.to_bytes())?;
        names.push(name.to_owned());
        if names.len() == DIRECTORY_ENTRY_BATCH {
            break;
        }
    }
    names.sort_by(|left, right| left.to_bytes().cmp(right.to_bytes()));
    Ok(names)
}

struct DirectoryStream(NonNull<nix::libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the stream returned by fdopendir.
        unsafe { nix::libc::closedir(self.0.as_ptr()) };
    }
}

fn sync_directory(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    budget.operation(path)?;
    directory
        .sync_all()
        .map_err(|source| prune_io("sync changed prune directory", path, source))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameState {
    Absent,
    Exact,
    Foreign,
}

fn observe_name(
    parent: &std::fs::File,
    name: &CStr,
    expected: EntryIdentity,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<NameState, ArchivedStatePruneError> {
    Ok(match open_optional_entry(parent, name, path, budget)? {
        None => NameState::Absent,
        Some(file) if identity(&file, path)? == expected => NameState::Exact,
        Some(_) => NameState::Foreign,
    })
}

fn open_required_entry(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<std::fs::File, ArchivedStatePruneError> {
    open_optional_entry(parent, name, path, budget)?
        .ok_or_else(|| ArchivedStatePruneError::EntryChanged { path: path.to_owned() })
}

fn open_optional_entry(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    budget: &mut DeleteBudget,
) -> Result<Option<std::fs::File>, ArchivedStatePruneError> {
    budget.operation(path)?;
    match openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        budget.deadline,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) if source.raw_os_error() == Some(nix::libc::EXDEV) => {
            Err(ArchivedStatePruneError::MountedEntry { path: path.to_owned() })
        }
        Err(source) => Err(prune_io("open retained prune entry", path, source)),
    }
}

fn identity(file: &std::fs::File, path: &Path) -> Result<EntryIdentity, ArchivedStatePruneError> {
    let metadata = file
        .metadata()
        .map_err(|source| prune_io("inspect retained prune entry", path, source))?;
    Ok(EntryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        kind: metadata.mode() & nix::libc::S_IFMT,
    })
}
