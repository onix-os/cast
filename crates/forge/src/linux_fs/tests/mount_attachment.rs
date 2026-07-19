use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::{
    mountinfo::{MountInfo, parse_mountinfo_bytes},
    mountinfo_attachment::{
        MOUNTINFO_ATTACHMENT_LIMITS, MountInfoAttachmentLimits, SelectedMountInfoAttachment,
        select_mountinfo_attachment_until, select_mountinfo_attachment_with_test_limits_and_clock,
    },
};

const SELECTOR: &[u8] = b"/synthetic/esp-attachment";
const MOUNT_ID: u64 = 41;
const MAJOR: u32 = 259;
const MINOR: u32 = 7;
const STABLE: &[u8] = b"41 1 259:7 / /synthetic/esp-attachment rw,nosuid - vfat ignored-source rw\n";

fn parsed(bytes: &[u8]) -> MountInfo {
    parse_mountinfo_bytes(bytes).unwrap()
}

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn selected<'a>(mountinfo: &'a MountInfo, selector: &[u8]) -> io::Result<SelectedMountInfoAttachment<'a>> {
    select_mountinfo_attachment_until(mountinfo, selector, MOUNT_ID, MAJOR, MINOR, future_deadline())
}

fn select_with<'a>(
    mountinfo: &'a MountInfo,
    selector: &[u8],
    mount_id: u64,
    major: u32,
    minor: u32,
    limits: MountInfoAttachmentLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<(SelectedMountInfoAttachment<'a>, usize)> {
    select_mountinfo_attachment_with_test_limits_and_clock(
        mountinfo, selector, mount_id, major, minor, limits, deadline, clock,
    )
}

fn record(
    mount_id: u64,
    parent_id: u64,
    major: u32,
    minor: u32,
    root: &[u8],
    mount_point: &[u8],
    filesystem_type: &[u8],
    source: &[u8],
) -> Vec<u8> {
    let mut bytes = format!("{mount_id} {parent_id} {major}:{minor} ").into_bytes();
    bytes.extend_from_slice(root);
    bytes.push(b' ');
    bytes.extend_from_slice(mount_point);
    bytes.extend_from_slice(b" rw - ");
    bytes.extend_from_slice(filesystem_type);
    bytes.push(b' ');
    bytes.extend_from_slice(source);
    bytes.extend_from_slice(b" rw\n");
    bytes
}

fn joined(records: &[Vec<u8>]) -> Vec<u8> {
    let total = records.iter().map(Vec::len).sum();
    let mut bytes = Vec::with_capacity(total);
    for record in records {
        bytes.extend_from_slice(record);
    }
    bytes
}

fn assert_invalid_data(result: io::Result<SelectedMountInfoAttachment<'_>>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

fn assert_invalid_input(result: io::Result<SelectedMountInfoAttachment<'_>>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn stable_snapshot_selects_only_exact_attachment_semantics() {
    let mountinfo = parsed(STABLE);
    let attachment = selected(&mountinfo, SELECTOR).unwrap();

    assert_eq!(attachment.mount_id(), MOUNT_ID);
    assert_eq!((attachment.device_major(), attachment.device_minor()), (MAJOR, MINOR));
    assert_eq!(attachment.root(), b"/");
    assert_eq!(attachment.mount_point(), SELECTOR);
}

#[test]
fn generic_selection_identity_ignores_filesystem_policy_and_source() {
    let first = parsed(b"41 1 259:7 / /synthetic/esp-attachment rw,nosuid shared:8 - vfat source-a rw,flush\n");
    let second =
        parsed(b"41 99 259:7 / /synthetic/esp-attachment ro,nodev unbindable - futurefs source-b ro,opaque=value\n");

    let first_selected = selected(&first, SELECTOR).unwrap();
    let second_selected = selected(&second, SELECTOR).unwrap();
    assert_eq!(first_selected, second_selected);
}

#[test]
fn unrelated_mount_table_churn_does_not_change_the_selected_view() {
    let before = parsed(&joined(&[
        record(MOUNT_ID, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"source"),
        record(60, 1, 0, 60, b"/", b"/unrelated-a", b"tmpfs", b"none"),
    ]));
    let after = parsed(&joined(&[
        record(70, 1, 0, 70, b"/", b"/unrelated-b", b"futurefs", b"changed"),
        record(MOUNT_ID, 999, MAJOR, MINOR, b"/", SELECTOR, b"otherfs", b"other-source"),
        record(71, 1, 0, 71, b"/", b"/unrelated-c", b"tmpfs", b"none"),
    ]));

    assert_eq!(
        selected(&before, SELECTOR).unwrap(),
        selected(&after, SELECTOR).unwrap()
    );
}

#[test]
fn escaped_mount_point_is_compared_as_exact_decoded_bytes() {
    let mountinfo = parsed(b"41 1 259:7 / /synthetic/boot\\040efi rw - vfat ignored rw\n");

    assert_eq!(
        selected(&mountinfo, b"/synthetic/boot efi").unwrap().mount_point(),
        b"/synthetic/boot efi"
    );
    assert_invalid_data(selected(&mountinfo, b"/synthetic/boot\\040efi"));
}

#[test]
fn invalid_utf8_mount_point_never_matches_or_gets_reinterpreted() {
    let mut bytes = b"41 1 259:7 / /synthetic/esp-invalid/".to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice(b" rw - vfat ignored rw\n");
    let mountinfo = parsed(&bytes);

    assert_invalid_data(selected(&mountinfo, b"/synthetic/esp-invalid/x"));
    let invalid_selector = b"/synthetic/esp-invalid/\xff";
    assert_invalid_input(selected(&mountinfo, invalid_selector));
}

#[test]
fn missing_selector_is_rejected_without_fallback() {
    let mountinfo = parsed(STABLE);
    assert_invalid_data(selected(&mountinfo, b"/synthetic/missing-boot"));
}

#[test]
fn full_scan_rejects_a_later_stacked_selector_match() {
    let mountinfo = parsed(&joined(&[
        record(MOUNT_ID, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"first"),
        record(88, 1, 8, 8, b"/", b"/unrelated", b"tmpfs", b"none"),
        record(89, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"later-stack"),
    ]));

    let error = selected(&mountinfo, SELECTOR).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("matched 2 entries"));
}

