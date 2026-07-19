//! Pure selection of one exact mount attachment from parsed mountinfo bytes.
//!
//! This module receives only an already parsed, immutable mountinfo snapshot.
//! It opens no path or descriptor, performs no discovery or mutation, and does
//! not treat filesystem-type, source, or option strings as authority.  A
//! successful value remains borrowed from the snapshot that was scanned.

use std::{io, time::Instant};

use super::mountinfo::{MOUNTINFO_LIMITS, MountInfo, MountInfoEntry};

const MAX_SELECTOR_BYTES: usize = 4_095;
const MAX_SELECTOR_COMPONENTS: usize = 128;
const MAX_SELECTOR_COMPONENT_BYTES: usize = 255;
const MAX_ATTACHMENT_ENTRIES: usize = 16 * 1_024;
// Comparing every admitted selector against every maximally populated parsed
// snapshot costs less than 128 MiB under the parser and selector ceilings.
const MAX_ATTACHMENT_WORK: usize = 128 * 1_024 * 1_024;
const WORST_CASE_ATTACHMENT_WORK: usize = MAX_ATTACHMENT_ENTRIES * (MAX_SELECTOR_BYTES + 8)
    + MOUNTINFO_LIMITS.max_bytes
    + 3 * MAX_SELECTOR_BYTES
    + MAX_SELECTOR_COMPONENTS
    + MOUNTINFO_LIMITS.max_field_bytes
    + 4;
const _: () = assert!(MAX_ATTACHMENT_ENTRIES >= MOUNTINFO_LIMITS.max_lines);
const _: () = assert!(MAX_ATTACHMENT_WORK >= WORST_CASE_ATTACHMENT_WORK);

#[derive(Debug, Clone, Copy)]
pub(super) struct MountInfoAttachmentLimits {
    pub(super) max_entries: usize,
    pub(super) max_work: usize,
}

pub(super) const MOUNTINFO_ATTACHMENT_LIMITS: MountInfoAttachmentLimits = MountInfoAttachmentLimits {
    max_entries: MAX_ATTACHMENT_ENTRIES,
    max_work: MAX_ATTACHMENT_WORK,
};

/// Selected attachment fields borrowed from one immutable mountinfo snapshot.
///
/// The filesystem type, mount source, and option fields are intentionally not
/// retained or exposed: they are descriptive kernel text, not partition-role
/// or attachment authority.  Revalidation of any live descriptor or namespace
/// is the responsibility of the descriptor-retaining aggregate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedMountInfoAttachment<'a> {
    mount_id: u64,
    device_major: u32,
    device_minor: u32,
    root: &'a [u8],
    mount_point: &'a [u8],
}

impl std::fmt::Debug for SelectedMountInfoAttachment<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectedMountInfoAttachment")
            .field("mount_id", &self.mount_id)
            .field("device_major", &self.device_major)
            .field("device_minor", &self.device_minor)
            .field("root", &self.root)
            .field("mount_point", &self.mount_point)
            .finish_non_exhaustive()
    }
}

impl SelectedMountInfoAttachment<'_> {
    pub(crate) const fn mount_id(&self) -> u64 {
        self.mount_id
    }

    pub(crate) const fn device_major(&self) -> u32 {
        self.device_major
    }

    pub(crate) const fn device_minor(&self) -> u32 {
        self.device_minor
    }

    /// Exact decoded mountinfo root bytes. Successful selections are `/`.
    pub(crate) const fn root(&self) -> &[u8] {
        self.root
    }

    /// Exact decoded mount-point bytes borrowed from the parsed snapshot.
    pub(crate) const fn mount_point(&self) -> &[u8] {
        self.mount_point
    }
}

