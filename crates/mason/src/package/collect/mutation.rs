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
    let original = staging::open_original(verified, &parent, &path)?;
    let witness = std::sync::Arc::clone(&verified.witness);
    let mut transition =
        witness::ReplacementWitness::begin(&witness, verified, &parent, expected_hash, cleanup_timeout, &path)?;
    let replacement_size = u64::try_from(replacement.len()).map_err(|_| Error::ArithmeticOverflow {
        resource: "regular file bytes",
        path: path.clone(),
    })?;
    transition.projected_regular_bytes(replacement_size, &path)?;

    let mut staged = match staging::open_private_stage(&parent, &path) {
        Ok(staged) => staged,
        Err(primary) => {
            return Err(rollback::rollback_private_parent(
                &mut transition,
                verified,
                &parent,
                &original,
                primary,
                &path,
            ));
        }
    };
    let staged_content =
        match staging::write_private_stage(&mut staged, replacement, verified.snapshot, &transition, &path) {
            Ok(staged) => staged,
            Err(primary) => {
                return Err(rollback::rollback_anonymous_stage(
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
        return Err(rollback::rollback_anonymous_stage(
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
        return Err(rollback::rollback_anonymous_stage(
            &mut transition,
            verified,
            &parent,
            &original,
            staged,
            primary,
            &path,
        ));
    }

    let stage_name = match staging::link_private_stage(&staged, &parent, &verified.deadline, &path) {
        Ok(name) => name,
        Err(primary) => {
            return Err(rollback::rollback_anonymous_stage(
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
    let linked_stage = match verification::require_named_regular(
        &parent,
        &stage_name,
        staged_content.snapshot,
        1,
        "authenticate linked regular-file replacement",
        &path,
    ) {
        Ok((_, snapshot)) => snapshot,
        Err(primary) => {
            return Err(rollback::rollback_linked_stage(
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
        return Err(rollback::rollback_linked_stage(
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
            verification::require_named_regular(
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
        return Err(rollback::rollback_linked_stage(
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

    if let Err(primary) = staging::rename_exchange(&parent, &stage_name, &verified.name, &path) {
        return Err(rollback::rollback_linked_stage(
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
        let (mut replacement_file, replacement_snapshot) = verification::require_named_regular(
            &parent,
            &verified.name,
            linked_stage,
            1,
            "authenticate exchanged regular-file replacement",
            &path,
        )?;
        let (_, retired_snapshot) = verification::require_named_regular(
            &parent,
            &stage_name,
            verified.snapshot,
            1,
            "authenticate exchanged original regular file",
            &path,
        )?;
        verification::require_regular_lineage(
            &metadata(&original, "reinspect retained original regular file", &path)?,
            verified.snapshot,
            1,
            &path,
            "retained original regular file changed during exchange",
        )?;
        transition.require_membership(&parent, Some(&stage_name), &path)?;
        let actual_hash = verification::hash_open_regular(
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
        verification::sync_parent(&parent, "sync replacement exchange", &path)?;
        verified.deadline.check(&path)?;
        hook(MutationCheckpoint::BeforeFinalization, &path)?;
        let (mut replacement_file, replacement_snapshot) = verification::require_named_regular(
            &parent,
            &verified.name,
            replacement_snapshot,
            1,
            "reauthenticate replacement before finalization",
            &path,
        )?;
        let final_hash = verification::hash_open_regular(
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
        let (_, retired_snapshot) = verification::require_named_regular(
            &parent,
            &stage_name,
            retired_snapshot,
            1,
            "reauthenticate original before replacement finalization",
            &path,
        )?;
        verification::require_exact_snapshot(
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
            return Err(rollback::rollback_exchange(
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
        let (mut current_replacement, current_replacement_snapshot) = verification::require_named_regular(
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
        let current_hash = verification::hash_open_regular(
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
        let (_, current_retired_snapshot) = verification::require_named_regular(
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
        verification::require_exact_snapshot(
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
        verification::unlink_owned(
            &parent,
            &stage_name,
            current_retired_snapshot.node,
            "retire original regular file after replacement",
            &path,
        )
    })();
    if let Err(primary) = retirement {
        return Err(rollback::rollback_exchange(
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
        verification::sync_parent(&parent, "sync finalized regular-file replacement", &path)?;
        verified.deadline.check(&path)?;
        let retired = metadata(&original, "verify retired original regular file", &path)?;
        verification::require_regular_lineage(
            &retired,
            verified.snapshot,
            0,
            &path,
            "retired original regular file did not lose its final link",
        )?;
        let (mut final_file, final_snapshot) = verification::require_named_regular(
            &parent,
            &verified.name,
            replacement_snapshot,
            1,
            "verify finalized regular-file replacement",
            &path,
        )?;
        let final_hash =
            verification::hash_open_regular(&mut final_file, final_snapshot, Some(&verified.deadline), &path)?;
        if final_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: final_hash,
            });
        }
        hook(MutationCheckpoint::BeforeWitnessCommit, &path)?;
        let (mut committed_file, committed_snapshot) = verification::require_named_regular(
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
            verification::hash_open_regular(&mut committed_file, committed_snapshot, Some(&verified.deadline), &path)?;
        if committed_hash != staged_content.hash {
            return Err(Error::ContentHashChanged {
                path: path.clone(),
                expected: staged_content.hash,
                actual: committed_hash,
            });
        }
        verification::require_regular_lineage(
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
            return Err(rollback::commit_ambiguous(&path, primary));
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

mod rollback;
mod staging;
mod verification;
mod witness;

#[cfg(test)]
mod tests;
