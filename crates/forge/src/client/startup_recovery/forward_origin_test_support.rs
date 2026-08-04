use std::{
    fs,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

use crate::{Installation, db, transition_journal::Phase};

use super::{
    MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
    active_state_snapshot::ActiveStateReservation,
    startup_gate::{self, CleanSystemStartup},
};

/// Re-enter the real mutable startup gate until it admits the system, and
/// return how many entries that took.
///
/// The other helpers here each advance recovery by one phase, which is what a
/// test pinning an exact chain wants. A test that only needs the installation
/// to be *usable again* needs the whole chain run to terminal, and asserting
/// every intermediate phase would couple it to a chain it is not testing.
///
/// Every intermediate entry must still be blocker-free: a recovery that stalls
/// is a different outcome from one that completes, and silently looping past a
/// stall would turn this into an infinite retry.
pub(crate) fn drive_startup_recovery_to_clean(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) -> usize {
    const BOUND: usize = 32;
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let mut visited = Vec::new();
    for entries in 1..=BOUND {
        match CleanSystemStartup::enter(&system, &reservation) {
            Ok(_) => return entries,
            Err(startup_gate::Error::RecoveryPending(pending)) => {
                assert!(
                    pending.blockers().is_empty(),
                    "recovery stalled at {:?} with blockers: {:?}",
                    pending.phase(),
                    pending.blockers()
                );
                visited.push(pending.phase());
            }
            Err(other) => panic!("recovery failed instead of advancing: {other:?} after {visited:?}"),
        }
    }
    // The phase trail distinguishes a chain that is merely longer than the
    // bound from one that is looping, which is the failure worth naming.
    panic!("recovery did not reach a clean startup within {BOUND} entries; visited {visited:?}");
}

/// Enter the real mutable startup gate at a reverse-exchange intent and
/// require that it does **not** reach `UsrRestored`, returning a rendering of
/// why it declined.
///
/// Every other helper here asserts that recovery *advances* to an exact phase.
/// A test that plants a substituted live tree needs the opposite proof, and
/// cannot express it through an assertion that hard-codes the phase it expects.
/// The reason is returned rendered because `startup_gate::Error` is private to
/// this facade; only the fact of refusal, and which blocker produced it,
/// crosses into the coordinator tests.
pub(crate) fn reverse_exchange_intent_refusal_reason(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) -> String {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted a substituted live tree"),
        Err(error) => error,
    };
    match error {
        startup_gate::Error::RecoveryPending(pending) => {
            assert_ne!(
                pending.phase(),
                Phase::UsrRestored,
                "recovery reversed the exchange over a substituted live tree"
            );
            let blockers = format!("{:?}", pending.blockers());
            assert!(
                !pending.blockers().is_empty(),
                "recovery stalled at {:?} without naming a blocker",
                pending.phase()
            );
            blockers
        }
        other => format!("{other:?}"),
    }
}

/// Enter the real mutable startup gate after a coordinator-owned exchange
/// fault and require the exact recovery-pending rollback decision. A durable
/// `UsrExchanged` source first consumes the separate root-ABI normalization
/// entry; an intent source reaches the decision directly.
pub(crate) fn assert_usr_exchange_post_recovers_to_pending_reverse(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved forward-exchange residue"),
        Err(error) => error,
    };
    let mut pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected recovery-pending startup result, got {other:?}"),
    };
    if pending.phase() == Phase::UsrExchanged {
        assert!(
            pending.blockers().is_empty(),
            "unexpected root-ABI normalization blockers: {:?}",
            pending.blockers()
        );
        drop(pending);
        let error = match CleanSystemStartup::enter(&system, &reservation) {
            Ok(_) => panic!("startup unexpectedly admitted an unresolved root-ABI durability boundary"),
            Err(error) => error,
        };
        pending = match error {
            startup_gate::Error::RecoveryPending(pending) => pending,
            other => panic!("expected post-normalization recovery-pending result, got {other:?}"),
        };
    }
    assert_eq!(pending.phase(), Phase::RollbackDecided);
    assert!(
        pending.blockers().is_empty(),
        "unexpected startup blockers: {:?}",
        pending.blockers()
    );
}

