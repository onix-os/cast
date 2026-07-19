//! Closed devtmpfs policy derived from one selected mountinfo attachment.
//!
//! This pure layer accepts no caller-authored path. It composes only with an
//! already selected attachment, requires that attachment to be exactly `/dev`
//! at filesystem root `/`, and retains scalar identity and policy facts. It
//! opens nothing and grants no path, descriptor, block-device, or mutation
//! authority. A later authenticated reader must still bind these facts to its
//! retained task root, mount namespace, and device descriptor.
//!
//! Linux mountinfo does not reliably preserve the original `bind`/`rbind`
//! operation token, and a whole-filesystem bind may still report root `/` and
//! type `devtmpfs`. Rejecting explicit bind tokens and subroots is therefore a
//! defense, not a standalone proof of non-bind provenance. The later retained
//! descriptor and mount-ID sandwiches remain mandatory.

use std::time::Instant;

use thiserror::Error;

use super::{mountinfo::MOUNTINFO_LIMITS, mountinfo_attachment::SelectedMountInfoAttachment};

const MAX_POLICY_OPTIONS: usize = MOUNTINFO_LIMITS.max_fields_per_line;
const OPTION_COMPARISON_WORK: usize = 40;
const MAX_POLICY_WORK: usize =
    5 * MOUNTINFO_LIMITS.max_field_bytes + 2 * MAX_POLICY_OPTIONS * OPTION_COMPARISON_WORK + 32;

#[derive(Clone, Copy, Debug)]
pub(super) struct DevtmpfsMountInfoPolicyLimits {
    pub(super) max_options: usize,
    pub(super) max_work: usize,
}

pub(super) const DEVTMPFS_MOUNTINFO_POLICY_LIMITS: DevtmpfsMountInfoPolicyLimits = DevtmpfsMountInfoPolicyLimits {
    max_options: MAX_POLICY_OPTIONS,
    max_work: MAX_POLICY_WORK,
};

/// Filesystem driver admitted for the authenticated device namespace.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DevtmpfsFilesystemKind {
    Devtmpfs,
}

/// Exact effective access state agreed by the mount and superblock domains.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DevtmpfsAccessMode {
    ReadOnly,
    ReadWrite,
}

/// The two option domains represented separately by mountinfo.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DevtmpfsMountOptionDomain {
    Mount,
    Superblock,
}

/// Scalar-only evidence for one exact `/dev` devtmpfs attachment.
//
// Construction is private. No selected record, path bytes, source string, or
// authority-bearing object can escape through this value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValidatedDevtmpfsMountInfoPolicy {
    filesystem: DevtmpfsFilesystemKind,
    access_mode: DevtmpfsAccessMode,
    mount_id: u64,
    device_major: u32,
    device_minor: u32,
}

impl ValidatedDevtmpfsMountInfoPolicy {
    pub(crate) const fn filesystem(self) -> DevtmpfsFilesystemKind {
        self.filesystem
    }

    pub(crate) const fn access_mode(self) -> DevtmpfsAccessMode {
        self.access_mode
    }

    pub(crate) const fn mount_id(self) -> u64 {
        self.mount_id
    }

    pub(crate) const fn device_major(self) -> u32 {
        self.device_major
    }

    pub(crate) const fn device_minor(self) -> u32 {
        self.device_minor
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum DevtmpfsMountInfoPolicyError {
    #[error("selected device attachment mount point is not exactly /dev")]
    UnexpectedMountPoint,
    #[error("selected device attachment root is not exactly filesystem root /")]
    UnexpectedMountRoot,
    #[error("selected device attachment filesystem type is not exactly devtmpfs")]
    UnsupportedFilesystem,
    #[error(
        "selected device attachment {domain:?} options contain rw {rw_count} times and ro {ro_count} times instead of exactly one access-mode token"
    )]
    InvalidAccessMode {
        domain: DevtmpfsMountOptionDomain,
        rw_count: usize,
        ro_count: usize,
    },
    #[error(
        "selected device attachment mount access mode {mount:?} does not equal superblock access mode {superblock:?}"
    )]
    AccessModeMismatch {
        mount: DevtmpfsAccessMode,
        superblock: DevtmpfsAccessMode,
    },
    #[error(
        "selected device attachment {domain:?} options contain bind {bind_count} times and rbind {rbind_count} times"
    )]
    BindSemantics {
        domain: DevtmpfsMountOptionDomain,
        bind_count: usize,
        rbind_count: usize,
    },
    #[error(
        "selected device attachment {domain:?} options contain dev {dev_count} times and nodev {nodev_count} times"
    )]
    InvalidDeviceSemantics {
        domain: DevtmpfsMountOptionDomain,
        dev_count: usize,
        nodev_count: usize,
    },
    #[error("selected device attachment {domain:?} options exceed the {limit} option limit")]
    OptionLimitExceeded {
        domain: DevtmpfsMountOptionDomain,
        limit: usize,
    },
    #[error("selected device attachment policy exceeds the {limit} unit work limit while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("selected device attachment policy limits are zero or exceed production ceilings")]
    InvalidLimits,
    #[error("selected device attachment policy exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
}

