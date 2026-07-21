// Bounded, byte-preserving parsing for Linux `/proc/*/mountinfo` records.
//
// This foundation deliberately stops at a syntactic snapshot. It does not
// open procfs, resolve public paths, infer mount topology, or grant filesystem
// mutation authority. Callers must authenticate and retain the procfs file
// separately before using `read_mountinfo_bounded`.

use std::{collections::HashSet, io, time::Instant};

use super::{read_to_end_bounded, read_to_end_bounded_until};

const MAX_MOUNTINFO_BYTES: usize = 4 * 1024 * 1024;
const MAX_MOUNTINFO_LINES: usize = 16 * 1024;
const MAX_MOUNTINFO_FIELDS_PER_LINE: usize = 256;
const MAX_MOUNTINFO_TOTAL_FIELDS: usize = 256 * 1024;
const MAX_MOUNTINFO_FIELD_BYTES: usize = 256 * 1024;
const MAX_MOUNTINFO_WORK: usize = MAX_MOUNTINFO_BYTES * 8;

#[derive(Debug, Clone, Copy)]
pub(super) struct MountInfoLimits {
    pub(super) max_bytes: usize,
    pub(super) max_lines: usize,
    pub(super) max_fields_per_line: usize,
    pub(super) max_total_fields: usize,
    pub(super) max_field_bytes: usize,
    pub(super) max_work: usize,
}

pub(super) const MOUNTINFO_LIMITS: MountInfoLimits = MountInfoLimits {
    max_bytes: MAX_MOUNTINFO_BYTES,
    max_lines: MAX_MOUNTINFO_LINES,
    max_fields_per_line: MAX_MOUNTINFO_FIELDS_PER_LINE,
    max_total_fields: MAX_MOUNTINFO_TOTAL_FIELDS,
    max_field_bytes: MAX_MOUNTINFO_FIELD_BYTES,
    max_work: MAX_MOUNTINFO_WORK,
};

/// One Linux device number from mountinfo's `major:minor` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MountDevice {
    major: u32,
    minor: u32,
}

impl MountDevice {
    pub(crate) fn major(self) -> u32 {
        self.major
    }

    pub(crate) fn minor(self) -> u32 {
        self.minor
    }
}

/// One immutable, syntactically validated mountinfo record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MountInfoEntry {
    mount_id: u64,
    parent_id: u64,
    device: MountDevice,
    root: Vec<u8>,
    mount_point: Vec<u8>,
    mount_options: Vec<Vec<u8>>,
    optional_fields: Vec<Vec<u8>>,
    filesystem_type: Vec<u8>,
    mount_source: Vec<u8>,
    super_options: Vec<Vec<u8>>,
}

impl MountInfoEntry {
    pub(crate) fn mount_id(&self) -> u64 {
        self.mount_id
    }

    pub(crate) fn parent_id(&self) -> u64 {
        self.parent_id
    }

    pub(crate) fn device(&self) -> MountDevice {
        self.device
    }

    /// Exact decoded bytes for field 4. No UTF-8 conversion is performed.
    ///
    /// Most filesystems emit an absolute pathname here, but Linux lets a
    /// filesystem's `show_path` callback provide this field. For example,
    /// nsfs emits an opaque namespace identity such as `mnt:[4026532758]`.
    pub(crate) fn root(&self) -> &[u8] {
        &self.root
    }

    /// Exact decoded bytes for field 5. No UTF-8 conversion is performed.
    pub(crate) fn mount_point(&self) -> &[u8] {
        &self.mount_point
    }

    pub(crate) fn mount_options(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.mount_options.iter().map(Vec::as_slice)
    }

    pub(crate) fn optional_fields(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.optional_fields.iter().map(Vec::as_slice)
    }

    /// Exact decoded bytes for the filesystem-type field.
    pub(crate) fn filesystem_type(&self) -> &[u8] {
        &self.filesystem_type
    }

    /// Exact decoded bytes for field 10. No UTF-8 conversion is performed.
    pub(crate) fn mount_source(&self) -> &[u8] {
        &self.mount_source
    }

    pub(crate) fn super_options(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.super_options.iter().map(Vec::as_slice)
    }
}

/// An ordered snapshot of all records in one complete mountinfo read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MountInfo {
    entries: Vec<MountInfoEntry>,
}

impl MountInfo {
    pub(crate) fn entries(&self) -> &[MountInfoEntry] {
        &self.entries
    }
}

