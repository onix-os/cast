mod anchored_root;
mod pseudo_filesystems;
mod syscalls;

use std::os::fd::OwnedFd;
use std::path::PathBuf;

pub(super) use anchored_root::{
    AnchoredMountTargetKind, PinnedAnchoredBindSource, descriptor_target_kind, normalized_anchored_mount_target,
    pin_anchored_bind_sources, setup,
};
#[cfg(test)]
pub(super) use anchored_root::{
    PreparedAnchoredMount, descriptor_stat, open_anchored_mount_target, validate_anchored_mount_topology,
};
#[cfg(test)]
pub(super) use pseudo_filesystems::set_mount_access;
#[cfg(test)]
pub(super) use pseudo_filesystems::{
    PseudoMountDecision, RootMountDecision, TMPFS_MAGIC, TmpfsLimitReadback, open_anchored_resolver_target,
    prepare_pseudo_mount_targets, pseudo_mount_decisions, reopen_pinned_readonly, resolver_stat_stable,
    root_mount_decisions, sealed_resolver_file, validate_minimal_device_source, validate_resolver_target,
    validate_tmpfs_limit_readback, verify_tmpfs_limits,
};
#[cfg(test)]
pub(super) use syscalls::prepare_bind_target;

pub(super) struct Bind {
    pub(super) source: BindSource,
    pub(super) target: PathBuf,
    pub(super) read_only: bool,
}

pub(super) enum BindSource {
    /// Legacy pathname bind. Deliberately rejected by anchored execution.
    Path(PathBuf),
    /// Normalized path resolved beneath the authenticated root descriptor.
    RootRelative(PathBuf),
    /// Descriptor selected by the supervising runtime before activation.
    Pinned { descriptor: OwnedFd, label: PathBuf },
}
