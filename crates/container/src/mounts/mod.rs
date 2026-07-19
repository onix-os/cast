mod anchored_identity;
mod anchored_root;
mod pseudo_filesystems;
mod syscalls;

use std::path::PathBuf;

pub use anchored_identity::{AnchoredLocator, AnchoredLocatorComponent, AnchoredLocatorError};
pub(super) use anchored_root::{
    AnchoredMountTargetKind, ReboundAnchoredBindSource, ReboundAnchoredInputs, authenticate_anchored_inputs,
    normalized_anchored_mount_target, setup,
};
#[cfg(test)]
pub(super) use anchored_root::{
    PreparedAnchoredMount, descriptor_stat, open_anchored_mount_target, validate_anchored_bind_inputs,
    validate_anchored_mount_topology,
};
#[cfg(test)]
pub(super) use pseudo_filesystems::set_mount_access;
#[cfg(test)]
pub(super) use pseudo_filesystems::{
    PseudoMountDecision, RootMountDecision, TMPFS_MAGIC, TmpfsLimitReadback, open_anchored_resolver_target,
    prepare_pseudo_mount_targets, pseudo_mount_decisions, resolver_stat_stable, root_mount_decisions,
    sealed_resolver_file, validate_minimal_device_source, validate_resolver_target, validate_tmpfs_limit_readback,
    verify_tmpfs_limits,
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
    /// Locator authenticated by the supervisor and reopened by the child.
    Anchored(AnchoredLocator),
}
