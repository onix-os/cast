use std::io;

use super::{WorkBudget, copied_bytes, invalid_data, invalid_input, unexpected_eof};

const MAX_UEVENT_BYTES: usize = 64 * 1024;
const MAX_UEVENT_LINES: usize = 256;
const MAX_UEVENT_LINE_BYTES: usize = 4 * 1024;
const MAX_UEVENT_KEY_BYTES: usize = 64;
const MAX_UEVENT_WORK: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(in super::super) struct UeventLimits {
    pub(in super::super) max_bytes: usize,
    pub(in super::super) max_lines: usize,
    pub(in super::super) max_line_bytes: usize,
    pub(in super::super) max_key_bytes: usize,
    pub(in super::super) max_work: usize,
}

const UEVENT_LIMITS: UeventLimits = UeventLimits {
    max_bytes: MAX_UEVENT_BYTES,
    max_lines: MAX_UEVENT_LINES,
    max_line_bytes: MAX_UEVENT_LINE_BYTES,
    max_key_bytes: MAX_UEVENT_KEY_BYTES,
    max_work: MAX_UEVENT_WORK,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsUeventField {
    key: Vec<u8>,
    value: Vec<u8>,
}

impl SysfsUeventField {
    pub(crate) fn key(&self) -> &[u8] {
        &self.key
    }

    /// Return the exact bytes following the first `=` in this field.
    pub(crate) fn value(&self) -> &[u8] {
        &self.value
    }
}

/// One ordered, duplicate-free sysfs `uevent` snapshot.
///
/// Unknown keys and values remain byte-exact so later evidence layers can
/// report them without this parser silently inventing policy for new kernels.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsUevent {
    fields: Vec<SysfsUeventField>,
}

impl SysfsUevent {
    pub(crate) fn fields(&self) -> &[SysfsUeventField] {
        &self.fields
    }

    pub(crate) fn value(&self, key: &[u8]) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(SysfsUeventField::value)
    }
}

pub(crate) fn parse_sysfs_uevent(bytes: &[u8]) -> io::Result<SysfsUevent> {
    parse_sysfs_uevent_with_limits_and_work(bytes, UEVENT_LIMITS).map(|(event, _)| event)
}

pub(in super::super) fn parse_sysfs_uevent_with_limits_and_work(
    bytes: &[u8],
    limits: UeventLimits,
) -> io::Result<(SysfsUevent, usize)> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_bytes {
        return Err(invalid_data(format!(
            "sysfs uevent exceeds the {} byte limit",
            limits.max_bytes
        )));
    }
    if bytes.is_empty() {
        return Err(unexpected_eof("sysfs uevent is empty"));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(unexpected_eof("sysfs uevent lacks its terminating newline"));
    }
    if bytes.contains(&b'\0') {
        return Err(invalid_data("sysfs uevent contains a NUL byte"));
    }
    if bytes.contains(&b'\r') {
        return Err(invalid_data("sysfs uevent contains a carriage return"));
    }

    let mut budget = WorkBudget::new(limits.max_work);
    budget.charge(bytes.len(), "scanning uevent bytes")?;
    let mut fields = Vec::<SysfsUeventField>::new();
    fields
        .try_reserve(limits.max_lines.min(16))
        .map_err(|source| io::Error::other(format!("could not allocate sysfs uevent fields: {source}")))?;

    for (line_index, line) in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n').enumerate() {
        let line_number = line_index + 1;
        if line_number > limits.max_lines {
            return Err(invalid_data(format!(
                "sysfs uevent exceeds the {} line limit",
                limits.max_lines
            )));
        }
        if line.is_empty() {
            return Err(invalid_data(format!("sysfs uevent line {line_number} is empty")));
        }
        if line.len() > limits.max_line_bytes {
            return Err(invalid_data(format!(
                "sysfs uevent line {line_number} exceeds the {} byte line limit",
                limits.max_line_bytes
            )));
        }

        budget.charge(line.len(), "finding a uevent key separator")?;
        let Some(separator) = line.iter().position(|byte| *byte == b'=') else {
            return Err(invalid_data(format!(
                "sysfs uevent line {line_number} lacks a key separator"
            )));
        };
        let key = &line[..separator];
        let value = &line[separator + 1..];
        budget.charge(key.len(), "validating uevent key syntax")?;
        validate_key(key, line_number, limits.max_key_bytes)?;

        for existing in &fields {
            budget.charge(
                key.len().saturating_add(existing.key.len()),
                "checking duplicate uevent keys",
            )?;
            if existing.key == key {
                return Err(invalid_data(format!("sysfs uevent repeats key on line {line_number}")));
            }
        }

        budget.charge(line.len(), "retaining a uevent field")?;
        fields
            .try_reserve(1)
            .map_err(|source| io::Error::other(format!("could not grow sysfs uevent fields: {source}")))?;
        fields.push(SysfsUeventField {
            key: copied_bytes(key, "sysfs uevent key")?,
            value: copied_bytes(value, "sysfs uevent value")?,
        });
    }

    if fields.is_empty() {
        return Err(invalid_data("sysfs uevent contains no fields"));
    }
    let consumed = budget.consumed();
    Ok((SysfsUevent { fields }, consumed))
}

fn validate_limits(limits: UeventLimits) -> io::Result<()> {
    if limits.max_bytes == 0
        || limits.max_lines == 0
        || limits.max_line_bytes == 0
        || limits.max_key_bytes == 0
        || limits.max_work == 0
    {
        return Err(invalid_input("sysfs uevent parser limits must be nonzero"));
    }
    if limits.max_line_bytes > limits.max_bytes {
        return Err(invalid_input(
            "sysfs uevent line limit exceeds the aggregate byte limit",
        ));
    }
    if limits.max_key_bytes > limits.max_line_bytes {
        return Err(invalid_input("sysfs uevent key limit exceeds the line byte limit"));
    }
    Ok(())
}

fn validate_key(key: &[u8], line_number: usize, max_key_bytes: usize) -> io::Result<()> {
    if key.is_empty() {
        return Err(invalid_data(format!(
            "sysfs uevent line {line_number} has an empty key"
        )));
    }
    if key.len() > max_key_bytes {
        return Err(invalid_data(format!(
            "sysfs uevent line {line_number} key exceeds the {max_key_bytes} byte limit"
        )));
    }
    if !key[0].is_ascii_uppercase()
        || !key
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(invalid_data(format!(
            "sysfs uevent line {line_number} key is not canonical kernel-environment syntax"
        )));
    }
    Ok(())
}
