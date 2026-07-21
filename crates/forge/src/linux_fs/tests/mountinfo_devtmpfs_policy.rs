use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::{
    mountinfo::{MountInfo, parse_mountinfo_bytes},
    mountinfo_attachment::{SelectedMountInfoAttachment, select_mountinfo_attachment_until},
    mountinfo_devtmpfs_policy::{
        DEVTMPFS_MOUNTINFO_POLICY_LIMITS, DevtmpfsAccessMode, DevtmpfsFilesystemKind, DevtmpfsMountInfoPolicyError,
        DevtmpfsMountInfoPolicyLimits, DevtmpfsMountOptionDomain, ValidatedDevtmpfsMountInfoPolicy,
        validate_selected_devtmpfs_mount_policy_until,
        validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock,
    },
};

const DEV_SELECTOR: &[u8] = b"/dev";
const MOUNT_ID: u64 = 73;
const MAJOR: u32 = 0;
const MINOR: u32 = 5;

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn record(
    mount_id: u64,
    major: u32,
    minor: u32,
    root: &str,
    mount_point: &str,
    mount_options: &str,
    optional_fields: &str,
    filesystem_type: &str,
    source: &str,
    super_options: &str,
) -> Vec<u8> {
    let optional = if optional_fields.is_empty() {
        String::new()
    } else {
        format!(" {optional_fields}")
    };
    format!(
        "{mount_id} 1 {major}:{minor} {root} {mount_point} {mount_options}{optional} - {filesystem_type} {source} {super_options}\n"
    )
    .into_bytes()
}

fn stable_record() -> Vec<u8> {
    record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "rw,nosuid",
        "",
        "devtmpfs",
        "devtmpfs",
        "rw,size=4096k",
    )
}

fn parsed(bytes: &[u8]) -> MountInfo {
    parse_mountinfo_bytes(bytes).unwrap()
}

fn selected_at<'a>(
    mountinfo: &'a MountInfo,
    selector: &[u8],
    mount_id: u64,
    major: u32,
    minor: u32,
) -> io::Result<SelectedMountInfoAttachment<'a>> {
    select_mountinfo_attachment_until(mountinfo, selector, mount_id, major, minor, future_deadline())
}

fn selected(mountinfo: &MountInfo) -> SelectedMountInfoAttachment<'_> {
    selected_at(mountinfo, DEV_SELECTOR, MOUNT_ID, MAJOR, MINOR).unwrap()
}

fn validate(bytes: &[u8]) -> Result<ValidatedDevtmpfsMountInfoPolicy, DevtmpfsMountInfoPolicyError> {
    let mountinfo = parsed(bytes);
    validate_selected_devtmpfs_mount_policy_until(selected(&mountinfo), future_deadline())
}

#[test]
fn exact_devtmpfs_policy_retains_only_closed_scalar_identity_and_mode() {
    let policy = validate(&stable_record()).unwrap();
    assert_eq!(policy.filesystem(), DevtmpfsFilesystemKind::Devtmpfs);
    assert_eq!(policy.access_mode(), DevtmpfsAccessMode::ReadWrite);
    assert_eq!(policy.mount_id(), MOUNT_ID);
    assert_eq!((policy.device_major(), policy.device_minor()), (MAJOR, MINOR));
}

#[test]
fn exact_read_only_devtmpfs_is_admitted_for_a_reader() {
    let policy = validate(&record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "ro,nosuid",
        "",
        "devtmpfs",
        "ignored",
        "ro,size=4096k",
    ))
    .unwrap();
    assert_eq!(policy.access_mode(), DevtmpfsAccessMode::ReadOnly);
}

#[test]
fn filesystem_type_is_exactly_devtmpfs_and_never_tmpfs() {
    for filesystem in ["tmpfs", "DEVtmpfs", "devtmpfs.subtype", "fuse.devtmpfs", "futurefs"] {
        assert_eq!(
            validate(&record(
                MOUNT_ID, MAJOR, MINOR, "/", "/dev", "rw", "", filesystem, "ignored", "rw",
            )),
            Err(DevtmpfsMountInfoPolicyError::UnsupportedFilesystem),
            "filesystem {filesystem} unexpectedly passed",
        );
    }
}

