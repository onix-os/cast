use std::{io, time::Instant};

/// Node kind reported by the retained descriptor observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) enum ObservedNodeKind {
    BlockDevice,
    Other,
}

/// Access authority retained by the observed descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) enum ObservedDeviceAccess {
    ReadOnly,
    WriteCapable,
}

/// One fixed-size observation of an already retained parent block device.
///
/// The future syscall adapter is responsible for deriving these fields from
/// one descriptor. This vocabulary contains no descriptor, path, buffer, or
/// operation that could reopen, read, or mutate the device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) struct BlockDeviceObservation {
    node_kind: ObservedNodeKind,
    access: ObservedDeviceAccess,
    containing_device: u64,
    inode: u64,
    mount_id: u64,
    block_major: u32,
    block_minor: u32,
    logical_block_size: u32,
    byte_length: u64,
}

impl BlockDeviceObservation {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::linux_fs) const fn new(
        node_kind: ObservedNodeKind,
        access: ObservedDeviceAccess,
        containing_device: u64,
        inode: u64,
        mount_id: u64,
        block_major: u32,
        block_minor: u32,
        logical_block_size: u32,
        byte_length: u64,
    ) -> Self {
        Self {
            node_kind,
            access,
            containing_device,
            inode,
            mount_id,
            block_major,
            block_minor,
            logical_block_size,
            byte_length,
        }
    }

    pub(super) const fn node_kind(self) -> ObservedNodeKind {
        self.node_kind
    }

    pub(super) const fn access(self) -> ObservedDeviceAccess {
        self.access
    }

    pub(super) const fn containing_device(self) -> u64 {
        self.containing_device
    }

    pub(super) const fn inode(self) -> u64 {
        self.inode
    }

    pub(super) const fn mount_id(self) -> u64 {
        self.mount_id
    }

    pub(super) const fn block_major(self) -> u32 {
        self.block_major
    }

    pub(super) const fn block_minor(self) -> u32 {
        self.block_minor
    }

    pub(super) const fn logical_block_size(self) -> u32 {
        self.logical_block_size
    }

    pub(super) const fn byte_length(self) -> u64 {
        self.byte_length
    }
}

/// Private-to-`linux_fs` seam for a later descriptor/syscall adapter.
///
/// Implementations must observe the same retained descriptor on every call.
/// The deadline bounds userspace and retry work but cannot preempt one kernel
/// call that is already blocked.
pub(in crate::linux_fs) trait BlockDeviceObserver {
    fn observe_until(&mut self, deadline: Instant) -> io::Result<BlockDeviceObservation>;
}