/// Re-enter the real mutable startup gate at coordinator-owned
/// `RootLinksComplete` and require exactly one journal-only rollback decision.
/// The already-complete root ABI is retained as evidence and is never
/// republished by this recovery entry.
pub(crate) fn assert_root_links_complete_restart_persists_rollback_decision(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted durable RootLinksComplete"),
        Err(error) => error,
    };
    let pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected RootLinksComplete recovery-pending result, got {other:?}"),
    };
    assert_eq!(pending.phase(), Phase::RollbackDecided);
    assert!(
        pending.blockers().is_empty(),
        "unexpected RootLinksComplete rollback-decision blockers: {:?}",
        pending.blockers()
    );
}

/// Re-enter the real mutable startup gate at the exact decision produced by
/// the helper above and require its journal-only route to the reverse intent.
pub(crate) fn assert_usr_rollback_decision_routes_to_reverse_exchange_intent(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted a decided /usr rollback"),
        Err(error) => error,
    };
    let pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected routed recovery-pending startup result, got {other:?}"),
    };
    assert_eq!(pending.phase(), Phase::ReverseExchangeIntent);
    assert!(
        pending.blockers().is_empty(),
        "unexpected routed startup blockers: {:?}",
        pending.blockers()
    );
}

/// Re-enter the real mutable startup gate at an exact reverse intent and
/// require one reverse phase to stop at durable `UsrRestored`.
pub(crate) fn assert_reverse_exchange_intent_recovers_to_usr_restored(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unfinished /usr rollback"),
        Err(error) => error,
    };
    let pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected reverse recovery-pending startup result, got {other:?}"),
    };
    assert_eq!(pending.phase(), Phase::UsrRestored);
    assert!(
        pending.blockers().is_empty(),
        "unexpected reverse startup blockers: {:?}",
        pending.blockers()
    );
}

/// Re-enter the real mutable startup gate at exact `UsrRestored` evidence and
/// require its journal-only route to candidate preservation.
pub(crate) fn assert_usr_restored_routes_to_candidate_preserve_intent(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
) {
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_db.clone(),
        layout_db.clone(),
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(&system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unfinished candidate rollback"),
        Err(error) => error,
    };
    let pending = match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected routed recovery-pending startup result, got {other:?}"),
    };
    assert_eq!(pending.phase(), Phase::CandidatePreserveIntent);
    assert!(
        pending.blockers().is_empty(),
        "unexpected candidate-preservation startup blockers: {:?}",
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
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupRecoveryNamespaceEntryKind {
    Directory,
    File,
    Symlink,
}

/// Capture the semantic identity of every visible installation entry except
/// the journal subtree, whose exact change is asserted independently by the
/// coordinator-origin test. Rename-driven timestamps are deliberately omitted:
/// a forward and reverse `RENAME_EXCHANGE` restores names, inodes, modes, link
/// counts, lengths, and payloads, but the kernel necessarily advances ctime on
/// the moved directories and timestamps on their parents.
pub(crate) fn snapshot_startup_recovery_namespace(root: &Path) -> Vec<StartupRecoveryNamespaceEntry> {
    let mut entries = Vec::new();
    snapshot_directory(root, root, &mut entries);
    entries
}

pub(crate) fn snapshot_startup_recovery_namespace_without_root_abi(root: &Path) -> Vec<StartupRecoveryNamespaceEntry> {
    snapshot_startup_recovery_namespace(root)
        .into_iter()
        .filter(|entry| {
            !["bin", "sbin", "lib", "lib32", "lib64"]
                .into_iter()
                .any(|name| entry.relative == Path::new(name))
        })
        .collect()
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
            payload,
        });
        if file_type.is_dir() {
            snapshot_directory(root, &path, output);
        }
    }
}