#[test]
fn policy_accepts_no_arbitrary_mount_point_argument_or_attachment() {
    let mountinfo = parsed(&record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/synthetic/dev",
        "rw",
        "",
        "devtmpfs",
        "ignored",
        "rw",
    ));
    let arbitrary = selected_at(&mountinfo, b"/synthetic/dev", MOUNT_ID, MAJOR, MINOR).unwrap();
    assert_eq!(
        validate_selected_devtmpfs_mount_policy_until(arbitrary, future_deadline()),
        Err(DevtmpfsMountInfoPolicyError::UnexpectedMountPoint),
    );
}

#[test]
fn subroot_and_explicit_bind_semantics_are_rejected() {
    let subroot = parsed(&record(
        MOUNT_ID, MAJOR, MINOR, "/devices", "/dev", "rw", "", "devtmpfs", "ignored", "rw",
    ));
    assert_eq!(
        selected_at(&subroot, DEV_SELECTOR, MOUNT_ID, MAJOR, MINOR)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData,
    );

    assert_eq!(
        validate(&record(
            MOUNT_ID, MAJOR, MINOR, "/", "/dev", "rw,bind", "", "devtmpfs", "ignored", "rw",
        )),
        Err(DevtmpfsMountInfoPolicyError::BindSemantics {
            domain: DevtmpfsMountOptionDomain::Mount,
            bind_count: 1,
            rbind_count: 0,
        }),
    );
}

#[test]
fn access_mode_tokens_are_unique_unopposed_and_cross_domain_consistent() {
    let cases = [
        (
            "relatime",
            "rw",
            DevtmpfsMountInfoPolicyError::InvalidAccessMode {
                domain: DevtmpfsMountOptionDomain::Mount,
                rw_count: 0,
                ro_count: 0,
            },
        ),
        (
            "rw,rw",
            "rw",
            DevtmpfsMountInfoPolicyError::InvalidAccessMode {
                domain: DevtmpfsMountOptionDomain::Mount,
                rw_count: 2,
                ro_count: 0,
            },
        ),
        (
            "rw,ro",
            "rw",
            DevtmpfsMountInfoPolicyError::InvalidAccessMode {
                domain: DevtmpfsMountOptionDomain::Mount,
                rw_count: 1,
                ro_count: 1,
            },
        ),
        (
            "rw",
            "relatime",
            DevtmpfsMountInfoPolicyError::InvalidAccessMode {
                domain: DevtmpfsMountOptionDomain::Superblock,
                rw_count: 0,
                ro_count: 0,
            },
        ),
        (
            "rw",
            "ro",
            DevtmpfsMountInfoPolicyError::AccessModeMismatch {
                mount: DevtmpfsAccessMode::ReadWrite,
                superblock: DevtmpfsAccessMode::ReadOnly,
            },
        ),
    ];
    for (mount_options, super_options, expected) in cases {
        assert_eq!(
            validate(&record(
                MOUNT_ID,
                MAJOR,
                MINOR,
                "/",
                "/dev",
                mount_options,
                "",
                "devtmpfs",
                "ignored",
                super_options,
            )),
            Err(expected),
        );
    }
}

#[test]
fn bind_and_rbind_tokens_are_rejected_in_both_option_domains() {
    let cases = [
        ("rw,bind", "rw", DevtmpfsMountOptionDomain::Mount, 1, 0),
        ("rw,rbind", "rw", DevtmpfsMountOptionDomain::Mount, 0, 1),
        ("rw", "rw,bind", DevtmpfsMountOptionDomain::Superblock, 1, 0),
        ("rw", "rw,rbind", DevtmpfsMountOptionDomain::Superblock, 0, 1),
    ];
    for (mount_options, super_options, domain, bind_count, rbind_count) in cases {
        assert_eq!(
            validate(&record(
                MOUNT_ID,
                MAJOR,
                MINOR,
                "/",
                "/dev",
                mount_options,
                "",
                "devtmpfs",
                "ignored",
                super_options,
            )),
            Err(DevtmpfsMountInfoPolicyError::BindSemantics {
                domain,
                bind_count,
                rbind_count,
            }),
        );
    }
}

