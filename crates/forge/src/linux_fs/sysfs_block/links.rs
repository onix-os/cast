use std::{io, time::Instant};

use super::{SYSFS_LINK_TARGET_MAX_BYTES, WorkBudget, copied_bytes, invalid_data, invalid_input};

const MAX_LINK_COMPONENTS: usize = 128;
const MAX_LINK_COMPONENT_BYTES: usize = 255;
const MAX_LINK_WORK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub(in super::super) struct LinkLimits {
    pub(in super::super) max_bytes: usize,
    pub(in super::super) max_components: usize,
    pub(in super::super) max_component_bytes: usize,
    pub(in super::super) max_work: usize,
}

const LINK_LIMITS: LinkLimits = LinkLimits {
    max_bytes: SYSFS_LINK_TARGET_MAX_BYTES,
    max_components: MAX_LINK_COMPONENTS,
    max_component_bytes: MAX_LINK_COMPONENT_BYTES,
    max_work: MAX_LINK_WORK,
};

/// A normalized path below the authenticated sysfs root.
///
/// Components are raw bytes and always begin with `devices`. This value still
/// carries no authority to resolve them; the descriptor layer must walk them
/// beneath the retained sysfs root without following unvalidated links.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsDeviceTarget {
    components: Vec<Vec<u8>>,
}

impl SysfsDeviceTarget {
    pub(crate) fn components(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.components.iter().map(Vec::as_slice)
    }

    pub(crate) fn basename(&self) -> &[u8] {
        self.components
            .last()
            .expect("validated sysfs device target has a descendant")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsSubsystemName(Vec<u8>);

impl SysfsSubsystemName {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Normalize the raw target of `/sys/dev/block/<major>:<minor>` relative to
/// the fixed `dev/block` base.
pub(crate) fn normalize_sysfs_dev_block_target(target: &[u8]) -> io::Result<SysfsDeviceTarget> {
    normalize_sysfs_dev_block_target_with_limits_and_work_deadline(target, LINK_LIMITS, None).map(|(target, _)| target)
}

/// Normalize one `dev/block` link within the caller's operation deadline.
pub(crate) fn normalize_sysfs_dev_block_target_until(
    target: &[u8],
    deadline: Instant,
) -> io::Result<SysfsDeviceTarget> {
    normalize_sysfs_dev_block_target_with_limits_and_work_deadline(target, LINK_LIMITS, Some(deadline))
        .map(|(target, _)| target)
}

pub(in super::super) fn normalize_sysfs_dev_block_target_with_limits_and_work(
    target: &[u8],
    limits: LinkLimits,
) -> io::Result<(SysfsDeviceTarget, usize)> {
    normalize_sysfs_dev_block_target_with_limits_and_work_deadline(target, limits, None)
}

fn normalize_sysfs_dev_block_target_with_limits_and_work_deadline(
    target: &[u8],
    limits: LinkLimits,
    deadline: Option<Instant>,
) -> io::Result<(SysfsDeviceTarget, usize)> {
    super::require_deadline(deadline)?;
    validate_limits(limits)?;
    let mut budget = match deadline {
        Some(deadline) => WorkBudget::until(limits.max_work, deadline),
        None => WorkBudget::new(limits.max_work),
    };
    validate_whole_target(target, limits, "sysfs dev/block link", &budget)?;
    budget.charge(target.len(), "scanning a sysfs dev/block link")?;

    let mut base_depth = 2_usize;
    let mut saw_device_component = false;
    let mut component_count = 0_usize;
    let mut components = Vec::<Vec<u8>>::new();
    for component in target.split(|byte| *byte == b'/') {
        budget.checkpoint()?;
        component_count = component_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("sysfs link component count overflowed"))?;
        require_component_bound(component, component_count, limits)?;
        budget.charge(component.len().saturating_add(1), "normalizing a sysfs dev/block link")?;

        match component {
            b"." => return Err(invalid_data("sysfs dev/block link contains a dot component")),
            b".." if saw_device_component => {
                return Err(invalid_data(
                    "sysfs dev/block link traverses upward after a named component",
                ));
            }
            b".." => {
                base_depth = base_depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid_data("sysfs dev/block link escapes the sysfs root"))?;
            }
            _ => {
                saw_device_component = true;
                if base_depth != 0 {
                    return Err(invalid_data("sysfs dev/block link does not leave the dev/block base"));
                }
                budget.charge(component.len(), "retaining a normalized sysfs path component")?;
                components
                    .try_reserve(1)
                    .map_err(|source| io::Error::other(format!("could not grow normalized sysfs path: {source}")))?;
                components.push(copied_bytes(component, "normalized sysfs path component")?);
                budget.checkpoint()?;
            }
        }
    }

    if base_depth != 0 {
        return Err(invalid_data(
            "sysfs dev/block link does not normalize from its fixed base",
        ));
    }
    if components.first().map(Vec::as_slice) != Some(b"devices") {
        return Err(invalid_data(
            "sysfs dev/block link does not normalize below sysfs devices",
        ));
    }
    if components.len() < 2 {
        return Err(invalid_data(
            "sysfs dev/block link names the devices directory rather than a device",
        ));
    }
    budget.checkpoint()?;
    let consumed = budget.consumed();
    Ok((SysfsDeviceTarget { components }, consumed))
}

/// Extract and validate the final kernel subsystem name from a raw relative
/// `subsystem` symlink target.
pub(crate) fn parse_sysfs_subsystem_target(target: &[u8]) -> io::Result<SysfsSubsystemName> {
    parse_sysfs_subsystem_target_with_limits_and_work_deadline(target, LINK_LIMITS, None).map(|(name, _)| name)
}

/// Parse one `subsystem` link within the caller's operation deadline.
pub(crate) fn parse_sysfs_subsystem_target_until(target: &[u8], deadline: Instant) -> io::Result<SysfsSubsystemName> {
    parse_sysfs_subsystem_target_with_limits_and_work_deadline(target, LINK_LIMITS, Some(deadline))
        .map(|(name, _)| name)
}

pub(in super::super) fn parse_sysfs_subsystem_target_with_limits_and_work(
    target: &[u8],
    limits: LinkLimits,
) -> io::Result<(SysfsSubsystemName, usize)> {
    parse_sysfs_subsystem_target_with_limits_and_work_deadline(target, limits, None)
}

fn parse_sysfs_subsystem_target_with_limits_and_work_deadline(
    target: &[u8],
    limits: LinkLimits,
    deadline: Option<Instant>,
) -> io::Result<(SysfsSubsystemName, usize)> {
    super::require_deadline(deadline)?;
    validate_limits(limits)?;
    let mut budget = match deadline {
        Some(deadline) => WorkBudget::until(limits.max_work, deadline),
        None => WorkBudget::new(limits.max_work),
    };
    validate_whole_target(target, limits, "sysfs subsystem link", &budget)?;
    budget.charge(target.len(), "scanning a sysfs subsystem link")?;

    let mut saw_named_component = false;
    let mut basename = None;
    let mut component_count = 0_usize;
    for component in target.split(|byte| *byte == b'/') {
        budget.checkpoint()?;
        component_count = component_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("sysfs subsystem component count overflowed"))?;
        require_component_bound(component, component_count, limits)?;
        budget.charge(component.len().saturating_add(1), "validating a sysfs subsystem link")?;

        match component {
            b"." => return Err(invalid_data("sysfs subsystem link contains a dot component")),
            b".." if saw_named_component => {
                return Err(invalid_data(
                    "sysfs subsystem link traverses upward after a named component",
                ));
            }
            b".." => {}
            _ => {
                saw_named_component = true;
                basename = Some(component);
            }
        }
    }

