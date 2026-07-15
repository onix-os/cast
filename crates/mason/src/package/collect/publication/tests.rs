use std::{
    ffi::OsString,
    os::unix::{ffi::OsStringExt, fs::symlink},
};

use fs_err as fs;
use stone_recipe::derivation::PathRuleKind;

use super::*;
use crate::package::collect::{CollectionLimits, Error};

fn make_collector(root: &Path, limits: CollectionLimits) -> Collector {
    let mut collector = Collector::new_with_limits(root, limits);
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    collector
}

#[test]
fn regular_publication_accepts_n_and_rejects_n_plus_one_before_mutation() {
    let exact = tempfile::tempdir().unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_file_bytes = 8;
    let collector = make_collector(exact.path(), limits);
    let artifact = GeneratedArtifact::regular(PathBuf::from("nested/output"), b"12345678".to_vec(), 0o640, None, false);
    let info = collector
        .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(fs::read(&info.path).unwrap(), b"12345678");
    assert_eq!(fs::metadata(&info.path).unwrap().mode() & 0o7777, 0o640);
    info.verify_unchanged().unwrap();
    collector.seal().unwrap();

    let over = tempfile::tempdir().unwrap();
    let collector = make_collector(over.path(), limits);
    let artifact = GeneratedArtifact::regular(
        PathBuf::from("nested/output"),
        b"123456789".to_vec(),
        0o644,
        None,
        false,
    );
    assert!(matches!(
        collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "regular file bytes",
            limit: 8,
            actual: 9,
            ..
        })
    ));
    assert!(!over.path().join("nested").exists());
    collector.seal().unwrap();
}

#[test]
fn publication_normalizes_generated_directory_mode_under_adverse_umask() {
    const CHILD: &str = "MASON_GENERATED_PUBLICATION_UMASK_CHILD";
    const TEST: &str =
        "package::collect::publication::tests::publication_normalizes_generated_directory_mode_under_adverse_umask";

    // umask is process-global. Isolate it from every other unit test.
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "adverse-umask child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::regular(PathBuf::from("nested/output"), b"data".to_vec(), 0o640, None, false);
    // SAFETY: this is the sole test selected in the isolated child.
    let previous = unsafe { libc::umask(0o277) };
    let result = collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new());
    // SAFETY: restore the child mask before assertions can panic.
    unsafe { libc::umask(previous) };

    let info = result.unwrap().pop().unwrap();
    assert_eq!(fs::metadata(root.path().join("nested")).unwrap().mode() & 0o7777, 0o755);
    assert_eq!(fs::metadata(&info.path).unwrap().mode() & 0o7777, 0o640);
    collector.seal().unwrap();
}

#[test]
fn symlink_publication_accepts_n_and_rejects_n_plus_one() {
    let exact = tempfile::tempdir().unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_symlink_target_bytes = 8;
    let collector = make_collector(exact.path(), limits);
    let artifact = GeneratedArtifact::symlink(PathBuf::from("links/output"), "12345678".to_owned(), None, false);
    let info = collector
        .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(fs::read_link(&info.path).unwrap(), Path::new("12345678"));
    info.verify_unchanged().unwrap();
    collector.seal().unwrap();

    let over = tempfile::tempdir().unwrap();
    let collector = make_collector(over.path(), limits);
    let artifact = GeneratedArtifact::symlink(PathBuf::from("links/output"), "123456789".to_owned(), None, false);
    assert!(matches!(
        collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "symlink target bytes",
            limit: 8,
            actual: 9,
            ..
        })
    ));
    assert!(!over.path().join("links").exists());
    collector.seal().unwrap();
}

#[test]
fn publication_rejects_non_relative_or_unrepresentable_destinations_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let invalid = [
        PathBuf::new(),
        PathBuf::from("/absolute"),
        PathBuf::from("../escape"),
        PathBuf::from(OsString::from_vec(b"nul\0name".to_vec())),
        PathBuf::from(OsString::from_vec(vec![0xff])),
    ];

    for destination in invalid {
        let artifact = GeneratedArtifact::regular(destination, b"data".to_vec(), 0o644, None, false);
        assert!(
            collector
                .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
                .is_err()
        );
    }

    let conflicting = [
        GeneratedArtifact::regular(PathBuf::from("parent"), b"data".to_vec(), 0o644, None, false),
        GeneratedArtifact::symlink(PathBuf::from("parent/child"), "target".to_owned(), None, false),
    ];
    assert!(
        collector
            .publish_generated(&conflicting, &mut StoneDigestWriterHasher::new())
            .is_err()
    );

    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    collector.seal().unwrap();
}

