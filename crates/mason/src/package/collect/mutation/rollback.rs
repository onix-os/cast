use super::{
    staging::rename_exchange,
    verification::{hash_open_regular, require_exact_snapshot, require_named_regular, sync_parent, unlink_owned},
    witness::ReplacementWitness,
    *,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn rollback_anonymous_stage(
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

pub(super) fn rollback_private_parent(
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
pub(super) fn rollback_linked_stage(
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
pub(super) fn rollback_exchange(
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

pub(super) fn commit_ambiguous(path: &Path, primary: Error) -> Error {
    Error::MutationCommitAmbiguous {
        path: path.to_owned(),
        primary: Box::new(primary),
    }
}
