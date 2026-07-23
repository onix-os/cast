use std::io;

use super::filesystem::{
    CaptureCheckpoint, Locator, NamespaceWitness, Operation, TaskRootWitness, namespace_witness, open_namespace,
    open_namespace_directory, open_task_root, require_same_namespace, require_same_task_root, task_root_witness,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub(super) namespace: NamespaceWitness,
    pub(super) task_root: TaskRootWitness,
}

pub(super) struct Capture {
    namespace: std::fs::File,
    task_root: std::fs::File,
    snapshot: Snapshot,
}

impl Capture {
    pub(super) const fn snapshot(&self) -> Snapshot {
        self.snapshot
    }

    pub(super) const fn task_root_file(&self) -> &std::fs::File {
        &self.task_root
    }

    pub(super) fn require_retained(&self, operation: &mut Operation<'_>) -> io::Result<()> {
        require_same_task_root(
            self.snapshot.task_root,
            task_root_witness(&self.task_root, operation)?,
            "retained current task root",
        )?;
        require_same_namespace(
            self.snapshot.namespace,
            namespace_witness(&self.namespace, operation)?,
            "retained mount namespace",
        )
    }

    fn require_pass_closed(
        &self,
        thread: &std::fs::File,
        namespace_directory: &std::fs::File,
        pass: usize,
        operation: &mut Operation<'_>,
    ) -> io::Result<()> {
        operation.emit(CaptureCheckpoint::PassTaskRootRecheck { pass })?;
        let (_task_root, task_root) = open_task_root(thread, operation)?;
        require_same_task_root(
            self.snapshot.task_root,
            task_root,
            "task-root name at complete-pass close",
        )?;
        operation.emit(CaptureCheckpoint::PassNamespaceRecheck { pass })?;
        let (_namespace, namespace) = open_namespace(namespace_directory, operation)?;
        require_same_namespace(
            self.snapshot.namespace,
            namespace,
            "mount-namespace name at complete-pass close",
        )
    }
}

pub(super) fn capture_twice(locator: &Locator, operation: &mut Operation<'_>) -> io::Result<Capture> {
    let first = capture_once(locator, 1, operation)?;
    let second = capture_once(locator, 2, operation)?;
    require_snapshot_matches(first.snapshot, second.snapshot, "two complete mount-context passes")?;
    first.require_retained(operation)?;
    second.require_retained(operation)?;
    Ok(first)
}

fn capture_once(locator: &Locator, pass: usize, operation: &mut Operation<'_>) -> io::Result<Capture> {
    let thread = locator.open_thread_for_pass(operation)?;
    let namespace_directory = open_namespace_directory(&thread, operation)?;
    operation.emit(CaptureCheckpoint::NamespaceDirectoryPinned { pass })?;
    let (namespace, namespace_witness) = open_namespace(&namespace_directory, operation)?;
    operation.emit(CaptureCheckpoint::NamespacePinned { pass })?;
    let (task_root, task_root_witness) = open_task_root(&thread, operation)?;
    operation.emit(CaptureCheckpoint::TaskRootPinned { pass })?;
    let capture = Capture {
        namespace,
        task_root,
        snapshot: Snapshot {
            namespace: namespace_witness,
            task_root: task_root_witness,
        },
    };
    capture.require_pass_closed(&thread, &namespace_directory, pass, operation)?;
    operation.emit(CaptureCheckpoint::PassComplete { pass })?;
    Ok(capture)
}

pub(super) fn require_snapshot_matches(expected: Snapshot, actual: Snapshot, context: &'static str) -> io::Result<()> {
    require_same_namespace(expected.namespace, actual.namespace, context)?;
    require_same_task_root(expected.task_root, actual.task_root, context)
}

impl Locator {
    pub(super) fn require_terminal_names(&self, expected: Snapshot, operation: &mut Operation<'_>) -> io::Result<()> {
        operation.emit(CaptureCheckpoint::TerminalTreeRebind)?;
        let thread = self.open_thread_for_terminal(operation)?;
        let namespace_directory = open_namespace_directory(&thread, operation)?;

        operation.emit(CaptureCheckpoint::TerminalNamespaceRebind)?;
        let (_namespace, namespace) = open_namespace(&namespace_directory, operation)?;
        require_same_namespace(expected.namespace, namespace, "terminal mount-namespace name rebind")?;

        operation.emit(CaptureCheckpoint::TerminalTaskRootRebind)?;
        let (_task_root, task_root) = open_task_root(&thread, operation)?;
        require_same_task_root(expected.task_root, task_root, "terminal task-root name rebind")?;

        operation.emit(CaptureCheckpoint::TerminalTaskRootRecheck)?;
        let (_task_root, task_root_recheck) = open_task_root(&thread, operation)?;
        require_same_task_root(expected.task_root, task_root_recheck, "terminal task-root name recheck")?;
        require_same_task_root(task_root, task_root_recheck, "terminal task-root name sandwich")?;

        operation.emit(CaptureCheckpoint::TerminalNamespaceRecheck)?;
        let (_namespace, namespace_recheck) = open_namespace(&namespace_directory, operation)?;
        require_same_namespace(
            expected.namespace,
            namespace_recheck,
            "terminal mount-namespace name recheck",
        )?;
        require_same_namespace(namespace, namespace_recheck, "terminal mount-namespace name sandwich")
    }
}

#[cfg(test)]
pub(super) fn validate_fixture_tree(locator: &Locator, operation: &mut Operation<'_>) -> io::Result<()> {
    let capture = capture_once(locator, 1, operation)?;
    locator.require_terminal_names(capture.snapshot, operation)?;
    capture.require_retained(operation)
}
