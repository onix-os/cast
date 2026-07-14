
//! Collector-owned regular-file replacement.
//!
//! A replacement is constructed as an anonymous inode in the witnessed file's
//! own directory. Only a complete, metadata-normalized, durable inode is linked
//! into that directory, and publication uses `RENAME_EXCHANGE` so the old inode
//! remains named and available for rollback until finalization. There is no
//! named-incomplete fallback: a filesystem without linkable `O_TMPFILE` or
//! `RENAME_EXCHANGE` support fails closed.

// This is a descriptor/syscall boundary. `std::fs::File` is required because
// paths are deliberately not reopened through fs-err's pathname wrapper.
#![allow(clippy::disallowed_types)]

use std::{
    ffi::{CStr, OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
    ptr::NonNull,
    sync::{
        MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use nix::libc;
use stone::{StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};

#[cfg(test)]
use std::path::PathBuf;

use super::{
    CollectionUsage, Deadline, DirectoryId, Error, FileSnapshot, HASH_BUFFER_BYTES, NodeIdentity, PathInfo,
    VerifiedKind, VerifiedPath, WitnessChild, WitnessChildKind, WitnessEntryKind, WitnessGraph, WitnessPhase,
    WitnessState, c_name, changed, directory_relative, enforce_u64_limit, find_child, metadata, open_entry,
    open_entry_handle, stable_directory_snapshot,
};

const STAGE_NAME_ATTEMPTS: u64 = 128;
const STAGE_NAME_PREFIX: &str = ".mason-mutation-";
const MUTATION_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_STAGE_NAME: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationCheckpoint {
    BeforeStageLink,
    BeforeExchange,
    AfterExchange,
    BeforeFinalization,
    BeforeRetiredUnlink,
    AfterRetiredUnlink,
    BeforeWitnessCommit,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum MutationCheckpoint {
    BeforeStageLink,
    BeforeExchange,
    AfterExchange,
    BeforeFinalization,
    BeforeRetiredUnlink,
    AfterRetiredUnlink,
    BeforeWitnessCommit,
}

pub(super) fn replace_regular_from(info: &mut PathInfo, replacement: &[u8]) -> Result<(), Error> {
    replace_regular_from_with(info, replacement, MUTATION_CLEANUP_TIMEOUT, |_, _| Ok(()))
}

#[cfg(test)]
pub(super) fn replace_regular_from_at_checkpoint(
    info: &mut PathInfo,
    replacement: &[u8],
    mut hook: impl FnMut(MutationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    replace_regular_from_with(info, replacement, MUTATION_CLEANUP_TIMEOUT, |checkpoint, path| {
        hook(checkpoint, path)
    })
}

#[cfg(test)]
fn replace_regular_from_with_cleanup_timeout(
    info: &mut PathInfo,
    replacement: &[u8],
    cleanup_timeout: Duration,
    mut hook: impl FnMut(MutationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    replace_regular_from_with(info, replacement, cleanup_timeout, |checkpoint, path| {
        hook(checkpoint, path)
    })
}

fn replace_regular_from_with(
    info: &mut PathInfo,
    replacement: &[u8],
    cleanup_timeout: Duration,
    mut hook: impl FnMut(MutationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    let (layout_hash, target) = match &info.layout.file {
        StonePayloadLayoutFile::Regular(hash, target) => (*hash, target.clone()),
        _ => {
            return Err(Error::UnverifiedContent {
                path: info.path.clone(),
            });
        }
    };
    let verified = info.verified.as_mut().ok_or_else(|| Error::UnverifiedContent {
        path: info.path.clone(),
    })?;
    let path = verified.display_path()?;
    let expected_hash = match verified.kind {
        VerifiedKind::Regular { hash } => hash,
        _ => return Err(Error::UnverifiedContent { path }),
    };
    if info.layout.uid != verified.snapshot.uid
        || info.layout.gid != verified.snapshot.gid
        || info.layout.mode != verified.snapshot.mode
        || info.size != verified.snapshot.size
        || layout_hash != expected_hash
    {
        return Err(changed(
            &path,
            "regular-file replacement received stale collected metadata",
        ));
    }
    if verified.snapshot.links != 1 {
        return Err(changed(
            &path,
            "collector-owned replacement requires a single-link regular file",
        ));
    }

    let parent = verified.open_parent("open regular-file replacement parent")?;
    let original = open_original(verified, &parent, &path)?;
    let witness = std::sync::Arc::clone(&verified.witness);
    let mut transition = ReplacementWitness::begin(&witness, verified, &parent, expected_hash, cleanup_timeout, &path)?;
    let replacement_size = u64::try_from(replacement.len()).map_err(|_| Error::ArithmeticOverflow {
        resource: "regular file bytes",
        path: path.clone(),
    })?;
    transition.projected_regular_bytes(replacement_size, &path)?;

    let mut staged = match open_private_stage(&parent, &path) {
        Ok(staged) => staged,
        Err(primary) => {
            return Err(rollback_private_parent(
                &mut transition,
                verified,
                &parent,
                &original,
                primary,
                &path,
            ));
        }
    };
    let staged_content = match write_private_stage(&mut staged, replacement, verified.snapshot, &transition, &path) {
        Ok(staged) => staged,
        Err(primary) => {
            return Err(rollback_anonymous_stage(
                &mut transition,
                verified,
                &parent,
                &original,
                staged,
                primary,
                &path,
            ));
        }
    };
    if let Err(primary) = hook(MutationCheckpoint::BeforeStageLink, &path) {
        return Err(rollback_anonymous_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            staged,
            primary,
            &path,
        ));
    }
    if let Err(primary) =
        transition.require_original(&parent, &original, &verified.name, verified.snapshot, None, &path)
    {
        return Err(rollback_anonymous_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            staged,
            primary,
            &path,
        ));
    }

    let stage_name = match link_private_stage(&staged, &parent, &verified.deadline, &path) {
        Ok(name) => name,
        Err(primary) => {
            return Err(rollback_anonymous_stage(
                &mut transition,
                verified,
                &parent,
                &original,
                staged,
                primary,
                &path,
            ));
        }
    };
    let linked_stage = match require_named_regular(
        &parent,
        &stage_name,
        staged_content.snapshot,
        1,
        "authenticate linked regular-file replacement",
        &path,
    ) {
        Ok((_, snapshot)) => snapshot,
        Err(primary) => {
            return Err(rollback_linked_stage(
                &mut transition,
                verified,
                &parent,
                &original,
                &stage_name,
                staged_content.snapshot,
                primary,
                &path,
            ));
        }
    };
    if let Err(primary) = transition.require_membership(&parent, Some(&stage_name), &path) {
        return Err(rollback_linked_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            &stage_name,
            linked_stage,
            primary,
            &path,
        ));
    }

    if let Err(primary) = hook(MutationCheckpoint::BeforeExchange, &path)
        .and_then(|()| {
            transition.require_original(
                &parent,
                &original,
                &verified.name,
                verified.snapshot,
                Some(&stage_name),
                &path,
            )
        })
        .and_then(|()| {
            require_named_regular(
                &parent,
                &stage_name,
                linked_stage,
                1,
                "reauthenticate linked regular-file replacement",
                &path,
            )
            .map(|_| ())
        })
        .and_then(|()| {
            transition
                .require_membership(&parent, Some(&stage_name), &path)
                .map(|_| ())
        })
    {
        return Err(rollback_linked_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            &stage_name,
            linked_stage,
            primary,
            &path,
        ));
    }

    if let Err(primary) = rename_exchange(&parent, &stage_name, &verified.name, &path) {
        return Err(rollback_linked_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            &stage_name,
            linked_stage,
            primary,
            &path,
        ));
    }

    let verified_replacement = (|| {
        hook(MutationCheckpoint::AfterExchange, &path)?;
        let (mut replacement_file, replacement_snapshot) = require_named_regular(
            &parent,
            &verified.name,
            linked_stage,
            1,
            "authenticate exchanged regular-file replacement",
            &path,
        )?;
        let (_, retired_snapshot) = require_named_regular(
            &parent,
            &stage_name,
            verified.snapshot,
            1,
            "authenticate exchanged original regular file",
            &path,
        )?;
        require_regular_lineage(
            &metadata(&original, "reinspect retained original regular file", &path)?,
            verified.snapshot,
            1,
            &path,
            "retained original regular file changed during exchange",
        )?;
        transition.require_membership(&parent, Some(&stage_name), &path)?;
        let actual_hash = hash_open_regular(
            &mut replacement_file,
            replacement_snapshot,
            Some(&verified.deadline),
            &path,
        )?;
        if actual_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: actual_hash,
            });
        }
        staged.sync_all().map_err(|source| Error::Io {
            operation: "sync exchanged regular-file replacement",
            path: path.clone(),
            source,
        })?;
        sync_parent(&parent, "sync replacement exchange", &path)?;
        verified.deadline.check(&path)?;
        hook(MutationCheckpoint::BeforeFinalization, &path)?;
        let (mut replacement_file, replacement_snapshot) = require_named_regular(
            &parent,
            &verified.name,
            replacement_snapshot,
            1,
            "reauthenticate replacement before finalization",
            &path,
        )?;
        let final_hash = hash_open_regular(
            &mut replacement_file,
            replacement_snapshot,
            Some(&verified.deadline),
            &path,
        )?;
        if final_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: final_hash,
            });
        }
        let (_, retired_snapshot) = require_named_regular(
            &parent,
            &stage_name,
            retired_snapshot,
            1,
            "reauthenticate original before replacement finalization",
            &path,
        )?;
        require_exact_snapshot(
            &path,
            retired_snapshot,
            &metadata(&original, "reinspect retained original before finalization", &path)?,
            "retained original descriptor changed before finalization",
        )?;
        transition.require_membership(&parent, Some(&stage_name), &path)?;
        let projected_regular_bytes = transition.projected_regular_bytes(replacement_snapshot.size, &path)?;
        Ok((replacement_snapshot, retired_snapshot, projected_regular_bytes))
    })();

    let (replacement_snapshot, retired_snapshot, projected_regular_bytes) = match verified_replacement {
        Ok(snapshots) => snapshots,
        Err(primary) => {
            return Err(rollback_exchange(
                &mut transition,
                verified,
                &parent,
                &original,
                &stage_name,
                linked_stage,
                primary,
                &path,
            ));
        }
    };

    let retirement = (|| {
        hook(MutationCheckpoint::BeforeRetiredUnlink, &path)?;
        transition.require_anchored_parent(&parent, &verified.deadline, &path)?;
        let (mut current_replacement, current_replacement_snapshot) = require_named_regular(
            &parent,
            &verified.name,
            replacement_snapshot,
            1,
            "reauthenticate replacement before retiring original",
            &path,
        )?;
        if current_replacement_snapshot != replacement_snapshot {
            return Err(changed(
                &path,
                "regular-file replacement changed immediately before retiring original",
            ));
        }
        let current_hash = hash_open_regular(
            &mut current_replacement,
            current_replacement_snapshot,
            Some(&verified.deadline),
            &path,
        )?;
        if current_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: current_hash,
            });
        }
        let (_, current_retired_snapshot) = require_named_regular(
            &parent,
            &stage_name,
            retired_snapshot,
            1,
            "reauthenticate original immediately before retirement",
            &path,
        )?;
        if current_retired_snapshot != retired_snapshot {
            return Err(changed(
                &path,
                "retained original changed immediately before retirement",
            ));
        }
        require_exact_snapshot(
            &path,
            current_retired_snapshot,
            &metadata(
                &original,
                "reinspect retained original immediately before retirement",
                &path,
            )?,
            "retained original descriptor changed immediately before retirement",
        )?;
        transition.require_membership(&parent, Some(&stage_name), &path)?;
        unlink_owned(
            &parent,
            &stage_name,
            current_retired_snapshot.node,
            "retire original regular file after replacement",
            &path,
        )
    })();
    if let Err(primary) = retirement {
        return Err(rollback_exchange(
            &mut transition,
            verified,
            &parent,
            &original,
            &stage_name,
            linked_stage,
            primary,
            &path,
        ));
    }

    let finalized = (|| {
        hook(MutationCheckpoint::AfterRetiredUnlink, &path)?;
        sync_parent(&parent, "sync finalized regular-file replacement", &path)?;
        verified.deadline.check(&path)?;
        let retired = metadata(&original, "verify retired original regular file", &path)?;
        require_regular_lineage(
            &retired,
            verified.snapshot,
            0,
            &path,
            "retired original regular file did not lose its final link",
        )?;
        let (mut final_file, final_snapshot) = require_named_regular(
            &parent,
            &verified.name,
            replacement_snapshot,
            1,
            "verify finalized regular-file replacement",
            &path,
        )?;
        let final_hash = hash_open_regular(&mut final_file, final_snapshot, Some(&verified.deadline), &path)?;
        if final_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: final_hash,
            });
        }
        hook(MutationCheckpoint::BeforeWitnessCommit, &path)?;
        let (mut committed_file, committed_snapshot) = require_named_regular(
            &parent,
            &verified.name,
            final_snapshot,
            1,
            "reauthenticate replacement immediately before witness commit",
            &path,
        )?;
        if committed_snapshot != final_snapshot {
            return Err(changed(
                &path,
                "regular-file replacement changed immediately before witness commit",
            ));
        }
        let committed_hash =
            hash_open_regular(&mut committed_file, committed_snapshot, Some(&verified.deadline), &path)?;
        if committed_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: committed_hash,
            });
        }
        require_regular_lineage(
            &metadata(&original, "reinspect retired original before witness commit", &path)?,
            verified.snapshot,
            0,
            &path,
            "retired original regained a link before witness commit",
        )?;
        let parent_snapshot = transition.require_membership(&parent, None, &path)?;
        let anchored_parent = transition.require_anchored_parent(&parent, &verified.deadline, &path)?;
        if anchored_parent != parent_snapshot {
            return Err(changed(
                &path,
                "regular-file replacement parent changed between membership and anchored verification",
            ));
        }
        Ok((final_snapshot, parent_snapshot))
    })();

    let (final_snapshot, parent_snapshot) = match finalized {
        Ok(snapshots) => snapshots,
        Err(primary) => {
            transition.poison();
            return Err(commit_ambiguous(&path, primary));
        }
    };

    transition.commit_replacement(
        final_snapshot,
        parent_snapshot,
        staged_content.hash,
        projected_regular_bytes,
    );
    let new_layout = StonePayloadLayoutRecord {
        uid: final_snapshot.uid,
        gid: final_snapshot.gid,
        mode: final_snapshot.mode,
        tag: info.layout.tag,
        file: StonePayloadLayoutFile::Regular(staged_content.hash, target),
    };
    info.layout = new_layout;
    info.size = final_snapshot.size;
    verified.snapshot = final_snapshot;
    verified.kind = VerifiedKind::Regular {
        hash: staged_content.hash,
    };
    Ok(())
}