/// Select one exact declared attachment under the caller's shared deadline.
///
/// The scan always examines the complete bounded snapshot. In particular, it
/// never stops at the first matching mount point, because a later stacked
/// attachment must make the selector ambiguous and fail closed.
pub(crate) fn select_mountinfo_attachment_until<'a>(
    mountinfo: &'a MountInfo,
    selector: &[u8],
    expected_mount_id: u64,
    expected_device_major: u32,
    expected_device_minor: u32,
    deadline: Instant,
) -> io::Result<SelectedMountInfoAttachment<'a>> {
    let mut clock = Instant::now;
    select_mountinfo_attachment_with_limits_and_clock(
        mountinfo,
        selector,
        expected_mount_id,
        expected_device_major,
        expected_device_minor,
        MOUNTINFO_ATTACHMENT_LIMITS,
        deadline,
        &mut clock,
    )
    .map(|(attachment, _work)| attachment)
}

fn select_mountinfo_attachment_with_limits_and_clock<'a>(
    mountinfo: &'a MountInfo,
    selector: &[u8],
    expected_mount_id: u64,
    expected_device_major: u32,
    expected_device_minor: u32,
    limits: MountInfoAttachmentLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<(SelectedMountInfoAttachment<'a>, usize)> {
    require_deadline(deadline, clock)?;
    validate_limits(limits)?;
    if expected_mount_id == 0 {
        return Err(invalid_input("expected mount ID must be nonzero"));
    }

    let mut budget = ScanBudget::new(limits.max_work, deadline, clock);
    validate_selector(selector, &mut budget)?;

    let entries = mountinfo.entries();
    if entries.len() > limits.max_entries {
        return Err(invalid_data(format!(
            "mountinfo attachment scan exceeds the {} entry limit",
            limits.max_entries
        )));
    }

    let mut selector_matches = 0usize;
    let mut expected_id_occurrences = 0usize;
    let mut selected: Option<&MountInfoEntry> = None;
    for entry in entries {
        // This pessimistically charges both slices even though slice equality
        // can return after comparing lengths or the first unequal byte.
        let comparison_work = selector
            .len()
            .checked_add(entry.mount_point().len())
            .and_then(|work| work.checked_add(8))
            .ok_or_else(|| invalid_data("mountinfo attachment comparison work overflowed"))?;
        budget.charge(comparison_work, "scanning one mountinfo attachment")?;

        if entry.mount_id() == expected_mount_id {
            expected_id_occurrences = expected_id_occurrences
                .checked_add(1)
                .ok_or_else(|| invalid_data("expected mount ID occurrence count overflowed"))?;
        }
        if entry.mount_point() == selector {
            selector_matches = selector_matches
                .checked_add(1)
                .ok_or_else(|| invalid_data("mount-point selector match count overflowed"))?;
            if selected.is_none() {
                selected = Some(entry);
            }
        }
    }
    budget.checkpoint()?;

    if selector_matches != 1 {
        return Err(invalid_data(format!(
            "mount-point selector matched {selector_matches} entries instead of exactly one"
        )));
    }
    let selected = selected
        .ok_or_else(|| invalid_data("mount-point selector count and retained selected entry are inconsistent"))?;
    if selected.mount_id() != expected_mount_id {
        return Err(invalid_data(format!(
            "selected mount ID {} does not equal expected mount ID {expected_mount_id}",
            selected.mount_id()
        )));
    }
    if expected_id_occurrences != 1 {
        return Err(invalid_data(format!(
            "expected mount ID {expected_mount_id} occurs {expected_id_occurrences} times instead of exactly once"
        )));
    }

    let selected_validation_work = selected
        .root()
        .len()
        .checked_add(4)
        .ok_or_else(|| invalid_data("selected mount validation work overflowed"))?;
    budget.charge(selected_validation_work, "validating selected mount authority")?;
    if selected.root() != b"/" {
        return Err(invalid_data("selected mount root is not exactly `/`"));
    }
    let device = selected.device();
    if device.major() != expected_device_major || device.minor() != expected_device_minor {
        return Err(invalid_data(format!(
            "selected mount device {}:{} does not equal expected device {expected_device_major}:{expected_device_minor}",
            device.major(),
            device.minor()
        )));
    }

    // No successful result may escape without a terminal deadline check after
    // the complete scan and all semantic predicates.
    budget.checkpoint()?;
    let work = budget.consumed();
    Ok((
        SelectedMountInfoAttachment {
            mount_id: selected.mount_id(),
            device_major: device.major(),
            device_minor: device.minor(),
            root: selected.root(),
            mount_point: selected.mount_point(),
        },
        work,
    ))
}