/// Read and parse one complete mountinfo snapshot under fixed resource bounds.
///
/// The extra byte distinguishes an exactly full valid input from truncation.
/// Interrupted reads use `read_to_end_bounded`'s finite retry ceiling.
pub(crate) fn read_mountinfo_bounded(reader: &mut impl io::Read) -> io::Result<MountInfo> {
    read_mountinfo_with_limits(reader, MOUNTINFO_LIMITS)
}

/// Read one exact mountinfo byte snapshot and parse it under the production
/// byte, grammar, work, retry, and deadline bounds.
///
/// Retaining the bytes lets an already-authenticated caller preserve its exact
/// snapshot provenance and bounded diagnostics. This generic reader grants no
/// descriptor-backed authority by itself. A later topology must compare its
/// selected parsed entries and their uniqueness predicates, not require whole
/// snapshot equality: unrelated mount-table activity is not a topology change.
pub(crate) fn read_mountinfo_snapshot_bounded_until(
    reader: &mut impl io::Read,
    deadline: Instant,
) -> io::Result<(Vec<u8>, MountInfo)> {
    let read_limit = MOUNTINFO_LIMITS
        .max_bytes
        .checked_add(1)
        .ok_or_else(|| invalid_input("mountinfo byte limit cannot reserve a truncation sentinel"))?;
    let bytes = read_to_end_bounded_until(reader, read_limit, deadline)?;
    if bytes.len() > MOUNTINFO_LIMITS.max_bytes {
        return Err(invalid_data(format!(
            "mountinfo exceeds the {} byte limit",
            MOUNTINFO_LIMITS.max_bytes
        )));
    }
    let parsed = parse_mountinfo_with_limits_and_work_until(&bytes, MOUNTINFO_LIMITS, deadline)?.0;
    Ok((bytes, parsed))
}

/// Parse one complete mountinfo snapshot under fixed resource bounds.
pub(crate) fn parse_mountinfo_bytes(bytes: &[u8]) -> io::Result<MountInfo> {
    parse_mountinfo_with_limits(bytes, MOUNTINFO_LIMITS)
}

pub(super) fn read_mountinfo_with_limits(reader: &mut impl io::Read, limits: MountInfoLimits) -> io::Result<MountInfo> {
    validate_limits(limits)?;
    let read_limit = limits
        .max_bytes
        .checked_add(1)
        .ok_or_else(|| invalid_input("mountinfo byte limit cannot reserve a truncation sentinel"))?;
    let bytes = read_to_end_bounded(reader, read_limit)?;
    if bytes.len() > limits.max_bytes {
        return Err(invalid_data(format!(
            "mountinfo exceeds the {} byte limit",
            limits.max_bytes
        )));
    }
    parse_mountinfo_with_limits(&bytes, limits)
}

pub(super) fn parse_mountinfo_with_limits(bytes: &[u8], limits: MountInfoLimits) -> io::Result<MountInfo> {
    parse_mountinfo_with_limits_and_work(bytes, limits).map(|(mountinfo, _work)| mountinfo)
}

pub(super) fn parse_mountinfo_with_limits_and_work(
    bytes: &[u8],
    limits: MountInfoLimits,
) -> io::Result<(MountInfo, usize)> {
    parse_mountinfo_with_limits_and_work_deadline(bytes, limits, None)
}

pub(super) fn parse_mountinfo_with_limits_and_work_until(
    bytes: &[u8],
    limits: MountInfoLimits,
    deadline: Instant,
) -> io::Result<(MountInfo, usize)> {
    parse_mountinfo_with_limits_and_work_deadline(bytes, limits, Some(deadline))
}

