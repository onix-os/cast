use std::{
    ffi::{CStr, CString},
    io,
    mem::zeroed,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    time::Instant,
};

use super::super::{
    authenticated_current_thread_procfs_with_deadline, controlled_resolution, openat2_file_until,
    parse_descriptor_mount_id, read_to_end_bounded_until, require_procfs_with_deadline, require_sysfs_until,
    retry_interrupted,
};

#[cfg(test)]
use super::super::{PROC_SUPER_MAGIC, SYSFS_MAGIC};

const MAX_PROC_FDINFO_BYTES: usize = 16 * 1024;
const AUTHENTICATED_THREAD_PROC_DESCRIPTOR_BOUND: usize = 5;

#[derive(Debug, Clone, Copy)]
pub(super) struct SysfsIdentityLimits {
    pub(super) max_work: usize,
    pub(super) max_ancestors: usize,
    pub(super) max_descriptors: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CaptureNode {
    Partition,
    Parent,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CaptureAttribute {
    Dev,
    Partition,
    Uevent,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CaptureCheckpoint {
    RootRebind,
    LookupPinned,
    LookupRebound,
    TargetPinned,
    AttributePinned {
        node: CaptureNode,
        attribute: CaptureAttribute,
    },
    AttributeRead {
        node: CaptureNode,
        attribute: CaptureAttribute,
    },
    AttributeRebound {
        node: CaptureNode,
        attribute: CaptureAttribute,
    },
    SubsystemPinned {
        depth: usize,
    },
    SubsystemRead {
        depth: usize,
    },
    SubsystemRebound {
        depth: usize,
    },
    AncestorExamined {
        depth: usize,
    },
    ParentSelected {
        depth: usize,
    },
    TerminalRebind,
    FinalNameRebind,
}

pub(super) struct Operation<'a> {
    deadline: Instant,
    remaining_work: usize,
    initial_work: usize,
    remaining_descriptors: usize,
    initial_descriptors: usize,
    pub(super) max_ancestors: usize,
    #[cfg(test)]
    hook: Option<&'a mut dyn FnMut(super::FixtureCheckpoint) -> io::Result<()>>,
    #[cfg(test)]
    clock: Option<&'a mut dyn FnMut() -> Instant>,
    #[cfg(not(test))]
    _lifetime: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> Operation<'a> {
    pub(super) fn production(limits: SysfsIdentityLimits, deadline: Instant) -> Self {
        Self::new(limits, deadline)
    }

    fn new(limits: SysfsIdentityLimits, deadline: Instant) -> Self {
        Self {
            deadline,
            remaining_work: limits.max_work,
            initial_work: limits.max_work,
            remaining_descriptors: limits.max_descriptors,
            initial_descriptors: limits.max_descriptors,
            max_ancestors: limits.max_ancestors,
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
        limits: SysfsIdentityLimits,
        deadline: Instant,
        hook: &'a mut impl FnMut(super::FixtureCheckpoint) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::require_fixture_limits(limits)?;
        let mut operation = Self::new(limits, deadline);
        operation.hook = Some(hook);
        operation.checkpoint()?;
        Ok(operation)
    }

    #[cfg(test)]
    pub(super) fn fixture_with_clock(
        limits: SysfsIdentityLimits,
        deadline: Instant,
        hook: &'a mut impl FnMut(super::FixtureCheckpoint) -> io::Result<()>,
        clock: &'a mut impl FnMut() -> Instant,
    ) -> io::Result<Self> {
        Self::require_fixture_limits(limits)?;
        let mut operation = Self::new(limits, deadline);
        operation.hook = Some(hook);
        operation.clock = Some(clock);
        operation.checkpoint()?;
        Ok(operation)
    }

    #[cfg(test)]
    pub(super) fn fixture_without_hook(limits: SysfsIdentityLimits, deadline: Instant) -> io::Result<Self> {
        Self::require_fixture_limits(limits)?;
        let mut operation = Self::new(limits, deadline);
        operation.checkpoint()?;
        Ok(operation)
    }

    #[cfg(test)]
    fn require_fixture_limits(limits: SysfsIdentityLimits) -> io::Result<()> {
        if limits.max_work == 0 || limits.max_ancestors == 0 || limits.max_descriptors == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sysfs fixture limits must all be nonzero",
            ));
        }
        Ok(())
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
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
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sysfs identity operation exceeded its deadline",
            ));
        }
        Ok(())
    }

    pub(super) fn charge(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.checkpoint()?;
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "sysfs identity exceeded its {} unit work limit while {action}",
                    self.initial_work
                ),
            )
        })?;
        self.checkpoint()
    }

    fn charge_descriptor(&mut self, action: &'static str) -> io::Result<()> {
        self.charge_descriptors(1, action)
    }

    fn charge_descriptors(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.charge(amount, action)?;
        self.remaining_descriptors = self.remaining_descriptors.checked_sub(amount).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "sysfs identity exceeded its {} descriptor-open limit while {action}",
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
}