/// Validate one already selected attachment without accepting a path input.
pub(crate) fn validate_selected_devtmpfs_mount_policy_until(
    selected: SelectedMountInfoAttachment<'_>,
    deadline: Instant,
) -> Result<ValidatedDevtmpfsMountInfoPolicy, DevtmpfsMountInfoPolicyError> {
    let mut clock = Instant::now;
    validate_selected_devtmpfs_mount_policy_with_limits_and_clock(
        selected,
        DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut clock,
    )
    .map(|(policy, _work)| policy)
}

fn validate_selected_devtmpfs_mount_policy_with_limits_and_clock(
    selected: SelectedMountInfoAttachment<'_>,
    limits: DevtmpfsMountInfoPolicyLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(ValidatedDevtmpfsMountInfoPolicy, usize), DevtmpfsMountInfoPolicyError> {
    require_deadline(deadline, clock)?;
    validate_limits(limits)?;
    let mut budget = PolicyBudget::new(limits.max_work, deadline, clock);

    budget.charge_comparison(
        selected.mount_point().len(),
        b"/dev".len(),
        "checking the exact mount point",
    )?;
    if selected.mount_point() != b"/dev" {
        return Err(DevtmpfsMountInfoPolicyError::UnexpectedMountPoint);
    }

    budget.charge_comparison(selected.root().len(), b"/".len(), "checking the exact mount root")?;
    if selected.root() != b"/" {
        return Err(DevtmpfsMountInfoPolicyError::UnexpectedMountRoot);
    }

    let filesystem_type = selected.policy_filesystem_type();
    budget.charge_comparison(
        filesystem_type.len(),
        b"devtmpfs".len(),
        "checking the exact filesystem type",
    )?;
    if filesystem_type != b"devtmpfs" {
        return Err(DevtmpfsMountInfoPolicyError::UnsupportedFilesystem);
    }

    let mount = scan_options(
        selected.policy_mount_options(),
        DevtmpfsMountOptionDomain::Mount,
        limits.max_options,
        &mut budget,
    )?;
    let superblock = scan_options(
        selected.policy_super_options(),
        DevtmpfsMountOptionDomain::Superblock,
        limits.max_options,
        &mut budget,
    )?;

    let mount_mode = require_access_mode(DevtmpfsMountOptionDomain::Mount, mount.rw, mount.ro)?;
    let superblock_mode = require_access_mode(DevtmpfsMountOptionDomain::Superblock, superblock.rw, superblock.ro)?;
    if mount_mode != superblock_mode {
        return Err(DevtmpfsMountInfoPolicyError::AccessModeMismatch {
            mount: mount_mode,
            superblock: superblock_mode,
        });
    }
    require_no_bind(DevtmpfsMountOptionDomain::Mount, mount.bind, mount.rbind)?;
    require_no_bind(DevtmpfsMountOptionDomain::Superblock, superblock.bind, superblock.rbind)?;
    require_device_semantics(DevtmpfsMountOptionDomain::Mount, mount.dev, mount.nodev)?;
    require_device_semantics(DevtmpfsMountOptionDomain::Superblock, superblock.dev, superblock.nodev)?;

    budget.checkpoint()?;
    let work = budget.consumed();
    Ok((
        ValidatedDevtmpfsMountInfoPolicy {
            filesystem: DevtmpfsFilesystemKind::Devtmpfs,
            access_mode: mount_mode,
            mount_id: selected.mount_id(),
            device_major: selected.device_major(),
            device_minor: selected.device_minor(),
        },
        work,
    ))
}

#[derive(Default)]
struct OptionCounts {
    rw: usize,
    ro: usize,
    bind: usize,
    rbind: usize,
    dev: usize,
    nodev: usize,
}

fn scan_options<'a>(
    options: impl ExactSizeIterator<Item = &'a [u8]>,
    domain: DevtmpfsMountOptionDomain,
    max_options: usize,
    budget: &mut PolicyBudget<'_, impl FnMut() -> Instant>,
) -> Result<OptionCounts, DevtmpfsMountInfoPolicyError> {
    if options.len() > max_options {
        return Err(DevtmpfsMountInfoPolicyError::OptionLimitExceeded {
            domain,
            limit: max_options,
        });
    }
    let mut counts = OptionCounts::default();
    for option in options {
        budget.charge_with_overhead(option.len(), OPTION_COMPARISON_WORK, option_action(domain))?;
        let counter = match option {
            b"rw" => Some(&mut counts.rw),
            b"ro" => Some(&mut counts.ro),
            b"bind" => Some(&mut counts.bind),
            b"rbind" => Some(&mut counts.rbind),
            b"dev" => Some(&mut counts.dev),
            b"nodev" => Some(&mut counts.nodev),
            _ => None,
        };
        if let Some(counter) = counter {
            *counter = counter
                .checked_add(1)
                .ok_or(DevtmpfsMountInfoPolicyError::WorkLimitExceeded {
                    limit: budget.initial(),
                    action: option_action(domain),
                })?;
        }
    }
    Ok(counts)
}

