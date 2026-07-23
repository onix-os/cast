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
