use std::{io, mem::zeroed, os::fd::AsRawFd as _, time::Instant};

#[cfg(test)]
use std::{
    ffi::{CStr, CString},
    os::fd::{FromRawFd as _, OwnedFd},
};

use super::super::{
    authenticated_current_thread_procfs_with_deadline, controlled_resolution, descriptor_mount_id_until,
    openat2_file_until, require_procfs_with_deadline, retry_interrupted,
};
use super::AuthenticatedMountNamespaceIdentity;

#[cfg(test)]
use super::super::{PROC_SUPER_MAGIC, SYSFS_MAGIC};

pub(super) const NSFS_MAGIC: nix::libc::c_long = 0x6e73_6673;
// Linux UAPI `_IO(NSIO, 0x3)`: an argument-free ioctl with NSIO=0xb7.
// `_IO` contributes only its type and command fields on supported Linux ABIs.
const NSIO: nix::libc::c_ulong = 0xb7;
const NS_GET_NSTYPE: nix::libc::c_ulong = (NSIO << 8) | 0x3;
const _: () = assert!(NS_GET_NSTYPE == 0xb703);
const AUTHENTICATED_THREAD_PROC_DESCRIPTOR_BOUND: usize = 5;
pub(super) const DESCRIPTOR_MOUNT_ID_DESCRIPTOR_BOUND: usize = 24;
pub(super) const DESCRIPTOR_MOUNT_ID_WORK_BOUND: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct MountNamespaceLimits {
    pub(super) max_work: usize,
    pub(super) max_descriptors: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CaptureCheckpoint {
    TreeRebind,
    NamespaceDirectoryPinned { pass: usize },
    NamespacePinned { pass: usize },
    TaskRootPinned { pass: usize },
    PassTaskRootRecheck { pass: usize },
    PassNamespaceRecheck { pass: usize },
    PassComplete { pass: usize },
    TerminalTreeRebind,
    TerminalNamespaceRebind,
    TerminalTaskRootRebind,
    TerminalTaskRootRecheck,
    TerminalNamespaceRecheck,
    OperationComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Production,
    #[cfg(test)]
    Fixture,
}

pub(super) struct Operation<'a> {
    deadline: Instant,
    remaining_work: usize,
    initial_work: usize,
    remaining_descriptors: usize,
    initial_descriptors: usize,
    admission: Admission,
    #[cfg(test)]
    hook: Option<&'a mut dyn FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>>,
    #[cfg(test)]
    clock: Option<&'a mut dyn FnMut() -> Instant>,
    #[cfg(not(test))]
    _lifetime: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> Operation<'a> {
    pub(super) fn production(limits: MountNamespaceLimits, deadline: Instant) -> Self {
        Self::new(limits, deadline, Admission::Production)
    }

    fn new(limits: MountNamespaceLimits, deadline: Instant, admission: Admission) -> Self {
        Self {
            deadline,
            remaining_work: limits.max_work,
            initial_work: limits.max_work,
            remaining_descriptors: limits.max_descriptors,
            initial_descriptors: limits.max_descriptors,
            admission,
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            clock: None,
            #[cfg(not(test))]
            _lifetime: std::marker::PhantomData,
        }
    }

    #[cfg(test)]
    pub(super) fn fixture(
        limits: MountNamespaceLimits,
        deadline: Instant,
        hook: &'a mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut operation = Self::validate_fixture_limits(limits, deadline)?;
        operation.hook = Some(hook);
        Ok(operation)
    }

    #[cfg(test)]
    pub(super) fn fixture_with_clock(
        limits: MountNamespaceLimits,
        deadline: Instant,
        hook: &'a mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &'a mut impl FnMut() -> Instant,
    ) -> io::Result<Self> {
        let mut operation = Self::validate_fixture_limits(limits, deadline)?;
        operation.hook = Some(hook);
        operation.clock = Some(clock);
        operation.checkpoint()?;
        Ok(operation)
    }

    #[cfg(test)]
    pub(super) fn fixture_without_hook(limits: MountNamespaceLimits, deadline: Instant) -> io::Result<Self> {
        Self::validate_fixture_limits(limits, deadline)
    }

    #[cfg(test)]
    fn validate_fixture_limits(limits: MountNamespaceLimits, deadline: Instant) -> io::Result<Self> {
        if limits.max_work == 0 || limits.max_descriptors == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mount-context fixture limits must both be nonzero",
            ));
        }
        let mut operation = Self::new(limits, deadline, Admission::Fixture);
        operation.checkpoint()?;
        Ok(operation)
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) const fn is_production(&self) -> bool {
        matches!(self.admission, Admission::Production)
    }

    #[cfg(test)]
    pub(super) const fn consumed_work(&self) -> usize {
        self.initial_work - self.remaining_work
    }

    #[cfg(test)]
    pub(super) const fn consumed_descriptors(&self) -> usize {
        self.initial_descriptors - self.remaining_descriptors
    }

    pub(super) fn checkpoint(&mut self) -> io::Result<()> {
        #[cfg(test)]
        let now = if let Some(clock) = self.clock.as_mut() {
            clock()
        } else {
            Instant::now()
        };
        #[cfg(not(test))]
        let now = Instant::now();
        if now > self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mount-context operation exceeded its deadline",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn charge(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.checkpoint()?;
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "mount-context operation exceeded its {} unit work limit while {action}",
                    self.initial_work
                ),
            )
        })?;
        self.checkpoint()
    }

    pub(super) fn charge_descriptor(&mut self, action: &'static str) -> io::Result<()> {
        self.charge_descriptors(1, action)
    }

    pub(super) fn charge_descriptors(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.charge(amount, action)?;
        self.remaining_descriptors = self.remaining_descriptors.checked_sub(amount).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "mount-context operation exceeded its {} descriptor-open limit while {action}",
                    self.initial_descriptors
                ),
            )
        })?;
        Ok(())
    }

    pub(super) fn emit(&mut self, checkpoint: CaptureCheckpoint) -> io::Result<()> {
        self.checkpoint()?;
        #[cfg(test)]
        if let Some(hook) = self.hook.as_mut() {
            hook(checkpoint.into())?;
        }
        #[cfg(not(test))]
        let _ = checkpoint;
        self.checkpoint()
    }

    pub(super) fn emit_attachment(&mut self, checkpoint: super::attachment::AttachmentCheckpoint) -> io::Result<()> {
        self.checkpoint()?;
        #[cfg(test)]
        if let Some(hook) = self.hook.as_mut() {
            hook(checkpoint.into())?;
        }
        #[cfg(not(test))]
        let _ = checkpoint;
        self.checkpoint()
    }

    pub(super) fn emit_mountinfo_snapshot(
        &mut self,
        checkpoint: super::mountinfo_snapshot::MountInfoSnapshotCheckpoint,
    ) -> io::Result<()> {
        self.checkpoint()?;
        #[cfg(test)]
        if let Some(hook) = self.hook.as_mut() {
            hook(checkpoint.into())?;
        }
        #[cfg(not(test))]
        let _ = checkpoint;
        self.checkpoint()
    }
}