const fn option_action(domain: DevtmpfsMountOptionDomain) -> &'static str {
    match domain {
        DevtmpfsMountOptionDomain::Mount => "checking per-mount policy options",
        DevtmpfsMountOptionDomain::Superblock => "checking superblock policy options",
    }
}

fn require_access_mode(
    domain: DevtmpfsMountOptionDomain,
    rw_count: usize,
    ro_count: usize,
) -> Result<DevtmpfsAccessMode, DevtmpfsMountInfoPolicyError> {
    match (rw_count, ro_count) {
        (1, 0) => Ok(DevtmpfsAccessMode::ReadWrite),
        (0, 1) => Ok(DevtmpfsAccessMode::ReadOnly),
        _ => Err(DevtmpfsMountInfoPolicyError::InvalidAccessMode {
            domain,
            rw_count,
            ro_count,
        }),
    }
}

fn require_no_bind(
    domain: DevtmpfsMountOptionDomain,
    bind_count: usize,
    rbind_count: usize,
) -> Result<(), DevtmpfsMountInfoPolicyError> {
    if bind_count == 0 && rbind_count == 0 {
        Ok(())
    } else {
        Err(DevtmpfsMountInfoPolicyError::BindSemantics {
            domain,
            bind_count,
            rbind_count,
        })
    }
}

fn require_device_semantics(
    domain: DevtmpfsMountOptionDomain,
    dev_count: usize,
    nodev_count: usize,
) -> Result<(), DevtmpfsMountInfoPolicyError> {
    if dev_count <= 1 && nodev_count == 0 {
        Ok(())
    } else {
        Err(DevtmpfsMountInfoPolicyError::InvalidDeviceSemantics {
            domain,
            dev_count,
            nodev_count,
        })
    }
}

fn validate_limits(limits: DevtmpfsMountInfoPolicyLimits) -> Result<(), DevtmpfsMountInfoPolicyError> {
    if limits.max_options == 0
        || limits.max_options > MAX_POLICY_OPTIONS
        || limits.max_work == 0
        || limits.max_work > MAX_POLICY_WORK
    {
        Err(DevtmpfsMountInfoPolicyError::InvalidLimits)
    } else {
        Ok(())
    }
}

struct PolicyBudget<'a, Clock> {
    remaining: usize,
    initial: usize,
    deadline: Instant,
    clock: &'a mut Clock,
}

impl<'a, Clock: FnMut() -> Instant> PolicyBudget<'a, Clock> {
    fn new(limit: usize, deadline: Instant, clock: &'a mut Clock) -> Self {
        Self {
            remaining: limit,
            initial: limit,
            deadline,
            clock,
        }
    }

    fn charge(&mut self, amount: usize, action: &'static str) -> Result<(), DevtmpfsMountInfoPolicyError> {
        self.checkpoint()?;
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or(DevtmpfsMountInfoPolicyError::WorkLimitExceeded {
                limit: self.initial,
                action,
            })?;
        self.checkpoint()
    }

    fn charge_with_overhead(
        &mut self,
        bytes: usize,
        overhead: usize,
        action: &'static str,
    ) -> Result<(), DevtmpfsMountInfoPolicyError> {
        let amount = bytes
            .checked_add(overhead)
            .ok_or(DevtmpfsMountInfoPolicyError::WorkLimitExceeded {
                limit: self.initial,
                action,
            })?;
        self.charge(amount, action)
    }

    fn charge_comparison(
        &mut self,
        actual_bytes: usize,
        expected_bytes: usize,
        action: &'static str,
    ) -> Result<(), DevtmpfsMountInfoPolicyError> {
        self.charge_with_overhead(actual_bytes, expected_bytes, action)
    }

    fn checkpoint(&mut self) -> Result<(), DevtmpfsMountInfoPolicyError> {
        require_deadline(self.deadline, self.clock)
    }

    const fn consumed(&self) -> usize {
        self.initial - self.remaining
    }

    const fn initial(&self) -> usize {
        self.initial
    }
}

fn require_deadline(
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(), DevtmpfsMountInfoPolicyError> {
    if clock() > deadline {
        Err(DevtmpfsMountInfoPolicyError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
    selected: SelectedMountInfoAttachment<'_>,
    limits: DevtmpfsMountInfoPolicyLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(ValidatedDevtmpfsMountInfoPolicy, usize), DevtmpfsMountInfoPolicyError> {
    validate_selected_devtmpfs_mount_policy_with_limits_and_clock(selected, limits, deadline, clock)
}
