use std::{fs, os::unix::fs::MetadataExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::PendingSystemTransition,
    },
    transition_journal::{Phase, RecoveryDisposition, RollbackActionOutcome, TransitionRecord},
};

use super::super::UsrRollbackReverseDispatchError;
pub(super) use super::super::reverse_test_support::{
    EffectOperationKind as OperationKind, ReverseFixture as Fixture, ReverseLayout,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UsrLayout {
    live: (u64, u64),
    staged: (u64, u64),
}

pub(super) fn enter(fixture: &Fixture) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(&fixture.fixture.installation, &fixture.fixture.database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved rollback"),
        Err(error) => error,
    }
}

pub(super) fn pending(error: &startup_gate::Error) -> &PendingSystemTransition {
    match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected recovery-pending startup result, got {other:?}"),
    }
}

pub(super) fn assert_usr_restored_pending(error: &startup_gate::Error) {
    let pending = pending(error);
    assert_eq!(pending.phase(), Phase::UsrRestored);
    assert_eq!(
        pending.disposition(),
        RecoveryDisposition::ResumeRollback {
            phase: Phase::UsrRestored,
        }
    );
    assert!(
        pending.blockers().is_empty(),
        "unexpected blockers: {:?}",
        pending.blockers()
    );
}

pub(super) fn assert_not_applied(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::NotApplied)
        ),
        "expected typed not-applied dispatch error, got {error:?}"
    );
}

pub(super) fn assert_ambiguous(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Ambiguous)
        ),
        "expected typed ambiguous dispatch error, got {error:?}"
    );
}

pub(super) fn expected_usr_restored(fixture: &Fixture, outcome: RollbackActionOutcome) -> TransitionRecord {
    fixture
        .record
        .rollback_successor(Some(outcome))
        .expect("exact reverse intent must admit its fixed UsrRestored successor")
}

pub(super) fn persist_usr_restored_fixture(fixture: &Fixture, outcome: RollbackActionOutcome) -> TransitionRecord {
    let successor = expected_usr_restored(fixture, outcome);
    let journal = fixture.open_journal();
    journal.advance(&fixture.record, &successor).unwrap();
    drop(journal);
    successor
}

pub(super) fn usr_layout(fixture: &Fixture) -> UsrLayout {
    let root = &fixture.fixture.installation.root;
    UsrLayout {
        live: directory_identity(&root.join("usr")),
        staged: directory_identity(&root.join(".cast/root/staging/usr")),
    }
}

pub(super) fn assert_layout_reversed(before: UsrLayout, after: UsrLayout) {
    assert_eq!(after.live, before.staged);
    assert_eq!(after.staged, before.live);
}

pub(super) fn assert_layout_unchanged(before: UsrLayout, after: UsrLayout) {
    assert_eq!(after, before);
}

pub(super) fn namespace_snapshot(fixture: &Fixture) -> impl std::fmt::Debug + Eq {
    snapshot_startup_recovery_namespace(&fixture.fixture.installation.root)
}

pub(super) fn assert_root_links_absent(fixture: &Fixture) {
    for name in ["bin", "sbin", "lib", "lib32", "lib64"] {
        assert!(
            fs::symlink_metadata(fixture.fixture.installation.root.join(name)).is_err(),
            "rollback-reverse dispatch unexpectedly published root link {name}"
        );
    }
}

fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    (metadata.dev(), metadata.ino())
}