    let basename = basename.ok_or_else(|| invalid_data("sysfs subsystem link has no subsystem basename"))?;
    if !canonical_subsystem_name(basename, &budget)? {
        return Err(invalid_data(
            "sysfs subsystem basename is not a canonical kernel identifier",
        ));
    }
    budget.charge(basename.len(), "retaining a sysfs subsystem basename")?;
    let name = SysfsSubsystemName(copied_bytes(basename, "sysfs subsystem basename")?);
    budget.checkpoint()?;
    let consumed = budget.consumed();
    Ok((name, consumed))
}

fn validate_limits(limits: LinkLimits) -> io::Result<()> {
    if limits.max_bytes == 0 || limits.max_components == 0 || limits.max_component_bytes == 0 || limits.max_work == 0 {
        return Err(invalid_input("sysfs link parser limits must be nonzero"));
    }
    if limits.max_component_bytes > limits.max_bytes {
        return Err(invalid_input(
            "sysfs link component limit exceeds its aggregate byte limit",
        ));
    }
    Ok(())
}

fn validate_whole_target(
    target: &[u8],
    limits: LinkLimits,
    field: &'static str,
    budget: &WorkBudget,
) -> io::Result<()> {
    if target.is_empty() {
        return Err(invalid_data(format!("{field} target is empty")));
    }
    if target.len() > limits.max_bytes {
        return Err(invalid_data(format!(
            "{field} exceeds the {} byte limit",
            limits.max_bytes
        )));
    }
    if target[0] == b'/' {
        return Err(invalid_data(format!("{field} target is absolute")));
    }
    for byte in target {
        budget.checkpoint()?;
        if *byte == b'\0' {
            return Err(invalid_data(format!("{field} target contains a NUL byte")));
        }
    }
    budget.checkpoint()?;
    Ok(())
}

fn require_component_bound(component: &[u8], count: usize, limits: LinkLimits) -> io::Result<()> {
    if count > limits.max_components {
        return Err(invalid_data(format!(
            "sysfs link exceeds the {} component limit",
            limits.max_components
        )));
    }
    if component.is_empty() {
        return Err(invalid_data("sysfs link contains an empty path component"));
    }
    if component.len() > limits.max_component_bytes {
        return Err(invalid_data(format!(
            "sysfs link component exceeds the {} byte limit",
            limits.max_component_bytes
        )));
    }
    Ok(())
}

fn canonical_subsystem_name(name: &[u8], budget: &WorkBudget) -> io::Result<bool> {
    if !name.first().is_some_and(u8::is_ascii_alphanumeric) {
        return Ok(false);
    }
    for byte in name {
        budget.checkpoint()?;
        if !(byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.')) {
            return Ok(false);
        }
    }
    budget.checkpoint()?;
    Ok(true)
}
