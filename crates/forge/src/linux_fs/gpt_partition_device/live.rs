//! Retained-descriptor Linux block-device primitives.
//!
//! This layer never accepts a path and never opens or discovers a device. It
//! borrows one descriptor retained by its caller, observes it with one-shot
//! `fstat(2)`, `fcntl(F_GETFL)`, `BLKSSZGET`, and `BLKGETSIZE64` calls, and
//! returns the existing scalar-only observation vocabulary. A temporary image
//! can then borrow the same descriptor and the length authenticated by the most
//! recent successful observation for bounded positional reads by the private
//! GPT image seam.
//!
//! The UAPI constants and production syscalls are deliberately gated to
//! 64-bit Linux. Linux 5.6 has no `STATX_MNT_ID`, so this primitive consumes a
//! nonzero mount ID authenticated separately through the retained-descriptor
//! proc-fdinfo protocol; it performs no procfs lookup itself. The 5.6 baseline
//! also has no descriptor-bound disk-sequence query used here, so no disk
//! sequence is claimed. Deadlines are checked immediately before and after
//! every syscall. They bound userspace work, but cannot preempt a kernel call
//! that is already blocked.

mod abi;
mod authentication;
mod image;
mod observation;
mod syscalls;

pub(in crate::linux_fs) use authentication::{
    LiveAuthenticatedGptPartitionDeviceEvidence, authenticate_retained_gpt_partition_device_with_interpass_until,
};
pub(in crate::linux_fs) use image::RetainedReadOnlyBlockImage;
pub(in crate::linux_fs) use observation::RetainedBlockDeviceObserver;

#[cfg(test)]
pub(in crate::linux_fs) use abi::fixture_block_ioctl_requests;
#[cfg(test)]
pub(in crate::linux_fs) use authentication::authenticate_retained_gpt_partition_device_sources_fixture_with_interpass_until;
#[cfg(test)]
pub(in crate::linux_fs) use image::retained_read_only_block_image_fixture_until;
#[cfg(test)]
pub(in crate::linux_fs) use observation::{
    FixtureBlockDeviceSyscall, FixtureBlockDeviceSyscallResult, observe_retained_block_device_fixture_with_clock_until,
};