fn parse_mountinfo_with_limits_and_work_deadline(
    bytes: &[u8],
    limits: MountInfoLimits,
    deadline: Option<Instant>,
) -> io::Result<(MountInfo, usize)> {
    require_parse_deadline(deadline)?;
    validate_limits(limits)?;
    if bytes.len() > limits.max_bytes {
        return Err(invalid_data(format!(
            "mountinfo exceeds the {} byte limit",
            limits.max_bytes
        )));
    }
    if bytes.is_empty() {
        return Err(unexpected_eof("mountinfo is empty"));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(unexpected_eof("mountinfo lacks its terminating newline"));
    }
    if bytes.contains(&b'\0') {
        return Err(invalid_data("mountinfo contains a NUL byte"));
    }
    require_parse_deadline(deadline)?;

    let mut budget = WorkBudget::new(limits.max_work, deadline);
    budget.charge(bytes.len(), "scanning mountinfo bytes")?;

    let mut entries = Vec::new();
    let mut mount_ids = HashSet::new();
    let mut total_fields = 0usize;
    for (line_index, line) in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n').enumerate() {
        budget.checkpoint()?;
        let line_number = line_index + 1;
        if line_number > limits.max_lines {
            return Err(invalid_data(format!(
                "mountinfo exceeds the {} line limit",
                limits.max_lines
            )));
        }
        if line.is_empty() {
            return Err(invalid_data(format!("mountinfo line {line_number} is empty")));
        }
        if line.len() > limits.max_bytes {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} exceeds the byte limit"
            )));
        }
        if line.contains(&b'\t') {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} contains an unescaped tab"
            )));
        }

        budget.charge(line.len(), "splitting mountinfo fields")?;
        let available_raw_fields = limits.max_total_fields - total_fields;
        let fields = split_fields(line, line_number, limits, available_raw_fields, &mut budget)?;
        total_fields = total_fields
            .checked_add(fields.len())
            .ok_or_else(|| invalid_data("mountinfo field count overflowed"))?;
        if total_fields > limits.max_total_fields {
            return Err(invalid_data(format!(
                "mountinfo exceeds the {} total field limit",
                limits.max_total_fields
            )));
        }

        let available_list_fields = limits.max_total_fields - total_fields;
        let (entry, list_fields) =
            parse_mountinfo_line(&fields, line_number, limits, available_list_fields, &mut budget)?;
        total_fields = total_fields
            .checked_add(list_fields)
            .ok_or_else(|| invalid_data("mountinfo field count overflowed"))?;
        if total_fields > limits.max_total_fields {
            return Err(invalid_data(format!(
                "mountinfo exceeds the {} total field limit",
                limits.max_total_fields
            )));
        }

        mount_ids
            .try_reserve(1)
            .map_err(|source| allocation_error("mount ID set", source))?;
        if !mount_ids.insert(entry.mount_id) {
            return Err(invalid_data(format!("mountinfo repeats mount ID {}", entry.mount_id)));
        }
        entries
            .try_reserve(1)
            .map_err(|source| allocation_error("mountinfo entries", source))?;
        entries.push(entry);
        budget.checkpoint()?;
    }

    if entries.is_empty() {
        return Err(unexpected_eof("mountinfo contains no records"));
    }
    budget.checkpoint()?;
    let consumed_work = budget.consumed();
    Ok((MountInfo { entries }, consumed_work))
}

fn split_fields<'a>(
    line: &'a [u8],
    line_number: usize,
    limits: MountInfoLimits,
    max_aggregate_fields: usize,
    budget: &mut WorkBudget,
) -> io::Result<Vec<&'a [u8]>> {
    let mut fields = Vec::new();
    for field in line.split(|byte| *byte == b' ') {
        if field.is_empty() {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} has an empty or ambiguously separated field"
            )));
        }
        if field.len() > limits.max_field_bytes {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} has a field longer than {} bytes",
                limits.max_field_bytes
            )));
        }
        if fields.len() >= limits.max_fields_per_line {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} exceeds the {} field limit",
                limits.max_fields_per_line
            )));
        }
        if fields.len() >= max_aggregate_fields {
            return Err(invalid_data(format!(
                "mountinfo exceeds the {} total field limit",
                limits.max_total_fields
            )));
        }
        budget.charge(1, "counting mountinfo fields")?;
        fields
            .try_reserve(1)
            .map_err(|source| allocation_error("mountinfo line fields", source))?;
        fields.push(field);
    }
    Ok(fields)
}

