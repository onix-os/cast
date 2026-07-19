//! Closed boot-filesystem policy derived from one selected mountinfo record.
//!
//! The generic attachment selector remains identity-only. This layer consumes
//! its private selected record under the same caller-owned deadline and retains
//! only the exact policy facts required before a future BLS publisher may
//! proceed. It opens nothing and grants no descriptor or mutation authority.

use std::time::Instant;

use thiserror::Error;

use super::{mountinfo::MOUNTINFO_LIMITS, mountinfo_attachment::SelectedMountInfoAttachment};

const MAX_POLICY_OPTIONS: usize = MOUNTINFO_LIMITS.max_fields_per_line;
const OPTION_COMPARISON_WORK: usize = 64;
const MAX_POLICY_WORK: usize =
    3 * MOUNTINFO_LIMITS.max_field_bytes + OPTION_COMPARISON_WORK * MOUNTINFO_LIMITS.max_fields_per_line + 16;

#[derive(Clone, Copy, Debug)]
pub(super) struct BootMountInfoPolicyLimits {
    pub(super) max_options: usize,
    pub(super) max_work: usize,
}

pub(super) const BOOT_MOUNTINFO_POLICY_LIMITS: BootMountInfoPolicyLimits = BootMountInfoPolicyLimits {
    max_options: MAX_POLICY_OPTIONS,
    max_work: MAX_POLICY_WORK,
};

/// Filesystem driver names admitted by the boot publication policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BootFilesystemKind {
    Vfat,
}

/// The two option domains represented separately by mountinfo.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MountOptionDomain {
    Mount,
    Superblock,
}

/// Per-mount safety flags required by the current BLS contract.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RequiredBootMountFlag {
    Nosuid,
    Nodev,
    Noexec,
    Nosymfollow,
}

/// Closed, scalar-only evidence retained by mounted boot topology.
///
/// Construction is private: every successful instance represents exact
/// `vfat`, writable mount and superblock domains, and every required per-mount
/// safety flag. The separate booleans preserve which predicates were observed
/// without retaining arbitrary kernel strings.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValidatedBootMountInfoPolicy {
    filesystem: BootFilesystemKind,
    mount_read_write: bool,
    superblock_read_write: bool,
    nosuid: bool,
    nodev: bool,
    noexec: bool,
    nosymfollow: bool,
}

impl ValidatedBootMountInfoPolicy {
    pub(crate) const fn filesystem(self) -> BootFilesystemKind {
        self.filesystem
    }

    pub(crate) const fn mount_read_write(self) -> bool {
        self.mount_read_write
    }

    pub(crate) const fn superblock_read_write(self) -> bool {
        self.superblock_read_write
    }

    pub(crate) const fn nosuid(self) -> bool {
        self.nosuid
    }

    pub(crate) const fn nodev(self) -> bool {
        self.nodev
    }

    pub(crate) const fn noexec(self) -> bool {
        self.noexec
    }

    pub(crate) const fn nosymfollow(self) -> bool {
        self.nosymfollow
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum BootMountInfoPolicyError {
    #[error("selected boot mount filesystem type is not exactly vfat")]
    UnsupportedFilesystem,
    #[error(
        "selected boot mount {domain:?} options contain rw {rw_count} times and ro {ro_count} times instead of exactly one rw and no ro"
    )]
    InvalidReadWriteState {
        domain: MountOptionDomain,
        rw_count: usize,
        ro_count: usize,
    },
    #[error(
        "selected boot mount options contain required {flag:?} {required_count} times and its inverse {inverse_count} times"
    )]
    InvalidSecurityFlagState {
        flag: RequiredBootMountFlag,
        required_count: usize,
        inverse_count: usize,
    },
    #[error("selected boot mount {domain:?} options exceed the {limit} option limit")]
    OptionLimitExceeded { domain: MountOptionDomain, limit: usize },
    #[error("selected boot mount policy exceeds the {limit} unit work limit while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("selected boot mount policy limits must be nonzero")]
    InvalidLimits,
    #[error("selected boot mount policy exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
}

/// Validate the selected record without replacing the caller's deadline.
pub(crate) fn validate_selected_boot_mount_policy_until(
    selected: SelectedMountInfoAttachment<'_>,
    deadline: Instant,
) -> Result<ValidatedBootMountInfoPolicy, BootMountInfoPolicyError> {
    let mut clock = Instant::now;
    validate_selected_boot_mount_policy_with_limits_and_clock(
        selected,
        BOOT_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut clock,
    )
    .map(|(policy, _work)| policy)
}