#[cfg(test)]
impl From<CaptureNode> for super::FixtureNode {
    fn from(node: CaptureNode) -> Self {
        match node {
            CaptureNode::Partition => Self::Partition,
            CaptureNode::Parent => Self::Parent,
        }
    }
}

#[cfg(test)]
impl From<CaptureAttribute> for super::FixtureAttribute {
    fn from(attribute: CaptureAttribute) -> Self {
        match attribute {
            CaptureAttribute::Dev => Self::Dev,
            CaptureAttribute::Partition => Self::Partition,
            CaptureAttribute::Uevent => Self::Uevent,
        }
    }
}

#[cfg(test)]
impl From<CaptureCheckpoint> for super::FixtureCheckpoint {
    fn from(checkpoint: CaptureCheckpoint) -> Self {
        match checkpoint {
            CaptureCheckpoint::RootRebind => Self::RootRebind,
            CaptureCheckpoint::LookupPinned => Self::LookupPinned,
            CaptureCheckpoint::LookupRebound => Self::LookupRebound,
            CaptureCheckpoint::TargetPinned => Self::TargetPinned,
            CaptureCheckpoint::AttributePinned { node, attribute } => Self::AttributePinned {
                node: node.into(),
                attribute: attribute.into(),
            },
            CaptureCheckpoint::AttributeRead { node, attribute } => Self::AttributeRead {
                node: node.into(),
                attribute: attribute.into(),
            },
            CaptureCheckpoint::AttributeRebound { node, attribute } => Self::AttributeRebound {
                node: node.into(),
                attribute: attribute.into(),
            },
            CaptureCheckpoint::SubsystemPinned { depth } => Self::SubsystemPinned { depth },
            CaptureCheckpoint::SubsystemRead { depth } => Self::SubsystemRead { depth },
            CaptureCheckpoint::SubsystemRebound { depth } => Self::SubsystemRebound { depth },
            CaptureCheckpoint::AncestorExamined { depth } => Self::AncestorExamined { depth },
            CaptureCheckpoint::ParentSelected { depth } => Self::ParentSelected { depth },
            CaptureCheckpoint::TerminalRebind => Self::TerminalRebind,
            CaptureCheckpoint::FinalNameRebind => Self::FinalNameRebind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileWitness {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) kind: u32,
    pub(super) mount_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawFileWitness {
    device: u64,
    inode: u64,
    kind: u32,
}

impl FileWitness {
    pub(super) fn require_kind(self, expected: u32, context: &'static str) -> io::Result<Self> {
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

enum RootLocator {
    Production,
    #[cfg(test)]
    Fixture {
        parent: std::fs::File,
        name: CString,
    },
}

pub(super) struct RootHandle {
    file: std::fs::File,
    witness: FileWitness,
    locator: RootLocator,
}

impl RootHandle {
    pub(super) fn open_production(operation: &mut Operation<'_>) -> io::Result<Self> {
        let file = open_absolute_root(operation)?;
        operation.charge(1, "authenticating fixed sysfs root filesystem")?;
        require_sysfs_until(&file, std::path::Path::new("/sys"), operation.deadline())?;
        let witness = witness(&file, operation, "sysfs root")?.require_kind(nix::libc::S_IFDIR, "sysfs root")?;
        Ok(Self {
            file,
            witness,
            locator: RootLocator::Production,
        })
    }

    #[cfg(test)]
    pub(super) fn admit_fixture(
        parent: std::fs::File,
        name: CString,
        operation: &mut Operation<'_>,
    ) -> io::Result<Self> {
        require_component(&name, "fixture sysfs root")?;
        reject_kernel_pseudo_fixture(&parent, operation, "fixture sysfs parent")?;
        witness(&parent, operation, "fixture sysfs parent")?
            .require_kind(nix::libc::S_IFDIR, "fixture sysfs parent")?;
        let file = open_directory(&parent, &name, operation, "fixture sysfs root")?;
        reject_kernel_pseudo_fixture(&file, operation, "fixture sysfs root")?;
        let witness =
            witness(&file, operation, "fixture sysfs root")?.require_kind(nix::libc::S_IFDIR, "fixture sysfs root")?;
        Ok(Self {
            file,
            witness,
            locator: RootLocator::Fixture { parent, name },
        })
    }

    pub(super) fn require_named(&self, operation: &mut Operation<'_>) -> io::Result<()> {
        operation.emit(CaptureCheckpoint::RootRebind)?;
        let named = match &self.locator {
            RootLocator::Production => {
                let named = open_absolute_root(operation)?;
                operation.charge(1, "authenticating rebound sysfs root filesystem")?;
                require_sysfs_until(&named, std::path::Path::new("/sys"), operation.deadline())?;
                named
            }
            #[cfg(test)]
            RootLocator::Fixture { parent, name } => {
                open_directory(parent, name, operation, "fixture sysfs root rebind")?
            }
        };
        require_same(
            self.witness,
            witness(&named, operation, "rebound sysfs root")?,
            "sysfs root",
        )?;
        require_same(
            self.witness,
            witness(&self.file, operation, "retained sysfs root")?,
            "retained sysfs root",
        )
    }

    pub(super) fn reopen_owned(&self, operation: &mut Operation<'_>) -> io::Result<Self> {
        match &self.locator {
            RootLocator::Production => Self::open_production(operation),
            #[cfg(test)]
            RootLocator::Fixture { parent, name } => {
                let owned_parent = duplicate(parent, operation, "fixture sysfs parent")?;
                Self::admit_fixture(owned_parent, copy_cstring(name, "fixture sysfs root name")?, operation)
            }
        }
    }

    pub(super) const fn file(&self) -> &std::fs::File {
        &self.file
    }

    pub(super) const fn witness(&self) -> FileWitness {
        self.witness
    }

    pub(super) fn require_descendant(&self, child: FileWitness, context: &'static str) -> io::Result<()> {
        if child.mount_id != self.witness.mount_id || child.device != self.witness.device {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{context} crossed the authenticated sysfs mount"),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
fn reject_kernel_pseudo_fixture(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    context: &'static str,
) -> io::Result<()> {
    operation.charge(1, "rejecting real sysfs from synthetic fixture admission")?;
    // SAFETY: zeroed statfs storage is a valid output buffer and `file`
    // remains a live descriptor for the duration of fstatfs.
    let mut status: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: status is writable and the retained descriptor is live.
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    if status.f_type == SYSFS_MAGIC || status.f_type == PROC_SUPER_MAGIC {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{context} is a real kernel pseudo-filesystem; fixtures require an ordinary test-owned directory"),
        ))
    } else {
        Ok(())
    }
}

fn open_absolute_root(operation: &mut Operation<'_>) -> io::Result<std::fs::File> {
    operation.charge_descriptor("opening fixed sysfs root")?;
    openat2_file_until(
        nix::libc::AT_FDCWD,
        c"/sys",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        operation.deadline(),
    )
}

pub(super) fn open_directory(
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

pub(super) fn pin_symlink(
    parent: &std::fs::File,
    name: &CStr,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<(std::fs::File, FileWitness)> {
    require_component(name, action)?;
    operation.charge_descriptor(action)?;
    let file = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    let evidence = witness(&file, operation, action)?.require_kind(nix::libc::S_IFLNK, action)?;
    Ok((file, evidence))
}

pub(super) fn read_pinned_symlink(
    file: &std::fs::File,
    max_bytes: usize,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<Vec<u8>> {
    operation.charge(max_bytes.saturating_add(1), action)?;
    let buffer_len = max_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sysfs symlink read bound overflowed"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(buffer_len)
        .map_err(|source| io::Error::other(format!("could not allocate bounded sysfs link buffer: {source}")))?;
    bytes.resize(buffer_len, 0);
    let length = retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: `file` and the empty C string are live; the vector exposes a
        // writable allocation exactly `buffer_len` bytes long.
        let result =
            unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
        if result >= 0 {
            usize::try_from(result).map_err(|_| io::Error::other("readlinkat returned invalid length"))
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    if length > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{action} exceeds its {max_bytes} byte limit"),
        ));
    }
    bytes.truncate(length);
    Ok(bytes)
}

pub(super) struct AttributeEvidence {
    pub(super) witness: FileWitness,
    pub(super) bytes: Vec<u8>,
}

impl PartialEq for AttributeEvidence {
    fn eq(&self, other: &Self) -> bool {
        self.witness == other.witness && self.bytes == other.bytes
    }
}

impl Eq for AttributeEvidence {}

pub(super) fn read_attribute(
    root: &RootHandle,
    node: &std::fs::File,
    name: &CStr,
    max_bytes: usize,
    capture_node: CaptureNode,
    capture_attribute: CaptureAttribute,
    operation: &mut Operation<'_>,
) -> io::Result<AttributeEvidence> {
    let pinned = pin_regular(node, name, operation, "pinning sysfs attribute")?;
    root.require_descendant(pinned.1, "sysfs attribute")?;
    operation.emit(CaptureCheckpoint::AttributePinned {
        node: capture_node,
        attribute: capture_attribute,
    })?;

    operation.charge_descriptor("opening sysfs attribute for bounded read")?;
    let mut readable = openat2_file_until(
        node.as_raw_fd(),
        name,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    require_same(
        pinned.1,
        witness(&readable, operation, "readable sysfs attribute")?,
        "readable sysfs attribute",
    )?;
    let sentinel = max_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sysfs attribute read bound overflowed"))?;
    operation.charge(sentinel, "reading bounded sysfs attribute")?;
    let bytes = read_to_end_bounded_until(&mut readable, sentinel, operation.deadline())?;
    operation.emit(CaptureCheckpoint::AttributeRead {
        node: capture_node,
        attribute: capture_attribute,
    })?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("sysfs attribute exceeds its {max_bytes} byte ceiling"),
        ));
    }

    operation.emit(CaptureCheckpoint::AttributeRebound {
        node: capture_node,
        attribute: capture_attribute,
    })?;
    let rebound = pin_regular(node, name, operation, "rebinding sysfs attribute")?;
    require_same(pinned.1, rebound.1, "rebound sysfs attribute")?;
    require_same(
        pinned.1,
        witness(&pinned.0, operation, "retained sysfs attribute")?,
        "retained sysfs attribute",
    )?;
    Ok(AttributeEvidence {
        witness: pinned.1,
        bytes,
    })
}

fn pin_regular(
    parent: &std::fs::File,
    name: &CStr,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<(std::fs::File, FileWitness)> {
    require_component(name, action)?;
    operation.charge_descriptor(action)?;
    let file = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    let evidence = witness(&file, operation, action)?.require_kind(nix::libc::S_IFREG, action)?;
    Ok((file, evidence))
}

pub(super) fn attribute_absent(parent: &std::fs::File, name: &CStr, operation: &mut Operation<'_>) -> io::Result<()> {
    require_component(name, "checking absent sysfs attribute")?;
    operation.charge_descriptor("checking absent sysfs attribute")?;
    match openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Err(source) => Err(source),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "whole-disk sysfs parent unexpectedly has a partition attribute",
        )),
    }
}

pub(super) fn witness(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<FileWitness> {
    let raw = raw_witness(file, operation, action)?;
    let mount_id = descriptor_mount_id(file, raw, operation)?;
    operation.checkpoint()?;
    Ok(FileWitness {
        device: raw.device,
        inode: raw.inode,
        kind: raw.kind,
        mount_id,
    })
}

fn raw_witness(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<RawFileWitness> {
    operation.charge(1, action)?;
    // SAFETY: zero initializes valid output storage for fstat.
    let mut status: nix::libc::stat = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: the descriptor and writable status buffer remain live.
        if unsafe { nix::libc::fstat(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    Ok(RawFileWitness {
        device: status.st_dev,
        inode: status.st_ino,
        kind: status.st_mode & nix::libc::S_IFMT,
    })
}

fn descriptor_mount_id(
    file: &std::fs::File,
    expected: RawFileWitness,
    operation: &mut Operation<'_>,
) -> io::Result<u64> {
    // The shared helper opens exactly the authenticated proc root, current
    // process, task directory, and current thread. Reserve their bounded
    // descriptor cost before entering it so the enclosing budget remains
    // global even though those opens are encapsulated in the proc primitive.
    operation.charge_descriptors(
        AUTHENTICATED_THREAD_PROC_DESCRIPTOR_BOUND,
        "authenticating current-thread procfs",
    )?;
    operation.charge(
        64,
        "reserving encapsulated current-thread procfs syscall and parser work",
    )?;
    let thread = authenticated_current_thread_procfs_with_deadline(Some(operation.deadline()))?;

    let descriptors = open_proc_directory(&thread, c"fd", operation, "opening current-thread proc fd directory")?;
    let descriptor = descriptor_component(file.as_raw_fd())?;
    let before_alias = open_proc_descriptor_alias(&descriptors, &descriptor, operation)?;
    require_same_raw(
        expected,
        raw_witness(&before_alias, operation, "authenticating proc descriptor alias")?,
        "proc descriptor alias",
    )?;

    let fdinfo = open_proc_directory(
        &thread,
        c"fdinfo",
        operation,
        "opening current-thread proc fdinfo directory",
    )?;
    operation.charge_descriptor("opening current-thread proc fdinfo entry")?;
    let mut entry = openat2_file_until(
        fdinfo.as_raw_fd(),
        &descriptor,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    operation.charge(1, "authenticating proc fdinfo entry filesystem")?;
    require_procfs_with_deadline(
        &entry,
        std::path::Path::new("/proc/<pid>/task/<tid>/fdinfo/<fd>"),
        Some(operation.deadline()),
    )?;
    raw_witness(&entry, operation, "authenticated proc fdinfo entry")?
        .require_kind(nix::libc::S_IFREG, "authenticated proc fdinfo entry")?;
    operation.charge(
        MAX_PROC_FDINFO_BYTES.saturating_add(1),
        "reading bounded proc fdinfo mount ID",
    )?;
    let bytes = read_to_end_bounded_until(
        &mut entry,
        MAX_PROC_FDINFO_BYTES.saturating_add(1),
        operation.deadline(),
    )?;
    operation.charge(
        MAX_PROC_FDINFO_BYTES.saturating_add(1),
        "reserving bounded proc fdinfo mount-ID parser work",
    )?;
    let mount_id = parse_descriptor_mount_id(&bytes)?;
    operation.checkpoint()?;

    let after_alias = open_proc_descriptor_alias(&descriptors, &descriptor, operation)?;
    require_same_raw(
        expected,
        raw_witness(&after_alias, operation, "revalidating proc descriptor alias")?,
        "revalidated proc descriptor alias",
    )?;
    require_same_raw(
        expected,
        raw_witness(file, operation, "revalidating retained descriptor")?,
        "retained descriptor around mount-ID read",
    )?;
    Ok(mount_id)
}

impl RawFileWitness {
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

fn open_proc_directory(
    parent: &std::fs::File,
    name: &CStr,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<std::fs::File> {
    operation.charge_descriptor(action)?;
    let directory = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    operation.charge(1, "authenticating proc directory filesystem")?;
    require_procfs_with_deadline(
        &directory,
        std::path::Path::new("/proc/<pid>/task/<tid>/<component>"),
        Some(operation.deadline()),
    )?;
    Ok(directory)
}

fn open_proc_descriptor_alias(
    descriptors: &std::fs::File,
    descriptor: &CStr,
    operation: &mut Operation<'_>,
) -> io::Result<std::fs::File> {
    operation.charge_descriptor("opening authenticated proc descriptor alias")?;
    // This is the sole intentional magic-link traversal. Its retained procfs
    // parent is authenticated above and raw-fstat sandwiches bind the alias
    // back to the exact descriptor supplied by the caller.
    openat2_file_until(
        descriptors.as_raw_fd(),
        descriptor,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
        0,
        operation.deadline(),
    )
}

fn descriptor_component(descriptor: i32) -> io::Result<CString> {
    let value = u32::try_from(descriptor)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "retained descriptor number is negative"))?;
    let mut reverse = [0_u8; 10];
    let mut remaining = value;
    let mut count = 0usize;
    loop {
        reverse[count] = b'0'
            + u8::try_from(remaining % 10)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "descriptor digit exceeds u8"))?;
        count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(count.saturating_add(1)).map_err(|source| {
        io::Error::other(format!(
            "could not allocate bounded proc descriptor component: {source}"
        ))
    })?;
    bytes.extend(reverse[..count].iter().rev());
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "bounded proc descriptor component unexpectedly contains NUL",
        )
    })
}

fn require_same_raw(expected: RawFileWitness, actual: RawFileWitness, context: &'static str) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed retained inode, kind, or device"),
        ))
    }
}

pub(super) fn require_same(expected: FileWitness, actual: FileWitness, context: &'static str) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed retained inode, kind, device, or mount ID"),
        ))
    }
}

fn duplicate(file: &std::fs::File, operation: &mut Operation<'_>, action: &'static str) -> io::Result<std::fs::File> {
    operation.charge_descriptor(action)?;
    let descriptor = retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: fcntl duplicates the live descriptor without accessing a
        // pathname; success returns a fresh owned descriptor.
        let result = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    // SAFETY: successful F_DUPFD_CLOEXEC returned a fresh descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(owned))
}

fn require_component(component: &CStr, context: &'static str) -> io::Result<()> {
    let bytes = component.to_bytes();
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
fn copy_cstring(value: &CStr, context: &'static str) -> io::Result<CString> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(value.to_bytes().len().saturating_add(1))
        .map_err(|source| io::Error::other(format!("could not allocate {context}: {source}")))?;
    bytes.extend_from_slice(value.to_bytes());
    CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{context} contains NUL")))
}