#[test]
fn nodev_and_duplicate_or_opposed_dev_tokens_are_rejected() {
    let cases = [
        ("rw,nodev", "rw", DevtmpfsMountOptionDomain::Mount, 0, 1),
        ("rw,dev,dev", "rw", DevtmpfsMountOptionDomain::Mount, 2, 0),
        ("rw,dev,nodev", "rw", DevtmpfsMountOptionDomain::Mount, 1, 1),
        ("rw", "rw,nodev", DevtmpfsMountOptionDomain::Superblock, 0, 1),
    ];
    for (mount_options, super_options, domain, dev_count, nodev_count) in cases {
        assert_eq!(
            validate(&record(
                MOUNT_ID,
                MAJOR,
                MINOR,
                "/",
                "/dev",
                mount_options,
                "",
                "devtmpfs",
                "ignored",
                super_options,
            )),
            Err(DevtmpfsMountInfoPolicyError::InvalidDeviceSemantics {
                domain,
                dev_count,
                nodev_count,
            }),
        );
    }

    validate(&record(
        MOUNT_ID, MAJOR, MINOR, "/", "/dev", "rw,dev", "", "devtmpfs", "ignored", "rw,dev",
    ))
    .unwrap();
}

#[test]
fn unrelated_options_source_and_optional_fields_do_not_change_policy() {
    let first = validate(&record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "rw,nosuid,noexec",
        "shared:7",
        "devtmpfs",
        "source-a",
        "rw,size=4096k,mode=755",
    ))
    .unwrap();
    let second = validate(&record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "rw,relatime",
        "unbindable",
        "devtmpfs",
        "source-b",
        "rw,nr_inodes=1024",
    ))
    .unwrap();
    assert_eq!(first, second);
}

#[test]
fn selected_mount_id_and_device_identity_are_preserved_as_scalars() {
    let alternate_id = MOUNT_ID + 10;
    let alternate_major = MAJOR + 1;
    let alternate_minor = MINOR + 2;
    let mountinfo = parsed(&record(
        alternate_id,
        alternate_major,
        alternate_minor,
        "/",
        "/dev",
        "rw",
        "",
        "devtmpfs",
        "ignored",
        "rw",
    ));
    let selected = selected_at(&mountinfo, DEV_SELECTOR, alternate_id, alternate_major, alternate_minor).unwrap();
    let policy = validate_selected_devtmpfs_mount_policy_until(selected, future_deadline()).unwrap();
    assert_eq!(policy.mount_id(), alternate_id);
    assert_eq!(
        (policy.device_major(), policy.device_minor()),
        (alternate_major, alternate_minor),
    );
}

#[test]
fn option_limit_admits_exact_n_and_rejects_n_minus_one() {
    let bytes = record(
        MOUNT_ID,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "rw,nosuid,noexec",
        "",
        "devtmpfs",
        "ignored",
        "rw,size=4096k",
    );
    let mountinfo = parsed(&bytes);
    let deadline = future_deadline();
    let exact = DevtmpfsMountInfoPolicyLimits {
        max_options: 3,
        ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
    };
    let mut clock = || deadline;
    validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        exact,
        deadline,
        &mut clock,
    )
    .unwrap();

    let mut clock = || deadline;
    assert_eq!(
        validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            DevtmpfsMountInfoPolicyLimits {
                max_options: 2,
                ..exact
            },
            deadline,
            &mut clock,
        )
        .unwrap_err(),
        DevtmpfsMountInfoPolicyError::OptionLimitExceeded {
            domain: DevtmpfsMountOptionDomain::Mount,
            limit: 2,
        },
    );
}

#[test]
fn work_limit_admits_exact_n_and_rejects_n_minus_one() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let mut clock = || deadline;
    let (_, work) = validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut clock,
    )
    .unwrap();
    let exact = DevtmpfsMountInfoPolicyLimits {
        max_work: work,
        ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
    };
    let mut clock = || deadline;
    validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        exact,
        deadline,
        &mut clock,
    )
    .unwrap();

    let mut clock = || deadline;
    assert!(matches!(
        validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            DevtmpfsMountInfoPolicyLimits {
                max_work: work - 1,
                ..exact
            },
            deadline,
            &mut clock,
        ),
        Err(DevtmpfsMountInfoPolicyError::WorkLimitExceeded { limit, .. }) if limit == work - 1
    ));
}