#[test]
fn selected_mount_id_must_equal_the_expected_unique_id() {
    let mountinfo = parsed(&joined(&[
        record(40, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"selected"),
        record(
            MOUNT_ID,
            1,
            MAJOR,
            MINOR,
            b"/",
            b"/different",
            b"vfat",
            b"expected-elsewhere",
        ),
    ]));
    assert_invalid_data(selected(&mountinfo, SELECTOR));

    let deadline = future_deadline();
    let mut clock = || deadline;
    let error = select_with(
        &parsed(STABLE),
        SELECTOR,
        0,
        MAJOR,
        MINOR,
        MOUNTINFO_ATTACHMENT_LIMITS,
        deadline,
        &mut clock,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn selected_root_must_be_exact_partition_root() {
    let mountinfo = parsed(&record(
        MOUNT_ID,
        1,
        MAJOR,
        MINOR,
        b"/bound-subdir",
        SELECTOR,
        b"vfat",
        b"ignored",
    ));
    assert_invalid_data(selected(&mountinfo, SELECTOR));
}

#[test]
fn selected_device_major_and_minor_are_both_exact() {
    let mountinfo = parsed(STABLE);
    assert_eq!(
        select_mountinfo_attachment_until(&mountinfo, SELECTOR, MOUNT_ID, MAJOR + 1, MINOR, future_deadline(),)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        select_mountinfo_attachment_until(&mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR + 1, future_deadline(),)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn malformed_selectors_are_defense_rejected_before_lookup() {
    let mountinfo = parsed(STABLE);
    let malformed = [
        b"".as_slice(),
        b"relative",
        b"/",
        b"/synthetic/esp\0nested",
        b"/synthetic/esp/",
        b"/synthetic/esp//nested",
        b"/synthetic/esp/./nested",
        b"/synthetic/esp/../nested",
    ];
    for selector in malformed {
        assert_invalid_input(selected(&mountinfo, selector));
    }
}

#[test]
fn selector_byte_component_and_component_count_bounds_are_exact() {
    let exact_total = selector_with_total_bytes(4_095);
    let over_total = selector_with_total_bytes(4_096);
    assert_eq!(exact_total.len(), 4_095);
    assert_eq!(over_total.len(), 4_096);
    assert_selector_accepted(exact_total.as_bytes());
    assert_invalid_input(selected(&parsed(STABLE), over_total.as_bytes()));

    let exact_component = format!("/synthetic/{}", "c".repeat(255));
    let over_component = format!("/synthetic/{}", "c".repeat(256));
    assert_selector_accepted(exact_component.as_bytes());
    assert_invalid_input(selected(&parsed(STABLE), over_component.as_bytes()));

    let exact_components = format!("/{}", vec!["c"; 128].join("/"));
    let over_components = format!("/{}", vec!["c"; 129].join("/"));
    assert_selector_accepted(exact_components.as_bytes());
    assert_invalid_input(selected(&parsed(STABLE), over_components.as_bytes()));
}

fn selector_with_total_bytes(total: usize) -> String {
    let mut remaining = total - 1;
    let mut components = Vec::new();
    while remaining > 0 {
        let component_bytes = remaining.min(255);
        components.push("c".repeat(component_bytes));
        remaining -= component_bytes;
        if remaining > 0 {
            remaining -= 1;
        }
    }
    format!("/{}", components.join("/"))
}

fn assert_selector_accepted(selector: &[u8]) {
    let mountinfo = parsed(&record(MOUNT_ID, 1, MAJOR, MINOR, b"/", selector, b"vfat", b"ignored"));
    assert_eq!(selected(&mountinfo, selector).unwrap().mount_point(), selector);
}

#[test]
fn entry_ceiling_admits_n_and_rejects_n_plus_one() {
    let two = parsed(&joined(&[
        record(MOUNT_ID, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"selected"),
        record(50, 1, 0, 50, b"/", b"/unrelated", b"tmpfs", b"none"),
    ]));
    let three = parsed(&joined(&[
        record(MOUNT_ID, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"selected"),
        record(50, 1, 0, 50, b"/", b"/unrelated", b"tmpfs", b"none"),
        record(51, 1, 0, 51, b"/", b"/later", b"tmpfs", b"none"),
    ]));
    let limits = MountInfoAttachmentLimits {
        max_entries: 2,
        ..MOUNTINFO_ATTACHMENT_LIMITS
    };
    let deadline = future_deadline();
    let mut clock = || deadline;
    select_with(&two, SELECTOR, MOUNT_ID, MAJOR, MINOR, limits, deadline, &mut clock).unwrap();
    assert_eq!(
        select_with(&three, SELECTOR, MOUNT_ID, MAJOR, MINOR, limits, deadline, &mut clock)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn work_ceiling_admits_exact_consumption_and_rejects_one_less() {
    let mountinfo = parsed(&joined(&[
        record(50, 1, 0, 50, b"/", b"/before", b"tmpfs", b"none"),
        record(MOUNT_ID, 1, MAJOR, MINOR, b"/", SELECTOR, b"vfat", b"selected"),
        record(51, 1, 0, 51, b"/", b"/after", b"tmpfs", b"none"),
    ]));
    let deadline = future_deadline();
    let measuring = MountInfoAttachmentLimits {
        max_work: usize::MAX,
        ..MOUNTINFO_ATTACHMENT_LIMITS
    };
    let mut clock = || deadline;
    let (_, consumed) = select_with(
        &mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR, measuring, deadline, &mut clock,
    )
    .unwrap();
    assert!(consumed > SELECTOR.len());

    let exact = MountInfoAttachmentLimits {
        max_work: consumed,
        ..MOUNTINFO_ATTACHMENT_LIMITS
    };
    let (_, exact_consumed) = select_with(
        &mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR, exact, deadline, &mut clock,
    )
    .unwrap();
    assert_eq!(exact_consumed, consumed);

    let short = MountInfoAttachmentLimits {
        max_work: consumed - 1,
        ..MOUNTINFO_ATTACHMENT_LIMITS
    };
    assert_eq!(
        select_with(
            &mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR, short, deadline, &mut clock
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn deadline_equality_is_admitted_and_one_nanosecond_late_is_rejected() {
    let mountinfo = parsed(STABLE);
    let deadline = future_deadline();
    let mut equal = || deadline;
    select_with(
        &mountinfo,
        SELECTOR,
        MOUNT_ID,
        MAJOR,
        MINOR,
        MOUNTINFO_ATTACHMENT_LIMITS,
        deadline,
        &mut equal,
    )
    .unwrap();

    let late_time = deadline.checked_add(Duration::from_nanos(1)).unwrap();
    let calls = Cell::new(0usize);
    let mut late = || {
        calls.set(calls.get() + 1);
        late_time
    };
    assert_eq!(
        select_with(
            &mountinfo,
            SELECTOR,
            MOUNT_ID,
            MAJOR,
            MINOR,
            MOUNTINFO_ATTACHMENT_LIMITS,
            deadline,
            &mut late,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn deadline_expiring_only_at_terminal_checkpoint_rejects_the_result() {
    let mountinfo = parsed(STABLE);
    let deadline = future_deadline();
    let checks = Cell::new(0usize);
    let mut counting_clock = || {
        checks.set(checks.get() + 1);
        deadline
    };
    select_with(
        &mountinfo,
        SELECTOR,
        MOUNT_ID,
        MAJOR,
        MINOR,
        MOUNTINFO_ATTACHMENT_LIMITS,
        deadline,
        &mut counting_clock,
    )
    .unwrap();
    let terminal_check = checks.get();
    assert!(terminal_check > 1);

    let late_time = deadline.checked_add(Duration::from_nanos(1)).unwrap();
    let replayed = Cell::new(0usize);
    let mut terminal_expiry = || {
        let call = replayed.get() + 1;
        replayed.set(call);
        if call == terminal_check { late_time } else { deadline }
    };
    assert_eq!(
        select_with(
            &mountinfo,
            SELECTOR,
            MOUNT_ID,
            MAJOR,
            MINOR,
            MOUNTINFO_ATTACHMENT_LIMITS,
            deadline,
            &mut terminal_expiry,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(replayed.get(), terminal_check);
}

#[test]
fn zero_entry_or_work_limits_fail_before_any_scan_clock() {
    let mountinfo = parsed(STABLE);
    let deadline = future_deadline();
    for limits in [
        MountInfoAttachmentLimits {
            max_entries: 0,
            ..MOUNTINFO_ATTACHMENT_LIMITS
        },
        MountInfoAttachmentLimits {
            max_work: 0,
            ..MOUNTINFO_ATTACHMENT_LIMITS
        },
    ] {
        let calls = Cell::new(0usize);
        let mut clock = || {
            calls.set(calls.get() + 1);
            deadline
        };
        assert_eq!(
            select_with(
                &mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR, limits, deadline, &mut clock
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(calls.get(), 1);
    }
}
