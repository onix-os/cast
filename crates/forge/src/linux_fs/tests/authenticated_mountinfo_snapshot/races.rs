use std::io;

use super::super::super::mount_namespace::{FixtureMountInfoSnapshotLimits, FixtureMountNamespaceCheckpoint};
use super::support::{RECORD, SyntheticMountInfoContext, deadline};

fn assert_replacement_rejected(
    fixture: &SyntheticMountInfoContext,
    checkpoint: FixtureMountNamespaceCheckpoint,
    mut replace: impl FnMut() -> io::Result<()>,
) {
    let prepared = fixture.prepared().unwrap();
    let mut reached = false;
    let result = prepared.read_fixture_mountinfo_bytes_with(
        RECORD,
        FixtureMountInfoSnapshotLimits::default(),
        deadline(),
        &mut |observed| {
            if observed == checkpoint && !reached {
                reached = true;
                replace()?;
            }
            Ok(())
        },
    );
    let error = match result {
        Ok(_) => panic!("mount-context replacement unexpectedly produced a snapshot"),
        Err(error) => error,
    };
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    assert!(reached);
    fixture.assert_outside_unchanged();
}

#[test]
fn namespace_and_root_replacements_after_cursor_read_fail_closed() {
    let namespace = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &namespace,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotAfterRead,
        || namespace.replace_namespace_identity(),
    );

    let root = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &root,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotAfterRead,
        || root.replace_root_identity(),
    );
}

#[test]
fn namespace_and_root_replacements_at_synthetic_file_rebind_fail_closed() {
    let namespace = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &namespace,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileRebound,
        || namespace.replace_namespace_identity(),
    );

    let root = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &root,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotFileRebound,
        || root.replace_root_identity(),
    );
}

#[test]
fn namespace_and_root_replacements_at_both_outer_anchor_edges_fail_closed() {
    let opening_namespace = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &opening_namespace,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeExactThread,
        || opening_namespace.replace_namespace_identity(),
    );

    let opening_root = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &opening_root,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeExactThread,
        || opening_root.replace_root_identity(),
    );

    let closing_namespace = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &closing_namespace,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeClosingAnchor,
        || closing_namespace.replace_namespace_identity(),
    );

    let closing_root = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &closing_root,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeClosingAnchor,
        || closing_root.replace_root_identity(),
    );
}

#[test]
fn task_tree_replacement_before_closing_anchor_fails_closed() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    assert_replacement_rejected(
        &fixture,
        FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeClosingAnchor,
        || fixture.replace_tree_identity(),
    );
}