#[test]
fn zero_and_overproduction_limits_fail_before_policy_scan() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let limits = [
        DevtmpfsMountInfoPolicyLimits {
            max_options: 0,
            ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
        },
        DevtmpfsMountInfoPolicyLimits {
            max_options: DEVTMPFS_MOUNTINFO_POLICY_LIMITS.max_options + 1,
            ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
        },
        DevtmpfsMountInfoPolicyLimits {
            max_work: 0,
            ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
        },
        DevtmpfsMountInfoPolicyLimits {
            max_work: DEVTMPFS_MOUNTINFO_POLICY_LIMITS.max_work + 1,
            ..DEVTMPFS_MOUNTINFO_POLICY_LIMITS
        },
    ];
    for limits in limits {
        let calls = Cell::new(0usize);
        let mut clock = || {
            calls.set(calls.get() + 1);
            deadline
        };
        assert_eq!(
            validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
                selected(&mountinfo),
                limits,
                deadline,
                &mut clock,
            )
            .unwrap_err(),
            DevtmpfsMountInfoPolicyError::InvalidLimits,
        );
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn deadline_equality_is_admitted_and_one_nanosecond_late_is_rejected() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let mut equal_clock = || deadline;
    validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut equal_clock,
    )
    .unwrap();

    let mut late_clock = || deadline + Duration::from_nanos(1);
    assert_eq!(
        validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
            deadline,
            &mut late_clock,
        )
        .unwrap_err(),
        DevtmpfsMountInfoPolicyError::DeadlineExceeded { deadline },
    );
}

#[test]
fn deadline_expiring_only_at_terminal_checkpoint_rejects_policy() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let calls = Cell::new(0usize);
    let mut counting_clock = || {
        calls.set(calls.get() + 1);
        deadline
    };
    validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut counting_clock,
    )
    .unwrap();
    let terminal_call = calls.get();

    let calls = Cell::new(0usize);
    let mut terminal_clock = || {
        let call = calls.get() + 1;
        calls.set(call);
        if call == terminal_call {
            deadline + Duration::from_nanos(1)
        } else {
            deadline
        }
    };
    assert_eq!(
        validate_selected_devtmpfs_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            DEVTMPFS_MOUNTINFO_POLICY_LIMITS,
            deadline,
            &mut terminal_clock,
        )
        .unwrap_err(),
        DevtmpfsMountInfoPolicyError::DeadlineExceeded { deadline },
    );
    assert_eq!(calls.get(), terminal_call);
}

#[test]
fn malformed_mountinfo_and_ambiguous_dev_attachments_fail_before_policy() {
    for malformed in [
        b"73 1 0:5 / /dev rw,,nosuid - devtmpfs ignored rw\n".as_slice(),
        b"73 1 0:5 / /dev rw - devtmpfs ignored rw".as_slice(),
    ] {
        assert!(parse_mountinfo_bytes(malformed).is_err());
    }

    let opaque_root = parsed(b"73 1 0:5 mnt:[4026532758] /dev rw - devtmpfs ignored rw\n");
    assert_eq!(
        selected_at(&opaque_root, DEV_SELECTOR, MOUNT_ID, MAJOR, MINOR)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData,
    );

    let first = stable_record();
    let second = record(
        MOUNT_ID + 1,
        MAJOR,
        MINOR,
        "/",
        "/dev",
        "rw",
        "",
        "devtmpfs",
        "ignored",
        "rw",
    );
    let mut stacked = Vec::with_capacity(first.len() + second.len());
    stacked.extend_from_slice(&first);
    stacked.extend_from_slice(&second);
    let mountinfo = parsed(&stacked);
    assert_eq!(
        selected_at(&mountinfo, DEV_SELECTOR, MOUNT_ID, MAJOR, MINOR)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData,
    );
}