fn validate_selected_boot_mount_policy_with_limits_and_clock(
    selected: SelectedMountInfoAttachment<'_>,
    limits: BootMountInfoPolicyLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(ValidatedBootMountInfoPolicy, usize), BootMountInfoPolicyError> {
    require_deadline(deadline, clock)?;
    if limits.max_options == 0 || limits.max_work == 0 {
        return Err(BootMountInfoPolicyError::InvalidLimits);
    }
    let mut budget = PolicyBudget::new(limits.max_work, deadline, clock);

    let filesystem_type = selected.policy_filesystem_type();
    budget.charge_with_overhead(filesystem_type.len(), b"vfat".len(), "checking the filesystem type")?;
    if filesystem_type != b"vfat" {
        return Err(BootMountInfoPolicyError::UnsupportedFilesystem);
    }

    let mount = scan_options(
        selected.policy_mount_options(),
        MountOptionDomain::Mount,
        limits.max_options,
        &mut budget,
    )?;
    let superblock = scan_options(
        selected.policy_super_options(),
        MountOptionDomain::Superblock,
        limits.max_options,
        &mut budget,
    )?;

    require_read_write(MountOptionDomain::Mount, mount.rw, mount.ro)?;
    require_read_write(MountOptionDomain::Superblock, superblock.rw, superblock.ro)?;
    require_flag(RequiredBootMountFlag::Nosuid, mount.nosuid, mount.suid)?;
    require_flag(RequiredBootMountFlag::Nodev, mount.nodev, mount.dev)?;
    require_flag(RequiredBootMountFlag::Noexec, mount.noexec, mount.exec)?;
    require_flag(RequiredBootMountFlag::Nosymfollow, mount.nosymfollow, mount.symfollow)?;

    budget.checkpoint()?;
    let work = budget.consumed();
    Ok((
        ValidatedBootMountInfoPolicy {
            filesystem: BootFilesystemKind::Vfat,
            mount_read_write: true,
            superblock_read_write: true,
            nosuid: true,
            nodev: true,
            noexec: true,
            nosymfollow: true,
        },
        work,
    ))
}

#[derive(Default)]
struct OptionCounts {
    rw: usize,
    ro: usize,
    nosuid: usize,
    suid: usize,
    nodev: usize,
    dev: usize,
    noexec: usize,
    exec: usize,
    nosymfollow: usize,
    symfollow: usize,
}

fn scan_options<'a>(
    options: impl ExactSizeIterator<Item = &'a [u8]>,
    domain: MountOptionDomain,
    max_options: usize,
    budget: &mut PolicyBudget<'_, impl FnMut() -> Instant>,
) -> Result<OptionCounts, BootMountInfoPolicyError> {
    if options.len() > max_options {
        return Err(BootMountInfoPolicyError::OptionLimitExceeded {
            domain,
            limit: max_options,
        });
    }
    let mut counts = OptionCounts::default();
    for option in options {
        budget.charge_with_overhead(
            option.len(),
            OPTION_COMPARISON_WORK,
            match domain {
                MountOptionDomain::Mount => "checking per-mount options",
                MountOptionDomain::Superblock => "checking superblock options",
            },
        )?;
        let counter = match option {
            b"rw" => Some(&mut counts.rw),
            b"ro" => Some(&mut counts.ro),
            b"nosuid" => Some(&mut counts.nosuid),
            b"suid" => Some(&mut counts.suid),
            b"nodev" => Some(&mut counts.nodev),
            b"dev" => Some(&mut counts.dev),
            b"noexec" => Some(&mut counts.noexec),
            b"exec" => Some(&mut counts.exec),
            b"nosymfollow" => Some(&mut counts.nosymfollow),
            b"symfollow" => Some(&mut counts.symfollow),
            _ => None,
        };
        if let Some(counter) = counter {
            *counter += 1;
        }
    }
    Ok(counts)
}

fn require_read_write(
    domain: MountOptionDomain,
    rw_count: usize,
    ro_count: usize,
) -> Result<(), BootMountInfoPolicyError> {
    if rw_count == 1 && ro_count == 0 {
        Ok(())
    } else {
        Err(BootMountInfoPolicyError::InvalidReadWriteState {
            domain,
            rw_count,
            ro_count,
        })
    }
}

fn require_flag(
    flag: RequiredBootMountFlag,
    required_count: usize,
    inverse_count: usize,
) -> Result<(), BootMountInfoPolicyError> {
    if required_count == 1 && inverse_count == 0 {
        Ok(())
    } else {
        Err(BootMountInfoPolicyError::InvalidSecurityFlagState {
            flag,
            required_count,
            inverse_count,
        })
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

    fn charge(&mut self, amount: usize, action: &'static str) -> Result<(), BootMountInfoPolicyError> {
        self.checkpoint()?;
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or(BootMountInfoPolicyError::WorkLimitExceeded {
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
    ) -> Result<(), BootMountInfoPolicyError> {
        let amount = bytes
            .checked_add(overhead)
            .ok_or(BootMountInfoPolicyError::WorkLimitExceeded {
                limit: self.initial,
                action,
            })?;
        self.charge(amount, action)
    }

    fn checkpoint(&mut self) -> Result<(), BootMountInfoPolicyError> {
        require_deadline(self.deadline, self.clock)
    }

    const fn consumed(&self) -> usize {
        self.initial - self.remaining
    }
}

fn require_deadline(deadline: Instant, clock: &mut impl FnMut() -> Instant) -> Result<(), BootMountInfoPolicyError> {
    if clock() > deadline {
        Err(BootMountInfoPolicyError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) const fn validated_boot_mount_policy_fixture() -> ValidatedBootMountInfoPolicy {
    ValidatedBootMountInfoPolicy {
        filesystem: BootFilesystemKind::Vfat,
        mount_read_write: true,
        superblock_read_write: true,
        nosuid: true,
        nodev: true,
        noexec: true,
        nosymfollow: true,
    }
}

#[cfg(test)]
pub(super) fn validate_selected_boot_mount_policy_with_test_limits_and_clock(
    selected: SelectedMountInfoAttachment<'_>,
    limits: BootMountInfoPolicyLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(ValidatedBootMountInfoPolicy, usize), BootMountInfoPolicyError> {
    validate_selected_boot_mount_policy_with_limits_and_clock(selected, limits, deadline, clock)
}