fn validate_selector(selector: &[u8], budget: &mut ScanBudget<'_, impl FnMut() -> Instant>) -> io::Result<()> {
    if selector.len() > MAX_SELECTOR_BYTES {
        return Err(invalid_input("mount-point selector exceeds 4095 bytes"));
    }
    budget.charge(selector.len(), "validating mount-point selector UTF-8")?;
    let selector_text =
        std::str::from_utf8(selector).map_err(|_| invalid_input("mount-point selector is not authored UTF-8"))?;

    budget.charge(selector.len(), "validating mount-point selector bytes")?;
    if selector.first() != Some(&b'/') {
        return Err(invalid_input("mount-point selector is not absolute"));
    }
    if selector == b"/" {
        return Err(invalid_input("filesystem root is not an attachment selector"));
    }
    if selector.contains(&0) {
        return Err(invalid_input("mount-point selector contains a NUL byte"));
    }

    let mut component_count = 0usize;
    for component in selector_text[1..].split('/') {
        let component_work = component
            .len()
            .checked_add(1)
            .ok_or_else(|| invalid_input("mount-point selector component work overflowed"))?;
        budget.charge(component_work, "validating mount-point selector component")?;
        component_count = component_count
            .checked_add(1)
            .ok_or_else(|| invalid_input("mount-point selector component count overflowed"))?;
        if component_count > MAX_SELECTOR_COMPONENTS {
            return Err(invalid_input("mount-point selector exceeds 128 components"));
        }
        if component.is_empty() {
            return Err(invalid_input(
                "mount-point selector contains an empty component, repeated slash, or trailing slash",
            ));
        }
        if matches!(component, "." | "..") {
            return Err(invalid_input(
                "mount-point selector contains a dot or dot-dot component",
            ));
        }
        if component.len() > MAX_SELECTOR_COMPONENT_BYTES {
            return Err(invalid_input("mount-point selector component exceeds 255 bytes"));
        }
    }
    budget.checkpoint()
}

fn validate_limits(limits: MountInfoAttachmentLimits) -> io::Result<()> {
    if limits.max_entries == 0 || limits.max_work == 0 {
        return Err(invalid_input("mountinfo attachment limits must be nonzero"));
    }
    Ok(())
}

struct ScanBudget<'a, Clock> {
    remaining: usize,
    initial: usize,
    deadline: Instant,
    clock: &'a mut Clock,
}

impl<'a, Clock: FnMut() -> Instant> ScanBudget<'a, Clock> {
    fn new(limit: usize, deadline: Instant, clock: &'a mut Clock) -> Self {
        Self {
            remaining: limit,
            initial: limit,
            deadline,
            clock,
        }
    }

    fn charge(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.checkpoint()?;
        self.remaining = self.remaining.checked_sub(amount).ok_or_else(|| {
            invalid_data(format!(
                "mountinfo attachment scan exceeded its {} unit work limit while {action}",
                self.initial
            ))
        })?;
        self.checkpoint()
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        require_deadline(self.deadline, self.clock)
    }

    const fn consumed(&self) -> usize {
        self.initial - self.remaining
    }
}

fn require_deadline(deadline: Instant, clock: &mut impl FnMut() -> Instant) -> io::Result<()> {
    if clock() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "mountinfo attachment scan exceeded its deadline",
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

#[cfg(test)]
pub(super) fn select_mountinfo_attachment_with_test_limits_and_clock<'a>(
    mountinfo: &'a MountInfo,
    selector: &[u8],
    expected_mount_id: u64,
    expected_device_major: u32,
    expected_device_minor: u32,
    limits: MountInfoAttachmentLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<(SelectedMountInfoAttachment<'a>, usize)> {
    select_mountinfo_attachment_with_limits_and_clock(
        mountinfo,
        selector,
        expected_mount_id,
        expected_device_major,
        expected_device_minor,
        limits,
        deadline,
        clock,
    )
}
