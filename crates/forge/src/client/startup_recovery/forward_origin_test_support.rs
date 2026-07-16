use std::{
    fs,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

use crate::{Installation, db, transition_journal::Phase};

use super::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::{self, CleanSystemStartup},
};

/// Enter the real mutable startup gate after a coordinator-owned exchange
/// fault and require the exact recovery-pending boundary reached by the
/// parent-durability normalizer.
pub(crate) fn assert_usr_exchange_intent_post_recovers_to_pending_reverse(
    installation: &Installation,
    state_db: &db::state::Database,
) {
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(installation, state_db, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved forward-exchange residue"),
        Err(error) => error,
    };
    let pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected recovery-pending startup result, got {other:?}"),
    };
    assert_eq!(pending.phase(), Phase::RollbackDecided);
    assert!(
        pending.blockers().is_empty(),
        "unexpected startup blockers: {:?}",
        pending.blockers()
    );
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StartupRecoveryNamespaceEntry {
    relative: PathBuf,
    kind: StartupRecoveryNamespaceEntryKind,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupRecoveryNamespaceEntryKind {
    Directory,
    File,
    Symlink,
}

/// Capture every visible installation entry except the journal subtree, whose
/// exact change is asserted independently by the coordinator-origin test.
pub(crate) fn snapshot_startup_recovery_namespace(root: &Path) -> Vec<StartupRecoveryNamespaceEntry> {
    let mut entries = Vec::new();
    snapshot_directory(root, root, &mut entries);
    entries
}

fn snapshot_directory(root: &Path, directory: &Path, output: &mut Vec<StartupRecoveryNamespaceEntry>) {
    let mut children = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .as_bytes()
            .cmp(right.file_name().unwrap_or_default().as_bytes())
    });
    for path in children {
        let relative = path.strip_prefix(root).unwrap().to_owned();
        if relative.starts_with(Path::new(".cast/journal")) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        let file_type = metadata.file_type();
        let (kind, payload) = if file_type.is_dir() {
            (StartupRecoveryNamespaceEntryKind::Directory, Vec::new())
        } else if file_type.is_file() {
            (StartupRecoveryNamespaceEntryKind::File, fs::read(&path).unwrap())
        } else if file_type.is_symlink() {
            (
                StartupRecoveryNamespaceEntryKind::Symlink,
                fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec(),
            )
        } else {
            panic!("unexpected startup-recovery namespace entry kind at {}", path.display());
        };
        output.push(StartupRecoveryNamespaceEntry {
            relative,
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            payload,
        });
        if file_type.is_dir() {
            snapshot_directory(root, &path, output);
        }
    }
}