#[cfg(test)]
impl From<CaptureCheckpoint> for super::FixtureMountNamespaceCheckpoint {
    fn from(checkpoint: CaptureCheckpoint) -> Self {
        match checkpoint {
            CaptureCheckpoint::TreeRebind => Self::TreeRebind,
            CaptureCheckpoint::NamespaceDirectoryPinned { pass } => Self::NamespaceDirectoryPinned { pass },
            CaptureCheckpoint::NamespacePinned { pass } => Self::NamespacePinned { pass },
            CaptureCheckpoint::TaskRootPinned { pass } => Self::TaskRootPinned { pass },
            CaptureCheckpoint::PassTaskRootRecheck { pass } => Self::PassTaskRootRecheck { pass },
            CaptureCheckpoint::PassNamespaceRecheck { pass } => Self::PassNamespaceRecheck { pass },
            CaptureCheckpoint::PassComplete { pass } => Self::PassComplete { pass },
            CaptureCheckpoint::TerminalTreeRebind => Self::TerminalTreeRebind,
            CaptureCheckpoint::TerminalNamespaceRebind => Self::TerminalNamespaceRebind,
            CaptureCheckpoint::TerminalTaskRootRebind => Self::TerminalTaskRootRebind,
            CaptureCheckpoint::TerminalTaskRootRecheck => Self::TerminalTaskRootRecheck,
            CaptureCheckpoint::TerminalNamespaceRecheck => Self::TerminalNamespaceRecheck,
            CaptureCheckpoint::OperationComplete => Self::MountContextComplete,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NamespaceWitness {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) kind: u32,
    namespace_type: nix::libc::c_int,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TaskRootWitness {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) kind: u32,
    pub(super) mount_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RawWitness {
    device: u64,
    inode: u64,
    kind: u32,
}

pub(super) enum Locator {
    Production,
    #[cfg(test)]
    Fixture {
        parent: std::fs::File,
        tree_name: CString,
        tree_witness: RawWitness,
    },
}

impl Locator {
    pub(super) const fn production() -> Self {
        Self::Production
    }

    #[cfg(test)]
    pub(super) const fn is_fixture(&self) -> bool {
        matches!(self, Self::Fixture { .. })
    }

    #[cfg(test)]
    pub(super) fn admit_fixture(
        parent: std::fs::File,
        tree_name: CString,
        operation: &mut Operation<'_>,
    ) -> io::Result<Self> {
        require_component(&tree_name, "fixture task tree")?;
        reject_kernel_pseudo_fixture(&parent, operation, "fixture task-tree parent")?;
        raw_witness(&parent, operation, "fixture task-tree parent")?
            .require_kind(nix::libc::S_IFDIR, "fixture task-tree parent")?;
        let tree = open_controlled_directory(&parent, &tree_name, operation, "opening fixture task tree")?;
        reject_kernel_pseudo_fixture(&tree, operation, "fixture task tree")?;
        let tree_witness = raw_witness(&tree, operation, "fixture task tree")?
            .require_kind(nix::libc::S_IFDIR, "fixture task tree")?;
        Ok(Self::Fixture {
            parent,
            tree_name,
            tree_witness,
        })
    }

    pub(super) fn open_thread_for_pass(&self, operation: &mut Operation<'_>) -> io::Result<std::fs::File> {
        match self {
            Self::Production => open_current_thread(operation),
            #[cfg(test)]
            Self::Fixture { .. } => {
                operation.emit(CaptureCheckpoint::TreeRebind)?;
                self.open_fixture_tree(operation)
            }
        }
    }

    pub(super) fn open_thread_for_terminal(&self, operation: &mut Operation<'_>) -> io::Result<std::fs::File> {
        match self {
            Self::Production => open_current_thread(operation),
            #[cfg(test)]
            Self::Fixture { .. } => self.open_fixture_tree(operation),
        }
    }

    #[cfg(test)]
    fn open_fixture_tree(&self, operation: &mut Operation<'_>) -> io::Result<std::fs::File> {
        let Self::Fixture {
            parent,
            tree_name,
            tree_witness,
        } = self
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "production locator cannot enter fixture tree",
            ));
        };
        // Fixtures perform no procfs access, but reserve the same encapsulated
        // production acquisition before resolving their ordinary named tree.
        reserve_authenticated_thread(operation)?;
        reject_kernel_pseudo_fixture(parent, operation, "retained fixture task-tree parent")?;
        let tree = open_controlled_directory(parent, tree_name, operation, "rebinding fixture task tree")?;
        reject_kernel_pseudo_fixture(&tree, operation, "rebound fixture task tree")?;
        require_same_raw(
            *tree_witness,
            raw_witness(&tree, operation, "rebound fixture task tree")?,
            "fixture task tree",
        )?;
        Ok(tree)
    }

