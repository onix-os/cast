use std::io;

#[cfg(test)]
use std::io::Cursor;

use super::{MountInfoSnapshotCheckpoint, filesystem as mountinfo_file};
use crate::linux_fs::mountinfo::{MountInfo, read_mountinfo_snapshot_bounded_until};

use super::super::{
    PreparedMountNamespaceAnchor,
    capture::{Snapshot, require_snapshot_matches},
    filesystem::{
        NamespaceWitness, Operation, TaskRootWitness, namespace_witness, open_namespace, open_namespace_directory,
        open_task_root, require_same_namespace, require_same_task_root, task_root_witness,
    },
};

#[cfg(test)]
const SYNTHETIC_FILE_DESCRIPTOR_RESERVATION: usize = 2;
#[cfg(test)]
const SYNTHETIC_FILE_AUTHENTICATION_WORK: usize = 16;

struct ExactThreadContext {
    namespace_directory: std::fs::File,
    namespace: std::fs::File,
    namespace_identity: NamespaceWitness,
    task_root: std::fs::File,
    task_root_identity: TaskRootWitness,
    thread: std::fs::File,
}

pub(super) fn read_current_thread_mountinfo(
    anchor: &PreparedMountNamespaceAnchor,
    operation: &mut Operation<'_>,
) -> io::Result<(Vec<u8>, MountInfo)> {
    if !operation.is_production() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture operation cannot enter the production mountinfo capture path",
        ));
    }

    let exact = open_exact_thread(anchor, operation)?;
    let (mut mountinfo, file_identity) = mountinfo_file::open_mountinfo(&exact.thread, operation)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::FileOpened)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeRead)?;
    let (bytes, parsed) = read_mountinfo_snapshot_bounded_until(&mut mountinfo, operation.deadline())?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::AfterRead)?;

    let after_read = mountinfo_file::authenticate_mountinfo_file(&mountinfo, operation)?;
    mountinfo_file::require_same_mountinfo_file(file_identity, after_read, "mountinfo descriptor around bounded read")?;
    let (_rebound_mountinfo, rebound_identity) = mountinfo_file::open_mountinfo(&exact.thread, operation)?;
    mountinfo_file::require_same_mountinfo_file(
        file_identity,
        rebound_identity,
        "fixed current-thread mountinfo name rebind",
    )?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::FileRebound)?;
    close_exact_thread(anchor, &exact, operation)?;
    Ok((bytes, parsed))
}

/// Cursor-only analogue of the exact production checkpoint schedule.
///
/// It opens and rechecks the admitted ordinary fixture's real synthetic
/// namespace marker and task-root descriptors from one retained tree. The
/// `FileOpened` and `FileRebound` events only reserve production-equivalent
/// descriptor/authentication cost and order race hooks around Cursor bytes;
/// they do not claim to authenticate an ordinary file as procfs mountinfo.
#[cfg(test)]
pub(super) fn read_fixture_mountinfo(
    anchor: &PreparedMountNamespaceAnchor,
    bytes: &[u8],
    operation: &mut Operation<'_>,
) -> io::Result<(Vec<u8>, MountInfo)> {
    if operation.is_production() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "production operation cannot enter the Cursor mountinfo fixture",
        ));
    }

    let exact = open_exact_thread(anchor, operation)?;
    operation.charge_descriptors(
        SYNTHETIC_FILE_DESCRIPTOR_RESERVATION,
        "reserving two fixed mountinfo file opens for Cursor fixture",
    )?;
    operation.charge(
        SYNTHETIC_FILE_AUTHENTICATION_WORK,
        "reserving mountinfo regular-file and procfs authentication for Cursor fixture",
    )?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::FileOpened)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeRead)?;
    let (bytes, parsed) = read_mountinfo_snapshot_bounded_until(&mut Cursor::new(bytes), operation.deadline())?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::AfterRead)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::FileRebound)?;
    close_exact_thread(anchor, &exact, operation)?;
    Ok((bytes, parsed))
}

fn open_exact_thread(
    anchor: &PreparedMountNamespaceAnchor,
    operation: &mut Operation<'_>,
) -> io::Result<ExactThreadContext> {
    // Namespace, task root, and mountinfo are all relative to this one exact
    // retained current-thread directory (or admitted synthetic equivalent).
    let thread = anchor.locator.open_thread_for_terminal(operation)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::ThreadOpened)?;
    let namespace_directory = open_namespace_directory(&thread, operation)?;
    let (namespace, namespace_identity) = open_namespace(&namespace_directory, operation)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::NamespacePinned)?;
    let (task_root, task_root_identity) = open_task_root(&thread, operation)?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::TaskRootPinned)?;
    require_snapshot_matches(
        anchor.capture.snapshot(),
        Snapshot {
            namespace: namespace_identity,
            task_root: task_root_identity,
        },
        "exact mountinfo current-thread context",
    )?;
    Ok(ExactThreadContext {
        namespace_directory,
        namespace,
        namespace_identity,
        task_root,
        task_root_identity,
        thread,
    })
}

fn close_exact_thread(
    anchor: &PreparedMountNamespaceAnchor,
    exact: &ExactThreadContext,
    operation: &mut Operation<'_>,
) -> io::Result<()> {
    let retained_task_root = task_root_witness(&exact.task_root, operation)?;
    require_same_task_root(
        exact.task_root_identity,
        retained_task_root,
        "retained task root after mountinfo read",
    )?;
    let (_rebound_task_root, rebound_task_root) = open_task_root(&exact.thread, operation)?;
    require_same_task_root(
        exact.task_root_identity,
        rebound_task_root,
        "fixed task-root name after mountinfo read",
    )?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::TaskRootRechecked)?;

    let retained_namespace = namespace_witness(&exact.namespace, operation)?;
    require_same_namespace(
        exact.namespace_identity,
        retained_namespace,
        "retained mount namespace after mountinfo read",
    )?;
    let (_rebound_namespace, rebound_namespace) = open_namespace(&exact.namespace_directory, operation)?;
    require_same_namespace(
        exact.namespace_identity,
        rebound_namespace,
        "fixed mount-namespace name after mountinfo read",
    )?;
    operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::NamespaceRechecked)?;

    require_snapshot_matches(
        anchor.capture.snapshot(),
        Snapshot {
            namespace: rebound_namespace,
            task_root: rebound_task_root,
        },
        "mountinfo same-thread closing context",
    )?;
    operation.checkpoint()
}
