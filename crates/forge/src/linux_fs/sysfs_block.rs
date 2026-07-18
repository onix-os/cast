//! Pure, bounded parsing for Linux sysfs block-device evidence.
//!
//! These values prove only the syntax and internal consistency of one captured
//! kernel view. They do not authenticate sysfs descriptors, prove that two
//! reads came from one stable object, identify a filesystem, or establish GPT
//! partition roles. The descriptor-retaining topology layer must provide those
//! stronger guarantees separately.

use std::{io, num::NonZeroU32, num::NonZeroU64};

mod identity;
mod links;
mod numeric;
mod uevent;
mod uuid;

#[allow(unused_imports)] // named surface for the later descriptor-retaining layer
pub(crate) use identity::{SysfsDiskIdentity, SysfsPartitionIdentity};
#[allow(unused_imports)] // named surface for the later descriptor-retaining layer
pub(crate) use identity::{parse_sysfs_disk_identity, parse_sysfs_partition_identity, require_matching_disk_sequence};
#[allow(unused_imports)] // named surface for the later descriptor-retaining layer
pub(crate) use links::{SysfsDeviceTarget, SysfsSubsystemName};
#[allow(unused_imports)] // named surface for the later descriptor-retaining layer
pub(crate) use links::{normalize_sysfs_dev_block_target, parse_sysfs_subsystem_target};
pub(crate) use numeric::{parse_sysfs_dev, parse_sysfs_partition_number};
#[allow(unused_imports)] // named surface for exact retained unknown fields
pub(crate) use uevent::SysfsUeventField;
pub(crate) use uevent::{SysfsUevent, parse_sysfs_uevent};

#[cfg(test)]
pub(super) use links::{
    LinkLimits, normalize_sysfs_dev_block_target_with_limits_and_work,
    parse_sysfs_subsystem_target_with_limits_and_work,
};
#[cfg(test)]
pub(super) use uevent::{UeventLimits, parse_sysfs_uevent_with_limits_and_work};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SysfsDeviceNumber {
    major: u32,
    minor: u32,
}

impl SysfsDeviceNumber {
    pub(crate) const fn from_major_minor(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    pub(crate) const fn major(self) -> u32 {
        self.major
    }

    pub(crate) const fn minor(self) -> u32 {
        self.minor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SysfsPartitionNumber(NonZeroU32);

impl SysfsPartitionNumber {
    pub(crate) const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SysfsDiskSequence(NonZeroU64);

impl SysfsDiskSequence {
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SysfsPartitionUuid([u8; 36]);

impl SysfsPartitionUuid {
    pub(crate) fn as_str(&self) -> &str {
        // UUID construction admits only lowercase ASCII bytes.
        std::str::from_utf8(&self.0).expect("validated sysfs PARTUUID is ASCII")
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; 36] {
        &self.0
    }
}

#[derive(Debug)]
pub(super) struct WorkBudget {
    remaining: usize,
    initial: usize,
}

impl WorkBudget {
    pub(super) const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            initial: limit,
        }
    }

    pub(super) fn charge(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.remaining = self.remaining.checked_sub(amount).ok_or_else(|| {
            invalid_data(format!(
                "sysfs parser exceeded its {} unit work limit while {action}",
                self.initial
            ))
        })?;
        Ok(())
    }

    pub(super) const fn consumed(&self) -> usize {
        self.initial - self.remaining
    }
}

pub(super) fn copied_bytes(bytes: &[u8], context: &'static str) -> io::Result<Vec<u8>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| io::Error::other(format!("could not allocate {context}: {source}")))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

pub(super) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(super) fn unexpected_eof(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message.into())
}