    #[cfg(test)]
    pub(super) fn reopen_owned(&self, operation: &mut Operation<'_>) -> io::Result<Self> {
        match self {
            Self::Production => Ok(Self::Production),
            Self::Fixture {
                parent,
                tree_name,
                tree_witness,
            } => Ok(Self::Fixture {
                parent: duplicate(parent, operation, "duplicating fixture task-tree parent")?,
                tree_name: copy_cstring(tree_name, "fixture task-tree name")?,
                tree_witness: *tree_witness,
            }),
        }
    }
}

fn open_current_thread(operation: &mut Operation<'_>) -> io::Result<std::fs::File> {
    reserve_authenticated_thread(operation)?;
    authenticated_current_thread_procfs_with_deadline(Some(operation.deadline()))
}

fn reserve_authenticated_thread(operation: &mut Operation<'_>) -> io::Result<()> {
    operation.charge_descriptors(
        AUTHENTICATED_THREAD_PROC_DESCRIPTOR_BOUND,
        "authenticating current-thread procfs path",
    )?;
    operation.charge(256, "reserving authenticated current-thread procfs work")
}

pub(super) fn open_namespace_directory(
    thread: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<std::fs::File> {
    operation.charge_descriptor("opening fixed current-thread namespace directory")?;
    let directory = openat2_file_until(
        thread.as_raw_fd(),
        c"ns",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    if operation.is_production() {
        operation.charge(1, "authenticating current-thread namespace directory as procfs")?;
        require_procfs_with_deadline(
            &directory,
            std::path::Path::new("/proc/<pid>/task/<tid>/ns"),
            Some(operation.deadline()),
        )?;
    } else {
        #[cfg(test)]
        reject_kernel_pseudo_fixture(&directory, operation, "fixture namespace directory")?;
    }
    raw_witness(&directory, operation, "current-thread namespace directory")?
        .require_kind(nix::libc::S_IFDIR, "current-thread namespace directory")?;
    Ok(directory)
}

pub(super) fn open_namespace(
    namespace_directory: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<(std::fs::File, NamespaceWitness)> {
    operation.charge_descriptor("opening fixed current-thread mount namespace")?;
    let namespace = if operation.is_production() {
        // Intentional procfs magic-link traversal below the exact retained and
        // authenticated current-thread `ns` directory.
        openat2_file_until(
            namespace_directory.as_raw_fd(),
            c"mnt",
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC,
            0,
            0,
            operation.deadline(),
        )?
    } else {
        openat2_file_until(
            namespace_directory.as_raw_fd(),
            c"mnt",
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
            operation.deadline(),
        )?
    };

    let witness = namespace_witness(&namespace, operation)?;
    Ok((namespace, witness))
}

pub(super) fn namespace_witness(
    namespace: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<NamespaceWitness> {
    operation.charge(4, "reserving typed namespace authentication work")?;
    if operation.is_production() {
        let identity = authenticate_mount_namespace_descriptor(namespace, Some(operation.deadline()))?;
        Ok(NamespaceWitness {
            device: identity.device,
            inode: identity.inode,
            kind: identity.kind,
            namespace_type: identity.namespace_type,
        })
    } else {
        let before = raw_witness(namespace, operation, "fixture mount-namespace marker")?
            .require_kind(nix::libc::S_IFREG, "fixture mount-namespace marker")?;
        #[cfg(test)]
        {
            reject_kernel_pseudo_fixture(namespace, operation, "fixture mount-namespace marker")?;
        }
        let after = raw_witness(namespace, operation, "revalidating fixture mount-namespace marker")?
            .require_kind(nix::libc::S_IFREG, "revalidated fixture mount-namespace marker")?;
        require_same_raw(before, after, "fixture mount-namespace marker authentication")?;
        Ok(NamespaceWitness {
            device: before.device,
            inode: before.inode,
            kind: before.kind,
            namespace_type: nix::libc::CLONE_NEWNS,
        })
    }
}

pub(super) fn open_task_root(
    thread: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<(std::fs::File, TaskRootWitness)> {
    operation.charge_descriptor("opening fixed current-thread task root")?;
    let task_root = if operation.is_production() {
        // Intentional procfs magic-link traversal below the exact retained and
        // authenticated current-thread directory. This resolves the task's
        // absolute-path root, not a purported global namespace root.
        openat2_file_until(
            thread.as_raw_fd(),
            c"root",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            0,
            0,
            operation.deadline(),
        )?
    } else {
        openat2_file_until(
            thread.as_raw_fd(),
            c"root",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
            operation.deadline(),
        )?
    };

    let witness = task_root_witness(&task_root, operation)?;
    Ok((task_root, witness))
}

pub(super) fn task_root_witness(
    task_root: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<TaskRootWitness> {
    mounted_directory_witness(task_root, operation, "current task root")
}

pub(super) fn mounted_directory_witness(
    directory: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<TaskRootWitness> {
    #[cfg(test)]
    if !operation.is_production() {
        reject_kernel_pseudo_fixture(directory, operation, "fixture mounted directory")?;
    }
    let before = raw_witness(directory, operation, action)?.require_kind(nix::libc::S_IFDIR, action)?;
    operation.charge_descriptors(
        DESCRIPTOR_MOUNT_ID_DESCRIPTOR_BOUND,
        "reserving authenticated mounted-directory mount-ID descriptor work",
    )?;
    operation.charge(
        DESCRIPTOR_MOUNT_ID_WORK_BOUND,
        "reserving authenticated mounted-directory mount-ID parser work",
    )?;
    let mount_id = if operation.is_production() {
        descriptor_mount_id_until(directory, operation.deadline())?
    } else {
        // Fixture mount identity is deliberately synthetic and procfs-free.
        // Replacement is still detected by the independent dev/inode fields.
        before.inode
    };
    let after = raw_witness(directory, operation, action)?.require_kind(nix::libc::S_IFDIR, action)?;
    require_same_raw(before, after, "mounted directory around mount-ID capture")?;
    if mount_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mounted directory has a zero mount ID",
        ));
    }
    Ok(TaskRootWitness {
        device: before.device,
        inode: before.inode,
        kind: before.kind,
        mount_id,
    })
}

pub(crate) fn authenticate_mount_namespace_descriptor(
    namespace: &std::fs::File,
    deadline: Option<Instant>,
) -> io::Result<AuthenticatedMountNamespaceIdentity> {
    let before =
        raw_namespace_witness(namespace, deadline)?.require_kind(nix::libc::S_IFREG, "mount-namespace descriptor")?;
    // SAFETY: zeroed statfs storage is a valid output buffer and namespace is
    // retained for the complete bounded fstatfs operation.
    let mut status: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(deadline, || {
        // SAFETY: status is writable and namespace is a live descriptor.
        if unsafe { nix::libc::fstatfs(namespace.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    retry_interrupted(deadline, || Ok(()))?;
    // Never issue a namespace ioctl on an arbitrary non-nsfs descriptor.
    validate_namespace_filesystem(status.f_type)?;
    let namespace_type = retry_interrupted(deadline, || {
        // SAFETY: NS_GET_NSTYPE accepts no third argument and returns the
        // clone flag for the namespace represented by the retained fd.
        let result = unsafe { nix::libc::ioctl(namespace.as_raw_fd(), NS_GET_NSTYPE) };
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    retry_interrupted(deadline, || Ok(()))?;
    validate_namespace_authentication(status.f_type, namespace_type)?;
    let after = raw_namespace_witness(namespace, deadline)?
        .require_kind(nix::libc::S_IFREG, "revalidated mount-namespace descriptor")?;
    require_same_raw(before, after, "mount-namespace descriptor authentication")?;
    Ok(AuthenticatedMountNamespaceIdentity {
        device: before.device,
        inode: before.inode,
        kind: before.kind,
        namespace_type,
    })
}

pub(super) fn validate_namespace_authentication(
    filesystem_magic: nix::libc::c_long,
    namespace_type: nix::libc::c_int,
) -> io::Result<()> {
    validate_namespace_filesystem(filesystem_magic)?;
    if namespace_type != nix::libc::CLONE_NEWNS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "namespace descriptor has type {namespace_type:#x}, expected mount namespace type {:#x}",
                nix::libc::CLONE_NEWNS
            ),
        ));
    }
    Ok(())
}

fn validate_namespace_filesystem(filesystem_magic: nix::libc::c_long) -> io::Result<()> {
    if filesystem_magic == NSFS_MAGIC {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("mount-namespace descriptor is not nsfs: expected {NSFS_MAGIC:#x}, found {filesystem_magic:#x}"),
        ))
    }
}

fn raw_namespace_witness(namespace: &std::fs::File, deadline: Option<Instant>) -> io::Result<RawWitness> {
    // SAFETY: zeroed stat storage is a valid fstat output buffer.
    let mut status: nix::libc::stat = unsafe { zeroed() };
    retry_interrupted(deadline, || {
        // SAFETY: status is writable and namespace remains a live descriptor.
        if unsafe { nix::libc::fstat(namespace.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    retry_interrupted(deadline, || Ok(()))?;
    let witness = RawWitness {
        device: status.st_dev,
        inode: status.st_ino,
        kind: status.st_mode & nix::libc::S_IFMT,
    };
    if witness.device == 0 || witness.inode == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mount-namespace descriptor has a zero device or inode identity",
        ));
    }
    Ok(witness)
}

fn raw_witness(file: &std::fs::File, operation: &mut Operation<'_>, action: &'static str) -> io::Result<RawWitness> {
    operation.charge(1, action)?;
    // SAFETY: zeroed stat storage is a valid fstat output buffer.
    let mut status: nix::libc::stat = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: status is writable and file remains a live descriptor.
        if unsafe { nix::libc::fstat(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    let witness = RawWitness {
        device: status.st_dev,
        inode: status.st_ino,
        kind: status.st_mode & nix::libc::S_IFMT,
    };
    if witness.device == 0 || witness.inode == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{action} has a zero device or inode identity"),
        ));
    }
    Ok(witness)
}

impl RawWitness {
    fn require_kind(self, expected: u32, context: &'static str) -> io::Result<Self> {
        if self.kind == expected {
            Ok(self)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{context} has inode kind {:#o}, expected {expected:#o}", self.kind),
            ))
        }
    }
}

pub(super) fn require_same_namespace(
    expected: NamespaceWitness,
    actual: NamespaceWitness,
    context: &'static str,
) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed nsfs device, inode, kind, or namespace type"),
        ))
    }
}