fn parse_mountinfo_line(
    fields: &[&[u8]],
    line_number: usize,
    limits: MountInfoLimits,
    max_list_fields: usize,
    budget: &mut WorkBudget,
) -> io::Result<(MountInfoEntry, usize)> {
    if fields.len() < 10 {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} has fewer than 10 required fields"
        )));
    }
    let separator_index = fields.len() - 4;
    if fields[separator_index] != b"-" {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} does not place '-' before its three trailing fields"
        )));
    }
    if fields[6..separator_index].contains(&b"-".as_slice()) {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} has an ambiguous '-' optional field"
        )));
    }

    let mount_id = parse_positive_u64(fields[0], "mount ID", line_number, budget)?;
    let parent_id = parse_positive_u64(fields[1], "parent mount ID", line_number, budget)?;
    let device = parse_device(fields[2], line_number, budget)?;
    // Linux normally renders this as a path, but a filesystem-specific
    // `show_path` callback may render an opaque nonempty identity instead.
    // Authority-bearing consumers validate the exact root they require.
    let root = decode_mountinfo_path(fields[3], "root", line_number, false, budget)?;
    let mount_point = decode_mountinfo_path(fields[4], "mount point", line_number, true, budget)?;

    let mut line_field_units = fields.len();
    let mut list_field_units = 0usize;
    let mount_options = parse_option_list(
        fields[5],
        "mount options",
        line_number,
        (limits.max_fields_per_line - line_field_units).min(max_list_fields - list_field_units),
        budget,
    )?;
    line_field_units = add_list_field_units(line_field_units, mount_options.len(), line_number, limits)?;
    list_field_units = list_field_units
        .checked_add(mount_options.len())
        .ok_or_else(|| invalid_data("mountinfo aggregate option-item count overflowed"))?;

    let mut optional_fields = Vec::new();
    for field in &fields[6..separator_index] {
        budget.charge(field.len(), "copying mountinfo optional fields")?;
        optional_fields
            .try_reserve(1)
            .map_err(|source| allocation_error("mountinfo optional fields", source))?;
        optional_fields.push(copy_bytes(field, "mountinfo optional field")?);
    }

    let filesystem_type =
        decode_mountinfo_mangled_field(fields[separator_index + 1], "filesystem type", line_number, budget)?;
    let mount_source =
        decode_mountinfo_mangled_field(fields[separator_index + 2], "mount source", line_number, budget)?;
    let super_options = parse_option_list(
        fields[separator_index + 3],
        "superblock options",
        line_number,
        (limits.max_fields_per_line - line_field_units).min(max_list_fields - list_field_units),
        budget,
    )?;
    add_list_field_units(line_field_units, super_options.len(), line_number, limits)?;
    list_field_units = list_field_units
        .checked_add(super_options.len())
        .ok_or_else(|| invalid_data("mountinfo aggregate option-item count overflowed"))?;

    Ok((
        MountInfoEntry {
            mount_id,
            parent_id,
            device,
            root,
            mount_point,
            mount_options,
            optional_fields,
            filesystem_type,
            mount_source,
            super_options,
        },
        list_field_units,
    ))
}

fn add_list_field_units(
    current: usize,
    count: usize,
    line_number: usize,
    limits: MountInfoLimits,
) -> io::Result<usize> {
    let total = current
        .checked_add(count)
        .ok_or_else(|| invalid_data("mountinfo line field count overflowed"))?;
    if total > limits.max_fields_per_line {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} exceeds the {} field and option-item limit",
            limits.max_fields_per_line
        )));
    }
    Ok(total)
}

fn parse_positive_u64(field: &[u8], name: &str, line_number: usize, budget: &mut WorkBudget) -> io::Result<u64> {
    budget.charge(field.len(), "parsing mountinfo decimal fields")?;
    if field.is_empty() || field[0] == b'0' || !field.iter().all(u8::is_ascii_digit) {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} has a non-canonical {name}"
        )));
    }
    field.iter().try_fold(0_u64, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
            .ok_or_else(|| invalid_data(format!("mountinfo line {line_number} {name} overflows u64")))
    })
}

fn parse_canonical_u32(field: &[u8], name: &str, line_number: usize, budget: &mut WorkBudget) -> io::Result<u32> {
    budget.charge(field.len(), "parsing mountinfo device fields")?;
    if field.is_empty() || (field.len() > 1 && field[0] == b'0') || !field.iter().all(u8::is_ascii_digit) {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} has a non-canonical {name}"
        )));
    }
    field.iter().try_fold(0_u32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(*digit - b'0')))
            .ok_or_else(|| invalid_data(format!("mountinfo line {line_number} {name} overflows u32")))
    })
}

fn parse_device(field: &[u8], line_number: usize, budget: &mut WorkBudget) -> io::Result<MountDevice> {
    budget.charge(field.len(), "splitting mountinfo device fields")?;
    let mut parts = field.split(|byte| *byte == b':');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().ok_or_else(|| {
        invalid_data(format!(
            "mountinfo line {line_number} device field lacks one ':' separator"
        ))
    })?;
    if parts.next().is_some() {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} device field has multiple ':' separators"
        )));
    }
    Ok(MountDevice {
        major: parse_canonical_u32(major, "device major number", line_number, budget)?,
        minor: parse_canonical_u32(minor, "device minor number", line_number, budget)?,
    })
}

fn decode_mountinfo_path(
    field: &[u8],
    name: &str,
    line_number: usize,
    require_absolute: bool,
    budget: &mut WorkBudget,
) -> io::Result<Vec<u8>> {
    decode_mountinfo_escaped_field(field, name, line_number, require_absolute, false, budget)
}

fn decode_mountinfo_mangled_field(
    field: &[u8],
    name: &str,
    line_number: usize,
    budget: &mut WorkBudget,
) -> io::Result<Vec<u8>> {
    decode_mountinfo_escaped_field(field, name, line_number, false, true, budget)
}