#[test]
fn publication_never_traverses_a_witnessed_symlink_parent() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let parent = root.path().join("nested");
    symlink(outside.path(), &parent).unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::regular(PathBuf::from("nested/output"), b"data".to_vec(), 0o644, None, false);

    assert!(
        collector
            .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
            .is_err()
    );
    assert_eq!(fs::read_link(&parent).unwrap(), outside.path());
    assert!(!outside.path().join("output").exists());
    collector.seal().unwrap();
}

#[test]
fn regular_publication_never_clobbers_an_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    fs::write(&path, b"original").unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
    let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"replacement".to_vec(), 0o644, None, false);
    assert!(
        collector
            .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"original");
    collector.seal().unwrap();
}

#[test]
fn symlink_publication_never_clobbers_an_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    symlink("original", &path).unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::symlink(PathBuf::from("output"), "replacement".to_owned(), None, false);

    assert!(
        collector
            .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
            .is_err()
    );
    assert_eq!(fs::read_link(&path).unwrap(), Path::new("original"));
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [OsString::from("output")]
    );
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn post_publication_substitution_is_not_unlinked_and_poisons_inventory() {
    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
    let result = publish_generated_at_checkpoint(
        &collector,
        &[artifact],
        &mut StoneDigestWriterHasher::new(),
        |checkpoint, path| {
            if checkpoint == PublicationCheckpoint::AfterPublish {
                fs::remove_file(path).unwrap();
                symlink("attacker", path).unwrap();
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(Error::GeneratedPublicationRollback { .. })));
    assert_eq!(
        fs::read_link(root.path().join("output")).unwrap(),
        Path::new("attacker")
    );
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn same_inode_content_race_cannot_be_admitted_as_declared_bytes() {
    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
    let result = publish_generated_at_checkpoint(
        &collector,
        &[artifact],
        &mut StoneDigestWriterHasher::new(),
        |checkpoint, path| {
            if checkpoint == PublicationCheckpoint::AfterPublish {
                fs::write(path, b"attacker!").unwrap();
            }
            Ok(())
        },
    );

    assert!(matches!(result, Err(Error::GeneratedPublicationCommitAmbiguous { .. })));
    assert_eq!(fs::read(root.path().join("output")).unwrap(), b"attacker!");
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn partial_batch_failure_rolls_back_owned_nodes_and_poisons_inventory() {
    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifacts = [
        GeneratedArtifact::regular(PathBuf::from("nested/first"), b"first".to_vec(), 0o644, None, false),
        GeneratedArtifact::regular(PathBuf::from("nested/second"), b"second".to_vec(), 0o644, None, false),
    ];
    let result = publish_generated_at_checkpoint(
        &collector,
        &artifacts,
        &mut StoneDigestWriterHasher::new(),
        |checkpoint, path| {
            if checkpoint == PublicationCheckpoint::BeforePublish && path.ends_with("second") {
                return Err(changed(path, "injected publication failure"));
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!root.path().join("nested").exists());
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn failure_after_admission_is_ambiguous_and_poisons_without_deleting_committed_path() {
    let root = tempfile::tempdir().unwrap();
    let collector = make_collector(root.path(), CollectionLimits::default());
    let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
    let result = publish_generated_at_checkpoint(
        &collector,
        &[artifact],
        &mut StoneDigestWriterHasher::new(),
        |checkpoint, path| {
            if checkpoint == PublicationCheckpoint::AfterAdmission {
                return Err(changed(path, "injected post-admission failure"));
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(Error::GeneratedPublicationCommitAmbiguous { .. })));
    assert_eq!(fs::read(root.path().join("output")).unwrap(), b"generated");
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}
