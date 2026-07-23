use std::io;

use super::super::super::mount_namespace::{FixtureMountInfoSnapshotLimits, FixtureMountNamespaceCheckpoint};
use super::super::super::mountinfo_attachment::select_mountinfo_attachment_until;
use super::support::{RECORD, SyntheticMountInfoContext, assert_error_kind, deadline};

fn is_snapshot_checkpoint(checkpoint: FixtureMountNamespaceCheckpoint) -> bool {
    matches!(
        checkpoint,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeExactThread
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotThreadOpened
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotNamespacePinned
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotTaskRootPinned
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileOpened
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeRead
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotAfterRead
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileRebound
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotTaskRootRechecked
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotNamespaceRechecked
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeClosingAnchor
            | FixtureMountNamespaceCheckpoint::MountInfoSnapshotComplete
    )
}

#[test]
fn stable_cursor_snapshot_retains_exact_bytes_and_parsed_values() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let snapshot = prepared
        .read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits::default(),
            deadline(),
            &mut |_| Ok(()),
        )
        .unwrap();

    assert_eq!(snapshot.bytes(), RECORD);
    assert_eq!(snapshot.mountinfo().entries().len(), 1);
    let entry = &snapshot.mountinfo().entries()[0];
    assert_eq!(entry.mount_id(), 41);
    assert_eq!(entry.device().major(), 259);
    assert_eq!(entry.device().minor(), 7);
    assert_eq!(entry.root(), b"/");
    assert_eq!(entry.mount_point(), b"/synthetic/firmware-attachment");
    fixture.assert_outside_unchanged();
}

#[test]
fn cursor_fixture_executes_the_exact_inner_checkpoint_schedule() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let mut observed = Vec::new();
    prepared
        .read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits::default(),
            deadline(),
            &mut |checkpoint| {
                if is_snapshot_checkpoint(checkpoint) {
                    observed.push(checkpoint);
                }
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        observed,
        [
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeExactThread,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotThreadOpened,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotNamespacePinned,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotTaskRootPinned,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileOpened,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeRead,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotAfterRead,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileRebound,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotTaskRootRechecked,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotNamespaceRechecked,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeClosingAnchor,
            FixtureMountNamespaceCheckpoint::MountInfoSnapshotComplete,
        ]
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn production_reader_rejects_a_fixture_anchor_before_live_access() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    assert_error_kind(
        prepared.read_current_thread_mountinfo_until(deadline()),
        io::ErrorKind::InvalidInput,
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn unrelated_mount_table_churn_does_not_require_whole_snapshot_equality() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let base = prepared
        .read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits::default(),
            deadline(),
            &mut |_| Ok(()),
        )
        .unwrap();
    let mut churned_bytes = RECORD.to_vec();
    churned_bytes.extend_from_slice(b"99 1 0:42 / /synthetic/unrelated rw - tmpfs ignored rw\n");
    let churned = prepared
        .read_fixture_mountinfo_bytes_with(
            &churned_bytes,
            FixtureMountInfoSnapshotLimits::default(),
            deadline(),
            &mut |_| Ok(()),
        )
        .unwrap();

    let base_selected = select_mountinfo_attachment_until(
        base.mountinfo(),
        b"/synthetic/firmware-attachment",
        41,
        259,
        7,
        deadline(),
    )
    .unwrap();
    let churned_selected = select_mountinfo_attachment_until(
        churned.mountinfo(),
        b"/synthetic/firmware-attachment",
        41,
        259,
        7,
        deadline(),
    )
    .unwrap();
    assert_eq!(base_selected.mount_id(), churned_selected.mount_id());
    assert_eq!(base_selected.mount_point(), churned_selected.mount_point());
    assert_eq!(base.mountinfo().entries().len(), 1);
    assert_eq!(churned.mountinfo().entries().len(), 2);
    fixture.assert_outside_unchanged();
}