pub(super) fn require_same_task_root(
    expected: TaskRootWitness,
    actual: TaskRootWitness,
    context: &'static str,
) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed task-root device, inode, kind, or mount ID"),
        ))
    }
}

fn require_same_raw(expected: RawWitness, actual: RawWitness, context: &'static str) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed device, inode, or kind"),
        ))
    }
}

#[cfg(test)]
fn reject_kernel_pseudo_fixture(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    context: &'static str,
) -> io::Result<()> {
    operation.charge(1, "rejecting kernel pseudo-filesystem fixture")?;
    // SAFETY: zeroed statfs storage is a valid output buffer and file remains
    // retained for the bounded fstatfs call.
    let mut status: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: status is writable and file is a live descriptor.
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    if matches!(status.f_type, PROC_SUPER_MAGIC | SYSFS_MAGIC | NSFS_MAGIC) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{context} is procfs, sysfs, or nsfs; fixtures require an ordinary test-owned filesystem"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn open_controlled_directory(
    parent: &std::fs::File,
    name: &CStr,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<std::fs::File> {
    require_component(name, action)?;
    operation.charge_descriptor(action)?;
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    )
}

#[cfg(test)]
fn require_component(name: &CStr, context: &'static str) -> io::Result<()> {
    let bytes = name.to_bytes();
    if bytes.is_empty() || bytes.len() > 255 || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{context} is not one bounded path component"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn duplicate(file: &std::fs::File, operation: &mut Operation<'_>, action: &'static str) -> io::Result<std::fs::File> {
    operation.charge_descriptor(action)?;
    let descriptor = retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: fcntl duplicates the live descriptor on success.
        let result = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    // SAFETY: successful F_DUPFD_CLOEXEC returned a new owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

#[cfg(test)]
fn copy_cstring(value: &CStr, context: &'static str) -> io::Result<CString> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(value.to_bytes().len().saturating_add(1))
        .map_err(|source| io::Error::other(format!("could not allocate {context}: {source}")))?;
    bytes.extend_from_slice(value.to_bytes());
    CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{context} contains NUL")))
}