struct StagedContent {
    snapshot: FileSnapshot,
    hash: u128,
}

struct ReplacementWitness<'a> {
    state: MutexGuard<'a, WitnessState>,
    usage: MutexGuard<'a, CollectionUsage>,
    anchor: &'a super::RootAnchor,
    limits: super::CollectionLimits,
    deadline: &'a Deadline,
    parent: DirectoryId,
    position: usize,
    expected_parent: FileSnapshot,
    expected_file: FileSnapshot,
    expected_hash: u128,
    base_regular_bytes: u64,
    cleanup_timeout: Duration,
}

impl<'a> ReplacementWitness<'a> {
    fn begin(
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

    fn cleanup_deadline(&self) -> Deadline {
        Deadline::new(self.cleanup_timeout)
    }

    fn projected_regular_bytes(&self, replacement_bytes: u64, path: &Path) -> Result<u64, Error> {
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

    fn require_original(
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

    fn require_membership(&self, parent: &File, temporary: Option<&OsStr>, path: &Path) -> Result<FileSnapshot, Error> {
        self.require_membership_until(parent, temporary, self.deadline, path)
    }

    fn require_membership_until(
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
    fn require_anchored_parent(&self, parent: &File, deadline: &Deadline, path: &Path) -> Result<FileSnapshot, Error> {
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

    fn commit_replacement(
        &mut self,
        replacement: FileSnapshot,
        parent: FileSnapshot,
        hash: u128,
        projected_regular_bytes: u64,
    ) {
        self.state.directories[self.parent].snapshot = parent;
        self.state.directories[self.parent].children[self.position].kind =
            WitnessChildKind::Entry(super::EntryWitness {
                snapshot: replacement,
                kind: WitnessEntryKind::Regular { hash },
            });
        self.usage.regular_bytes = projected_regular_bytes;
    }

    fn commit_rollback(&mut self, original: FileSnapshot, parent: FileSnapshot) {
        self.state.directories[self.parent].snapshot = parent;
        self.state.directories[self.parent].children[self.position].kind =
            WitnessChildKind::Entry(super::EntryWitness {
                snapshot: original,
                kind: WitnessEntryKind::Regular {
                    hash: self.expected_hash,
                },
            });
    }

    fn poison(&mut self) {
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

fn open_original(verified: &VerifiedPath, parent: &File, path: &Path) -> Result<File, Error> {
    let original = open_entry(
        parent,
        &verified.name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        path,
    )?;
    let original_metadata = metadata(&original, "authenticate original regular file", path)?;
    if !original_metadata.file_type().is_file() {
        return Err(changed(path, "regular-file replacement target changed type"));
    }
    require_exact_snapshot(
        path,
        verified.snapshot,
        &original_metadata,
        "original regular file changed before replacement",
    )?;
    let handle = open_entry_handle(parent, &verified.name, path)?;
    require_exact_snapshot(
        path,
        verified.snapshot,
        &metadata(&handle, "authenticate named original regular file", path)?,
        "named original regular file changed before replacement",
    )?;
    Ok(original)
}

fn open_private_stage(parent: &File, path: &Path) -> Result<File, Error> {
    // `O_TMPFILE` keeps incomplete bytes unreachable. Deliberately do not fall
    // back to a named O_EXCL file: that would expose partial analyzer output in
    // the witnessed tree and would not be an equally strong primitive.
    // SAFETY: parent and the static directory component remain live; a
    // successful openat returns a fresh descriptor owned below.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor == -1 {
        return Err(Error::Io {
            operation: "create private regular-file replacement",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

fn write_private_stage(
    staged: &mut File,
    replacement: &[u8],
    original: FileSnapshot,
    transition: &ReplacementWitness<'_>,
    path: &Path,
) -> Result<StagedContent, Error> {
    let initial = metadata(staged, "inspect private regular-file replacement", path)?;
    if !initial.file_type().is_file() || initial.nlink() != 0 {
        return Err(changed(
            path,
            "private regular-file replacement is not an anonymous regular inode",
        ));
    }
    let staged_node = NodeIdentity::from_metadata(&initial);
    if staged_node == original.node {
        return Err(changed(
            path,
            "private replacement unexpectedly reused the original inode",
        ));
    }

    let mut hasher = StoneDigestWriterHasher::new();
    let bytes = u64::try_from(replacement.len()).map_err(|_| Error::ArithmeticOverflow {
        resource: "regular file bytes",
        path: path.to_owned(),
    })?;
    transition.projected_regular_bytes(bytes, path)?;
    for chunk in replacement.chunks(HASH_BUFFER_BYTES) {
        transition.deadline.check(path)?;
        staged.write_all(chunk).map_err(|source| Error::Io {
            operation: "write private regular-file replacement",
            path: path.to_owned(),
            source,
        })?;
        hasher.update(chunk);
    }
    transition.deadline.check(path)?;

    if initial.uid() != original.uid || initial.gid() != original.gid {
        // SAFETY: staged is the still-anonymous inode owned by this transaction.
        if unsafe { libc::fchown(staged.as_raw_fd(), original.uid, original.gid) } == -1 {
            return Err(Error::Io {
                operation: "preserve regular-file replacement ownership",
                path: path.to_owned(),
                source: io::Error::last_os_error(),
            });
        }
    }
    // Ownership is normalized before mode because chown may clear set-ID bits.
    // SAFETY: staged is a live descriptor for the private inode.
    if unsafe { libc::fchmod(staged.as_raw_fd(), original.mode & 0o7777) } == -1 {
        return Err(Error::Io {
            operation: "preserve regular-file replacement mode",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    staged.sync_all().map_err(|source| Error::Io {
        operation: "sync private regular-file replacement",
        path: path.to_owned(),
        source,
    })?;
    transition.deadline.check(path)?;
    let normalized = metadata(staged, "verify private regular-file replacement", path)?;
    if !normalized.file_type().is_file()
        || normalized.nlink() != 0
        || NodeIdentity::from_metadata(&normalized) != staged_node
        || normalized.len() != bytes
        || normalized.uid() != original.uid
        || normalized.gid() != original.gid
        || normalized.mode() != original.mode
    {
        return Err(changed(
            path,
            "private regular-file replacement metadata did not normalize exactly",
        ));
    }
    Ok(StagedContent {
        snapshot: FileSnapshot::from_metadata(&normalized),
        hash: hasher.digest128(),
    })
}

fn link_private_stage(staged: &File, parent: &File, deadline: &Deadline, path: &Path) -> Result<OsString, Error> {
    let process = unsafe { libc::getpid() } as u32;
    let first = NEXT_STAGE_NAME.fetch_add(STAGE_NAME_ATTEMPTS, Ordering::Relaxed);
    let mut last_collision = None;
    for attempt in 0..STAGE_NAME_ATTEMPTS {
        deadline.check(path)?;
        let sequence = first.wrapping_add(attempt);
        let name = OsString::from(format!("{STAGE_NAME_PREFIX}{process:08x}-{sequence:016x}"));
        let c_name = c_name(&name, path)?;
        // Linking the anonymous inode is the first point at which it becomes
        // visible. `AT_EMPTY_PATH` names the exact retained descriptor; the
        // target is rooted in the authenticated parent descriptor.
        // SAFETY: all descriptors and C strings remain live for linkat.
        if unsafe {
            libc::linkat(
                staged.as_raw_fd(),
                c"".as_ptr(),
                parent.as_raw_fd(),
                c_name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } == 0
        {
            return Ok(name);
        }
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::AlreadyExists {
            last_collision = Some(source);
            continue;
        }
        return Err(Error::Io {
            operation: "link complete private regular-file replacement",
            path: path.to_owned(),
            source,
        });
    }
    Err(Error::Io {
        operation: "allocate private regular-file replacement name",
        path: path.to_owned(),
        source: last_collision.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)),
    })
}

fn rename_exchange(parent: &File, temporary: &OsStr, target: &OsStr, path: &Path) -> Result<(), Error> {
    let temporary = c_name(temporary, path)?;
    let target = c_name(target, path)?;
    // SAFETY: parent and both names remain live. RENAME_EXCHANGE either swaps
    // the two existing names atomically or changes neither name.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            temporary.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == -1 {
        Err(Error::Io {
            operation: "atomically exchange regular-file replacement",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn require_named_regular(
    parent: &File,
    name: &OsStr,
    lineage: FileSnapshot,
    links: u64,
    operation: &'static str,
    path: &Path,
) -> Result<(File, FileSnapshot), Error> {
    let file = open_entry(
        parent,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        path,
    )?;
    let current = metadata(&file, operation, path)?;
    require_regular_lineage(&current, lineage, links, path, "regular-file replacement inode changed")?;
    let handle = open_entry_handle(parent, name, path)?;
    let named = metadata(&handle, "reauthenticate named regular-file replacement", path)?;
    let snapshot = FileSnapshot::from_metadata(&current);
    require_exact_snapshot(
        path,
        snapshot,
        &named,
        "named regular-file replacement changed between descriptor opens",
    )?;
    Ok((file, snapshot))
}

fn require_regular_lineage(
    metadata: &std::fs::Metadata,
    lineage: FileSnapshot,
    links: u64,
    path: &Path,
    detail: &'static str,
) -> Result<(), Error> {
    let actual = FileSnapshot::from_metadata(metadata);
    if metadata.file_type().is_file()
        && actual.node == lineage.node
        && actual.size == lineage.size
        && actual.mode == lineage.mode
        && actual.uid == lineage.uid
        && actual.gid == lineage.gid
        && actual.links == links
    {
        Ok(())
    } else {
        Err(changed(path, detail))
    }
}

fn require_exact_snapshot(
    path: &Path,
    expected: FileSnapshot,
    metadata: &std::fs::Metadata,
    detail: &'static str,
) -> Result<(), Error> {
    if FileSnapshot::from_metadata(metadata) == expected {
        Ok(())
    } else {
        Err(changed(path, detail))
    }
}

fn hash_open_regular(
    file: &mut File,
    expected: FileSnapshot,
    deadline: Option<&Deadline>,
    path: &Path,
) -> Result<u128, Error> {
    require_exact_snapshot(
        path,
        expected,
        &metadata(file, "inspect regular-file replacement before hashing", path)?,
        "regular-file replacement changed before hashing",
    )?;
    let mut hasher = StoneDigestWriterHasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    loop {
        if let Some(deadline) = deadline {
            deadline.check(path)?;
        }
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify regular-file replacement bytes",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if read == 0 {
            break;
        }
        bytes = bytes.checked_add(read as u64).ok_or(Error::ArithmeticOverflow {
            resource: "regular file bytes",
            path: path.to_owned(),
        })?;
        if bytes > expected.size {
            return Err(changed(path, "regular-file replacement grew while being verified"));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected.size {
        return Err(changed(
            path,
            "regular-file replacement changed size while being verified",
        ));
    }
    require_exact_snapshot(
        path,
        expected,
        &metadata(file, "reinspect hashed regular-file replacement", path)?,
        "regular-file replacement metadata changed while hashing",
    )?;
    if let Some(deadline) = deadline {
        deadline.check(path)?;
    }
    Ok(hasher.digest128())
}

fn sync_parent(parent: &File, operation: &'static str, path: &Path) -> Result<(), Error> {
    parent.sync_all().map_err(|source| Error::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

// Linux does not expose a conditional unlink which accepts an expected inode.
// This transaction runs under Mason's derivation execution lock after analyzer
// children are reaped; the identity check therefore protects against stale
// names inside the collector's single-mutator boundary. Do not reuse this in a
// directory writable by a concurrent same-UID mutator.
fn unlink_owned(
    parent: &File,
    name: &OsStr,
    identity: NodeIdentity,
    operation: &'static str,
    path: &Path,
) -> Result<(), Error> {
    let handle = open_entry_handle(parent, name, path)?;
    let current = metadata(&handle, "authenticate regular-file replacement cleanup", path)?;
    if current.file_type().is_dir() || NodeIdentity::from_metadata(&current) != identity {
        return Err(changed(path, "regular-file replacement cleanup name changed ownership"));
    }
    let name = c_name(name, path)?;
    // SAFETY: the authenticated parent and single-component name remain live;
    // unlinkat does not follow the final component.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
        return Err(Error::Io {
            operation,
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn require_exact_membership(
    parent: &File,
    expected: &[WitnessChild],
    temporary: Option<&OsStr>,
    deadline: &Deadline,
    path: &Path,
) -> Result<FileSnapshot, Error> {
    deadline.check(path)?;
    let before = FileSnapshot::from_metadata(&metadata(
        parent,
        "inspect regular-file replacement parent membership",
        path,
    )?);
    let cursor = open_entry(
        parent,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        path,
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: descriptor is a fresh owned directory descriptor. fdopendir
    // consumes it on success; it remains ours on failure.
    let stream = unsafe { libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed without consuming descriptor.
        unsafe { libc::close(descriptor) };
        return Err(Error::Io {
            operation: "enumerate regular-file replacement parent",
            path: path.to_owned(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut found = 0usize;
    let mut temporary_found = false;
    loop {
        deadline.check(path)?;
        // SAFETY: errno is thread-local on Linux.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: stream is live and exclusively owned by this loop.
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::Io {
                operation: "enumerate regular-file replacement parent",
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let name = OsStr::from_bytes(name);
        if temporary.is_some_and(|temporary| temporary == name) {
            if temporary_found {
                return Err(changed(path, "regular-file replacement temporary name was duplicated"));
            }
            temporary_found = true;
        } else if expected
            .binary_search_by(|child| child.name.as_os_str().cmp(name))
            .is_ok()
        {
            found = found.checked_add(1).ok_or(Error::ArithmeticOverflow {
                resource: "regular-file replacement parent entries",
                path: path.to_owned(),
            })?;
        } else {
            return Err(changed(path, "regular-file replacement parent membership changed"));
        }
    }
    drop(stream);
    if found != expected.len() || temporary.is_some() != temporary_found {
        return Err(changed(path, "regular-file replacement parent membership changed"));
    }
    let after = FileSnapshot::from_metadata(&metadata(
        parent,
        "reinspect regular-file replacement parent membership",
        path,
    )?);
    if before != after {
        return Err(changed(
            path,
            "regular-file replacement parent changed during enumeration",
        ));
    }
    deadline.check(path)?;
    Ok(after)
}

struct DirectoryStream(NonNull<libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the fdopendir stream.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_anonymous_stage(
    transition: &mut ReplacementWitness<'_>,
    verified: &mut VerifiedPath,
    parent: &File,
    original: &File,
    staged: File,
    primary: Error,
    path: &Path,
) -> Error {
    // Closing the final descriptor removes the anonymous inode before the
    // parent witness is refreshed. Some filesystems update directory ctime for
    // O_TMPFILE creation even though membership never changes.
    drop(staged);
    rollback_private_parent(transition, verified, parent, original, primary, path)
}

fn rollback_private_parent(
    transition: &mut ReplacementWitness<'_>,
    verified: &mut VerifiedPath,
    parent: &File,
    original: &File,
    primary: Error,
    path: &Path,
) -> Error {
    let cleanup_deadline = transition.cleanup_deadline();
    let cleanup = (|| {
        cleanup_deadline.check(path)?;
        sync_parent(parent, "sync anonymous regular-file replacement cleanup", path)?;
        cleanup_deadline.check(path)?;
        let (mut named_original, original_snapshot) = require_named_regular(
            parent,
            &verified.name,
            transition.expected_file,
            1,
            "verify original after anonymous replacement cleanup",
            path,
        )?;
        require_exact_snapshot(
            path,
            original_snapshot,
            &metadata(original, "verify retained original after anonymous cleanup", path)?,
            "retained original changed during anonymous-stage cleanup",
        )?;
        let original_hash = hash_open_regular(&mut named_original, original_snapshot, Some(&cleanup_deadline), path)?;
        if original_hash != transition.expected_hash {
            return Err(Error::ContentHashChanged {
                path: path.to_owned(),
                expected: transition.expected_hash,
                actual: original_hash,
            });
        }
        finish_proven_rollback(transition, verified, parent, original_snapshot, &cleanup_deadline, path)
    })();
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            transition.poison();
            Error::MutationRollback {
                path: path.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_linked_stage(
    transition: &mut ReplacementWitness<'_>,
    verified: &mut VerifiedPath,
    parent: &File,
    original: &File,
    stage_name: &OsStr,
    staged: FileSnapshot,
    primary: Error,
    path: &Path,
) -> Error {
    let cleanup_deadline = transition.cleanup_deadline();
    let cleanup = (|| {
        cleanup_deadline.check(path)?;
        let (_, linked) = require_named_regular(
            parent,
            stage_name,
            staged,
            1,
            "authenticate unexchanged regular-file replacement cleanup",
            path,
        )?;
        cleanup_deadline.check(path)?;
        unlink_owned(
            parent,
            stage_name,
            linked.node,
            "remove unexchanged regular-file replacement",
            path,
        )?;
        cleanup_deadline.check(path)?;
        sync_parent(parent, "sync unexchanged regular-file replacement cleanup", path)?;
        cleanup_deadline.check(path)?;
        let (mut named_original, original_snapshot) = require_named_regular(
            parent,
            &verified.name,
            transition.expected_file,
            1,
            "verify original after unexchanged replacement cleanup",
            path,
        )?;
        require_exact_snapshot(
            path,
            original_snapshot,
            &metadata(original, "verify retained original after replacement cleanup", path)?,
            "retained original changed during linked-stage cleanup",
        )?;
        let original_hash = hash_open_regular(&mut named_original, original_snapshot, Some(&cleanup_deadline), path)?;
        if original_hash != transition.expected_hash {
            return Err(Error::ContentHashChanged {
                path: path.to_owned(),
                expected: transition.expected_hash,
                actual: original_hash,
            });
        }
        finish_proven_rollback(transition, verified, parent, original_snapshot, &cleanup_deadline, path)
    })();
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            transition.poison();
            Error::MutationRollback {
                path: path.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_exchange(
    transition: &mut ReplacementWitness<'_>,
    verified: &mut VerifiedPath,
    parent: &File,
    original: &File,
    stage_name: &OsStr,
    staged: FileSnapshot,
    primary: Error,
    path: &Path,
) -> Error {
    let cleanup_deadline = transition.cleanup_deadline();
    let cleanup = (|| {
        cleanup_deadline.check(path)?;
        require_named_regular(
            parent,
            &verified.name,
            staged,
            1,
            "authenticate published replacement before rollback",
            path,
        )?;
        require_named_regular(
            parent,
            stage_name,
            transition.expected_file,
            1,
            "authenticate retained original before rollback",
            path,
        )?;
        transition.require_membership_until(parent, Some(stage_name), &cleanup_deadline, path)?;
        cleanup_deadline.check(path)?;
        rename_exchange(parent, stage_name, &verified.name, path)?;
        cleanup_deadline.check(path)?;

        let (mut named_original, original_snapshot) = require_named_regular(
            parent,
            &verified.name,
            transition.expected_file,
            1,
            "verify restored original regular file",
            path,
        )?;
        require_exact_snapshot(
            path,
            original_snapshot,
            &metadata(original, "verify retained restored original regular file", path)?,
            "retained original changed during exchange rollback",
        )?;
        let (_, rolled_back_stage) = require_named_regular(
            parent,
            stage_name,
            staged,
            1,
            "authenticate rolled-back regular-file replacement",
            path,
        )?;
        cleanup_deadline.check(path)?;
        unlink_owned(
            parent,
            stage_name,
            rolled_back_stage.node,
            "remove rolled-back regular-file replacement",
            path,
        )?;
        cleanup_deadline.check(path)?;
        sync_parent(parent, "sync regular-file replacement rollback", path)?;
        cleanup_deadline.check(path)?;
        let original_hash = hash_open_regular(&mut named_original, original_snapshot, Some(&cleanup_deadline), path)?;
        if original_hash != transition.expected_hash {
            return Err(Error::ContentHashChanged {
                path: path.to_owned(),
                expected: transition.expected_hash,
                actual: original_hash,
            });
        }
        finish_proven_rollback(transition, verified, parent, original_snapshot, &cleanup_deadline, path)
    })();
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            transition.poison();
            Error::MutationRollback {
                path: path.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }
        }
    }
}

fn finish_proven_rollback(
    transition: &mut ReplacementWitness<'_>,
    verified: &mut VerifiedPath,
    parent: &File,
    original_snapshot: FileSnapshot,
    cleanup_deadline: &Deadline,
    path: &Path,
) -> Result<(), Error> {
    let parent_snapshot = transition.require_membership_until(parent, None, cleanup_deadline, path)?;
    let anchored_parent = transition.require_anchored_parent(parent, cleanup_deadline, path)?;
    if anchored_parent != parent_snapshot {
        return Err(changed(
            path,
            "regular-file replacement rollback parent changed before witness restoration",
        ));
    }
    cleanup_deadline.check(path)?;
    transition.commit_rollback(original_snapshot, parent_snapshot);
    verified.snapshot = original_snapshot;
    Ok(())
}

fn commit_ambiguous(path: &Path, primary: Error) -> Error {
    Error::MutationCommitAmbiguous {
        path: path.to_owned(),
        primary: Box::new(primary),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs::Permissions,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    };

    use fs_err as fs;
    use stone::StoneDigestWriterHasher;
    use stone_recipe::derivation::PathRuleKind;

    use super::*;
    use crate::package::collect::{CollectionLimits, Collector};

    fn write_file(root: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
        path
    }

    fn collect(root: &Path, path: &Path, limits: CollectionLimits) -> (Collector, PathInfo) {
        let mut collector = Collector::new_with_limits(root, limits);
        collector.add_rule("*", "out", PathRuleKind::Any).unwrap();
        let info = collector.path(path, &mut StoneDigestWriterHasher::new()).unwrap();
        (collector, info)
    }

    fn temporary_names(root: &Path) -> Vec<OsString> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.as_bytes().starts_with(STAGE_NAME_PREFIX.as_bytes()))
            .collect()
    }

    fn injected(path: &Path) -> Error {
        Error::TreeChanged {
            path: path.to_owned(),
            detail: "injected regular-file replacement failure",
        }
    }

    #[test]
    fn replacement_accepts_exact_file_and_aggregate_limits_and_rejects_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"payload", 0o751);
        write_file(root.path(), "other", b"payload", 0o644);
        let limits = CollectionLimits {
            max_file_bytes: 8,
            max_total_regular_bytes: 15,
            ..CollectionLimits::default()
        };
        let (collector, mut info) = collect(root.path(), &path, limits);
        info.replace_regular_from(b"12345678").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"12345678");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o751);
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"payload", 0o644);
        write_file(root.path(), "other", b"payload", 0o644);
        let limits = CollectionLimits {
            max_file_bytes: 9,
            max_total_regular_bytes: 15,
            ..CollectionLimits::default()
        };
        let (collector, mut info) = collect(root.path(), &path, limits);
        assert!(matches!(
            info.replace_regular_from(b"123456789"),
            Err(Error::LimitExceeded {
                resource: "total regular file bytes",
                limit: 15,
                actual: 16,
                ..
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"payload");
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();

        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"payload", 0o644);
        let limits = CollectionLimits {
            max_file_bytes: 8,
            max_total_regular_bytes: 64,
            ..CollectionLimits::default()
        };
        let (collector, mut info) = collect(root.path(), &path, limits);
        assert!(matches!(
            info.replace_regular_from(b"123456789"),
            Err(Error::LimitExceeded {
                resource: "regular file bytes",
                limit: 8,
                actual: 9,
                ..
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"payload");
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();
    }

    #[test]
    fn replacement_publishes_exact_bytes_mode_and_new_witnessed_inode_without_leaks() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original bytes", 0o6751);
        let original = fs::metadata(&path).unwrap();
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        info.replace_regular_from(b"replacement bytes").unwrap();

        let replaced = fs::metadata(&path).unwrap();
        assert_ne!(replaced.ino(), original.ino());
        assert_eq!(replaced.nlink(), 1);
        assert_eq!(replaced.uid(), original.uid());
        assert_eq!(replaced.gid(), original.gid());
        assert_eq!(replaced.mode() & 0o7777, original.mode() & 0o7777);
        assert_eq!(fs::read(&path).unwrap(), b"replacement bytes");
        assert_eq!(info.size, b"replacement bytes".len() as u64);
        info.verify_unchanged().unwrap();
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();
    }

    #[test]
    fn replacement_rewitnesses_parent_directory_infos_for_later_emission() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        let path = write_file(root.path(), "usr/lib/file", b"original", 0o644);
        let mut collector = Collector::new(root.path());
        collector.add_rule("*", "out", PathRuleKind::Any).unwrap();
        let mut paths = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let file = paths.iter().position(|info| info.path == path).unwrap();

        paths[file].replace_regular_from(b"replacement").unwrap();

        for info in &paths {
            info.verify_unchanged().unwrap();
        }
        assert!(temporary_names(root.path().join("usr/lib").as_path()).is_empty());
        let sealed = collector.seal().unwrap();
        sealed.verify().unwrap();
        for info in &paths {
            info.verify_unchanged().unwrap();
        }
    }

    #[test]
    fn replacement_race_is_rejected_without_deleting_foreign_names_or_leaking_stage() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o644);
        let displaced = root.path().join("displaced");
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |checkpoint, target| {
            if checkpoint == MutationCheckpoint::BeforeExchange {
                fs::rename(target, &displaced).unwrap();
                fs::write(target, b"racer").unwrap();
            }
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"racer");
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
        assert!(temporary_names(root.path()).is_empty());
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn injected_pre_finalization_failure_exactly_rolls_back_exchange_and_witness() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o751);
        let original = fs::metadata(&path).unwrap();
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());
        let injected_failure = Cell::new(false);

        let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |checkpoint, path| {
            if checkpoint == MutationCheckpoint::BeforeFinalization {
                injected_failure.set(true);
                Err(injected(path))
            } else {
                Ok(())
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged { .. })));
        assert!(injected_failure.get());
        let restored = fs::metadata(&path).unwrap();
        assert_eq!(restored.ino(), original.ino());
        assert_eq!(restored.nlink(), 1);
        assert_eq!(restored.mode() & 0o7777, original.mode() & 0o7777);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        info.verify_unchanged().unwrap();
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();
    }

    #[test]
    fn injected_retired_unlink_failure_restores_original_and_removes_stage() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o751);
        let original = fs::metadata(&path).unwrap();
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |checkpoint, path| {
            if checkpoint == MutationCheckpoint::BeforeRetiredUnlink {
                Err(injected(path))
            } else {
                Ok(())
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged { .. })));
        let restored = fs::metadata(&path).unwrap();
        assert_eq!(restored.ino(), original.ino());
        assert_eq!(restored.nlink(), 1);
        assert_eq!(restored.mode() & 0o7777, original.mode() & 0o7777);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        info.verify_unchanged().unwrap();
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();
    }

    #[test]
    fn exhausted_fresh_cleanup_deadline_poisons_without_exposing_anonymous_stage() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o640);
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        let error =
            replace_regular_from_with_cleanup_timeout(&mut info, b"replacement", Duration::ZERO, |checkpoint, path| {
                if checkpoint == MutationCheckpoint::BeforeStageLink {
                    Err(injected(path))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        let Error::MutationRollback { cleanup, .. } = error else {
            panic!("expected a bounded cleanup failure, got {error:?}");
        };
        assert!(matches!(
            *cleanup,
            Error::DurationExceeded {
                limit: Duration::ZERO,
                ..
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(temporary_names(root.path()).is_empty());
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[derive(Debug, Clone, Copy)]
    enum AnchorReplacement {
        Root,
        Parent,
    }

    fn anchor_replacement_fails_closed(checkpoint: MutationCheckpoint, replacement: AnchorReplacement) {
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("root");
        let parent = root.join("usr/lib");
        fs::create_dir_all(&parent).unwrap();
        let path = write_file(&root, "usr/lib/file", b"original", 0o644);
        let original_inode = fs::metadata(&path).unwrap().ino();
        let (collector, mut info) = collect(&root, &path, CollectionLimits::default());
        let displaced = match replacement {
            AnchorReplacement::Root => sandbox.path().join("retained-root"),
            AnchorReplacement::Parent => root.join("usr/lib-retained"),
        };
        let retained_parent = match replacement {
            AnchorReplacement::Root => displaced.join("usr/lib"),
            AnchorReplacement::Parent => displaced.clone(),
        };
        let retained_path = retained_parent.join("file");
        let mut replaced = false;

        let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |current, _| {
            if current != checkpoint || replaced {
                return Ok(());
            }
            replaced = true;
            match replacement {
                AnchorReplacement::Root => fs::rename(&root, &displaced).unwrap(),
                AnchorReplacement::Parent => fs::rename(&parent, &displaced).unwrap(),
            }
            fs::create_dir_all(&parent).unwrap();
            fs::write(parent.join("file"), b"foreign").unwrap();
            Ok(())
        });

        assert!(replaced, "checkpoint {checkpoint:?} was not reached");
        assert!(result.is_err(), "{replacement:?} replacement unexpectedly committed");
        assert_eq!(fs::read(parent.join("file")).unwrap(), b"foreign");
        assert!(temporary_names(&retained_parent).is_empty());
        assert!(
            matches!(info.verify_unchanged(), Err(Error::InventoryPoisoned)),
            "checkpoint={checkpoint:?} replacement={replacement:?} result={result:?}"
        );
        assert!(collector.seal().is_err());
        match checkpoint {
            MutationCheckpoint::BeforeRetiredUnlink => {
                assert_eq!(fs::metadata(&retained_path).unwrap().ino(), original_inode);
                assert_eq!(fs::read(&retained_path).unwrap(), b"original");
                assert!(matches!(result, Err(Error::MutationRollback { .. })));
            }
            MutationCheckpoint::BeforeWitnessCommit => {
                assert_ne!(fs::metadata(&retained_path).unwrap().ino(), original_inode);
                assert_eq!(fs::read(&retained_path).unwrap(), b"replacement");
                assert!(matches!(result, Err(Error::MutationCommitAmbiguous { .. })));
            }
            _ => unreachable!("anchor replacement test uses a finalization checkpoint"),
        }
    }

    #[test]
    fn root_and_parent_replacement_fail_before_unlink_and_witness_commit() {
        for checkpoint in [
            MutationCheckpoint::BeforeRetiredUnlink,
            MutationCheckpoint::BeforeWitnessCommit,
        ] {
            for replacement in [AnchorReplacement::Root, AnchorReplacement::Parent] {
                anchor_replacement_fails_closed(checkpoint, replacement);
            }
        }
    }

    #[test]
    fn same_inode_mutation_is_rechecked_at_both_irreversible_boundaries() {
        for checkpoint in [
            MutationCheckpoint::BeforeRetiredUnlink,
            MutationCheckpoint::BeforeWitnessCommit,
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = write_file(root.path(), "file", b"original", 0o644);
            let original_inode = fs::metadata(&path).unwrap().ino();
            let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

            let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |current, target| {
                if current == checkpoint {
                    // Keep the staged length and inode unchanged so only the
                    // immediate snapshot/hash recheck can catch this race.
                    fs::write(target, b"XXXXXXXXXXX").unwrap();
                }
                Ok(())
            });

            assert!(temporary_names(root.path()).is_empty());
            match checkpoint {
                MutationCheckpoint::BeforeRetiredUnlink => {
                    assert!(matches!(result, Err(Error::TreeChanged { .. })));
                    assert_eq!(fs::metadata(&path).unwrap().ino(), original_inode);
                    assert_eq!(fs::read(&path).unwrap(), b"original");
                    info.verify_unchanged().unwrap();
                    collector.seal().unwrap();
                }
                MutationCheckpoint::BeforeWitnessCommit => {
                    assert!(matches!(result, Err(Error::MutationCommitAmbiguous { .. })));
                    assert_ne!(fs::metadata(&path).unwrap().ino(), original_inode);
                    assert_eq!(fs::read(&path).unwrap(), b"XXXXXXXXXXX");
                    assert!(matches!(info.verify_unchanged(), Err(Error::InventoryPoisoned)));
                    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
                }
                _ => unreachable!("irreversible-boundary test uses finalization checkpoints"),
            }
        }
    }

    #[test]
    fn post_commit_failure_poisoned_inventory_retains_published_file_without_temp_leak() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o640);
        let original_inode = fs::metadata(&path).unwrap().ino();
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        let result = replace_regular_from_at_checkpoint(&mut info, b"replacement", |checkpoint, path| {
            if checkpoint == MutationCheckpoint::AfterRetiredUnlink {
                Err(injected(path))
            } else {
                Ok(())
            }
        });

        assert!(matches!(result, Err(Error::MutationCommitAmbiguous { .. })));
        assert_ne!(fs::metadata(&path).unwrap().ino(), original_inode);
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert!(temporary_names(root.path()).is_empty());
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn multiply_linked_regular_file_is_rejected_before_staging() {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", b"original", 0o644);
        fs::hard_link(&path, root.path().join("alias")).unwrap();
        let (collector, mut info) = collect(root.path(), &path, CollectionLimits::default());

        assert!(matches!(
            info.replace_regular_from(b"replacement"),
            Err(Error::TreeChanged { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(temporary_names(root.path()).is_empty());
        collector.seal().unwrap();
    }
}
