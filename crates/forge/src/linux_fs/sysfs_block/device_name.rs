//! Bounded validation of one kernel block-device name.
//!
//! The result preserves opaque bytes and proves only that they form a safe,
//! bounded relative locator. It is descriptive sysfs evidence, not permission
//! to open a device node or to derive mutation authority from a pathname.

use std::{io, time::Instant};

use super::{WorkBudget, copied_bytes, invalid_data};

const MAX_DEVICE_NAME_BYTES: usize = 4 * 1024 - 1;
const MAX_DEVICE_NAME_COMPONENTS: usize = 128;
const MAX_DEVICE_NAME_COMPONENT_BYTES: usize = 255;
const MAX_DEVICE_NAME_WORK: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsBlockDeviceName(Vec<u8>);

impl SysfsBlockDeviceName {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(crate) fn parse_sysfs_block_device_name(bytes: &[u8]) -> io::Result<SysfsBlockDeviceName> {
    parse_with_deadline(bytes, None)
}

pub(crate) fn parse_sysfs_block_device_name_until(bytes: &[u8], deadline: Instant) -> io::Result<SysfsBlockDeviceName> {
    parse_with_deadline(bytes, Some(deadline))
}

fn parse_with_deadline(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsBlockDeviceName> {
    let mut budget = match deadline {
        Some(deadline) => WorkBudget::until(MAX_DEVICE_NAME_WORK, deadline),
        None => WorkBudget::new(MAX_DEVICE_NAME_WORK),
    };
    budget.checkpoint()?;
    budget.charge(bytes.len(), "scanning a block-device name")?;
    if bytes.is_empty() || bytes.len() > MAX_DEVICE_NAME_BYTES || bytes[0] == b'/' || bytes.contains(&0) {
        return Err(invalid_data("sysfs DEVNAME is not one bounded relative locator"));
    }

    let mut components = 0usize;
    for component in bytes.split(|byte| *byte == b'/') {
        budget.charge(
            component.len().saturating_add(1),
            "validating a block-device name component",
        )?;
        components = components.saturating_add(1);
        if components > MAX_DEVICE_NAME_COMPONENTS
            || component.is_empty()
            || component.len() > MAX_DEVICE_NAME_COMPONENT_BYTES
            || component == b"."
            || component == b".."
        {
            return Err(invalid_data(
                "sysfs DEVNAME contains an unsafe or overlong path component",
            ));
        }
    }

    budget.charge(bytes.len(), "copying an authenticated block-device name")?;
    let retained = copied_bytes(bytes, "authenticated block-device name")?;
    budget.checkpoint()?;
    Ok(SysfsBlockDeviceName(retained))
}