fn decode_mountinfo_escaped_field(
    field: &[u8],
    name: &str,
    line_number: usize,
    require_absolute: bool,
    allow_hash_escape: bool,
    budget: &mut WorkBudget,
) -> io::Result<Vec<u8>> {
    budget.charge(field.len(), "decoding mountinfo path fields")?;
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(field.len())
        .map_err(|source| allocation_error("decoded mountinfo path", source))?;
    let mut offset = 0usize;
    while offset < field.len() {
        if offset % 4_096 == 0 {
            budget.checkpoint()?;
        }
        let byte = field[offset];
        if byte != b'\\' {
            decoded.push(byte);
            offset += 1;
            continue;
        }
        let escape_end = offset
            .checked_add(4)
            .ok_or_else(|| invalid_data("mountinfo escape offset overflowed"))?;
        let escape = field
            .get(offset..escape_end)
            .ok_or_else(|| invalid_data(format!("mountinfo line {line_number} {name} has a truncated escape")))?;
        let decoded_byte = match escape {
            b"\\040" => b' ',
            b"\\011" => b'\t',
            b"\\012" => b'\n',
            b"\\134" => b'\\',
            b"\\043" if allow_hash_escape => b'#',
            _ => {
                return Err(invalid_data(format!(
                    "mountinfo line {line_number} {name} has an unknown kernel escape"
                )));
            }
        };
        decoded.push(decoded_byte);
        offset = escape_end;
    }
    if decoded.is_empty() {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} has an empty {name}"
        )));
    }
    if require_absolute && decoded.first() != Some(&b'/') {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} {name} is not absolute"
        )));
    }
    Ok(decoded)
}

fn parse_option_list(
    field: &[u8],
    name: &str,
    line_number: usize,
    max_items: usize,
    budget: &mut WorkBudget,
) -> io::Result<Vec<Vec<u8>>> {
    // Filesystem option emitters may use escapes such as `\054` for a comma.
    // Those fields are therefore split only at literal delimiter commas and
    // otherwise retained byte-for-byte. Root and mount-point use four path
    // escapes; mangle-produced filesystem type and source add `\043`.
    if max_items == 0 {
        return Err(invalid_data(format!(
            "mountinfo line {line_number} exceeds its option-item limit"
        )));
    }
    budget.charge(field.len(), "splitting mountinfo option fields")?;
    let mut options = Vec::new();
    for option in field.split(|byte| *byte == b',') {
        if option.is_empty() {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} {name} contains an empty item"
            )));
        }
        if options.len() >= max_items {
            return Err(invalid_data(format!(
                "mountinfo line {line_number} exceeds its option-item limit"
            )));
        }
        budget.charge(option.len(), "copying mountinfo option fields")?;
        options
            .try_reserve(1)
            .map_err(|source| allocation_error("mountinfo options", source))?;
        options.push(copy_bytes(option, "mountinfo option")?);
    }
    Ok(options)
}

fn copy_bytes(bytes: &[u8], name: &str) -> io::Result<Vec<u8>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| allocation_error(name, source))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn validate_limits(limits: MountInfoLimits) -> io::Result<()> {
    if limits.max_bytes == 0
        || limits.max_lines == 0
        || limits.max_fields_per_line < 12
        || limits.max_total_fields < 12
        || limits.max_field_bytes == 0
        || limits.max_work == 0
    {
        return Err(invalid_input(
            "mountinfo parser limits must be nonzero and admit one record",
        ));
    }
    Ok(())
}

struct WorkBudget {
    limit: usize,
    consumed: usize,
    deadline: Option<Instant>,
}

impl WorkBudget {
    fn new(limit: usize, deadline: Option<Instant>) -> Self {
        Self {
            limit,
            consumed: 0,
            deadline,
        }
    }

    fn charge(&mut self, amount: usize, operation: &str) -> io::Result<()> {
        self.checkpoint()?;
        let consumed = self
            .consumed
            .checked_add(amount)
            .filter(|consumed| *consumed <= self.limit)
            .ok_or_else(|| invalid_data(format!("mountinfo exceeded its work limit while {operation}")))?;
        self.consumed = consumed;
        self.checkpoint()?;
        Ok(())
    }

    fn checkpoint(&self) -> io::Result<()> {
        require_parse_deadline(self.deadline)
    }

    fn consumed(&self) -> usize {
        self.consumed
    }
}

fn require_parse_deadline(deadline: Option<Instant>) -> io::Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() > deadline) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "mountinfo parsing exceeded its deadline",
        ))
    } else {
        Ok(())
    }
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn unexpected_eof(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message.into())
}

fn allocation_error(name: &str, source: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("could not allocate {name}: {source}"))
}
