use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use super::super::{
    mountinfo::{MountInfo, parse_mountinfo_bytes},
    mountinfo_attachment::{SelectedMountInfoAttachment, select_mountinfo_attachment_until},
    mountinfo_boot_policy::{
        BOOT_MOUNTINFO_POLICY_LIMITS, BootFilesystemKind, BootMountInfoPolicyError, BootMountInfoPolicyLimits,
        MountOptionDomain, RequiredBootMountFlag, ValidatedBootMountInfoPolicy,
        validate_selected_boot_mount_policy_until, validate_selected_boot_mount_policy_with_test_limits_and_clock,
    },
};

const SELECTOR: &[u8] = b"/synthetic/boot-policy";
const MOUNT_ID: u64 = 91;
const MAJOR: u32 = 259;
const MINOR: u32 = 17;
const REQUIRED_MOUNT_OPTIONS: &str = "rw,nosuid,nodev,noexec,nosymfollow";

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn record(
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
        "{MOUNT_ID} 1 {MAJOR}:{MINOR} / {} {mount_options}{optional} - {filesystem_type} {source} {super_options}\n",
        std::str::from_utf8(SELECTOR).unwrap(),
    )
    .into_bytes()
}

fn parsed(bytes: &[u8]) -> MountInfo {
    parse_mountinfo_bytes(bytes).unwrap()
}

fn selected(mountinfo: &MountInfo) -> SelectedMountInfoAttachment<'_> {
    select_mountinfo_attachment_until(mountinfo, SELECTOR, MOUNT_ID, MAJOR, MINOR, future_deadline()).unwrap()
}

fn validate(bytes: &[u8]) -> Result<ValidatedBootMountInfoPolicy, BootMountInfoPolicyError> {
    let mountinfo = parsed(bytes);
    validate_selected_boot_mount_policy_until(selected(&mountinfo), future_deadline())
}

fn stable_record() -> Vec<u8> {
    record(REQUIRED_MOUNT_OPTIONS, "", "vfat", "ignored", "rw")
}

#[test]
fn exact_vfat_policy_retains_every_required_fact() {
    let policy = validate(&stable_record()).unwrap();
    assert_eq!(policy.filesystem(), BootFilesystemKind::Vfat);
    assert!(policy.mount_read_write());
    assert!(policy.superblock_read_write());
    assert!(policy.nosuid());
    assert!(policy.nodev());
    assert!(policy.noexec());
    assert!(policy.nosymfollow());
}

#[test]
fn filesystem_type_is_exact_and_closed() {
    for filesystem in ["msdos", "VFAT", "vfat.subtype", "fuse.vfat", "futurefs"] {
        assert_eq!(
            validate(&record(REQUIRED_MOUNT_OPTIONS, "", filesystem, "ignored", "rw")),
            Err(BootMountInfoPolicyError::UnsupportedFilesystem),
            "filesystem {filesystem} unexpectedly passed",
        );
    }
}

#[test]
fn mount_and_superblock_each_require_one_unopposed_rw() {
    let cases = [
        ("nosuid,nodev,noexec,nosymfollow", "rw", MountOptionDomain::Mount, 0, 0),
        (
            "rw,rw,nosuid,nodev,noexec,nosymfollow",
            "rw",
            MountOptionDomain::Mount,
            2,
            0,
        ),
        (
            "rw,ro,nosuid,nodev,noexec,nosymfollow",
            "rw",
            MountOptionDomain::Mount,
            1,
            1,
        ),
        (REQUIRED_MOUNT_OPTIONS, "relatime", MountOptionDomain::Superblock, 0, 0),
        (REQUIRED_MOUNT_OPTIONS, "rw,rw", MountOptionDomain::Superblock, 2, 0),
        (REQUIRED_MOUNT_OPTIONS, "rw,ro", MountOptionDomain::Superblock, 1, 1),
    ];
    for (mount, superblock, domain, rw_count, ro_count) in cases {
        assert_eq!(
            validate(&record(mount, "", "vfat", "ignored", superblock)),
            Err(BootMountInfoPolicyError::InvalidReadWriteState {
                domain,
                rw_count,
                ro_count,
            }),
        );
    }
}

#[test]
fn each_security_flag_is_required_once_without_inverse() {
    let cases = [
        ("rw,nodev,noexec,nosymfollow", RequiredBootMountFlag::Nosuid, 0, 0),
        (
            "rw,nosuid,nosuid,nodev,noexec,nosymfollow",
            RequiredBootMountFlag::Nosuid,
            2,
            0,
        ),
        (
            "rw,nosuid,suid,nodev,noexec,nosymfollow",
            RequiredBootMountFlag::Nosuid,
            1,
            1,
        ),
        ("rw,nosuid,noexec,nosymfollow", RequiredBootMountFlag::Nodev, 0, 0),
        (
            "rw,nosuid,nodev,nodev,noexec,nosymfollow",
            RequiredBootMountFlag::Nodev,
            2,
            0,
        ),
        (
            "rw,nosuid,nodev,dev,noexec,nosymfollow",
            RequiredBootMountFlag::Nodev,
            1,
            1,
        ),
        ("rw,nosuid,nodev,nosymfollow", RequiredBootMountFlag::Noexec, 0, 0),
        (
            "rw,nosuid,nodev,noexec,noexec,nosymfollow",
            RequiredBootMountFlag::Noexec,
            2,
            0,
        ),
        (
            "rw,nosuid,nodev,noexec,exec,nosymfollow",
            RequiredBootMountFlag::Noexec,
            1,
            1,
        ),
        ("rw,nosuid,nodev,noexec", RequiredBootMountFlag::Nosymfollow, 0, 0),
        (
            "rw,nosuid,nodev,noexec,nosymfollow,nosymfollow",
            RequiredBootMountFlag::Nosymfollow,
            2,
            0,
        ),
        (
            "rw,nosuid,nodev,noexec,nosymfollow,symfollow",
            RequiredBootMountFlag::Nosymfollow,
            1,
            1,
        ),
    ];
    for (mount, flag, required_count, inverse_count) in cases {
        assert_eq!(
            validate(&record(mount, "", "vfat", "ignored", "rw")),
            Err(BootMountInfoPolicyError::InvalidSecurityFlagState {
                flag,
                required_count,
                inverse_count,
            }),
        );
    }
}

#[test]
fn superblock_security_options_cannot_satisfy_per_mount_policy() {
    assert_eq!(
        validate(&record(
            "rw,nodev,noexec,nosymfollow",
            "",
            "vfat",
            "ignored",
            "rw,nosuid",
        )),
        Err(BootMountInfoPolicyError::InvalidSecurityFlagState {
            flag: RequiredBootMountFlag::Nosuid,
            required_count: 0,
            inverse_count: 0,
        }),
    );
}

#[test]
fn unrelated_options_source_and_optional_fields_do_not_change_policy() {
    let first = validate(&record(
        "rw,nosuid,nodev,noexec,nosymfollow,relatime",
        "shared:7",
        "vfat",
        "source-a",
        "rw,flush",
    ))
    .unwrap();
    let second = validate(&record(
        "rw,nosuid,nodev,noexec,nosymfollow,noatime",
        "unbindable",
        "vfat",
        "source-b",
        "rw,utf8",
    ))
    .unwrap();
    assert_eq!(first, second);
}

#[test]
fn option_limit_admits_n_and_rejects_n_plus_one() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let mut clock = || deadline;
    let limits = BootMountInfoPolicyLimits {
        max_options: 5,
        ..BOOT_MOUNTINFO_POLICY_LIMITS
    };
    validate_selected_boot_mount_policy_with_test_limits_and_clock(selected(&mountinfo), limits, deadline, &mut clock)
        .unwrap();

    let mut clock = || deadline;
    let error = validate_selected_boot_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        BootMountInfoPolicyLimits {
            max_options: 4,
            ..limits
        },
        deadline,
        &mut clock,
    )
    .unwrap_err();
    assert_eq!(
        error,
        BootMountInfoPolicyError::OptionLimitExceeded {
            domain: MountOptionDomain::Mount,
            limit: 4,
        }
    );
}

#[test]
fn work_limit_admits_exact_consumption_and_rejects_one_less() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let mut clock = || deadline;
    let (_, work) = validate_selected_boot_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        BOOT_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut clock,
    )
    .unwrap();
    let exact = BootMountInfoPolicyLimits {
        max_work: work,
        ..BOOT_MOUNTINFO_POLICY_LIMITS
    };
    let mut clock = || deadline;
    validate_selected_boot_mount_policy_with_test_limits_and_clock(selected(&mountinfo), exact, deadline, &mut clock)
        .unwrap();
    let mut clock = || deadline;
    assert!(matches!(
        validate_selected_boot_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            BootMountInfoPolicyLimits { max_work: work - 1, ..exact },
            deadline,
            &mut clock,
        ),
        Err(BootMountInfoPolicyError::WorkLimitExceeded { limit, .. }) if limit == work - 1
    ));
}

#[test]
fn deadline_equality_is_admitted_and_later_time_is_rejected() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    let mut equal_clock = || deadline;
    validate_selected_boot_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        BOOT_MOUNTINFO_POLICY_LIMITS,
        deadline,
        &mut equal_clock,
    )
    .unwrap();

    let mut late_clock = || deadline + Duration::from_nanos(1);
    assert_eq!(
        validate_selected_boot_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            BOOT_MOUNTINFO_POLICY_LIMITS,
            deadline,
            &mut late_clock,
        )
        .unwrap_err(),
        BootMountInfoPolicyError::DeadlineExceeded { deadline },
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
    validate_selected_boot_mount_policy_with_test_limits_and_clock(
        selected(&mountinfo),
        BOOT_MOUNTINFO_POLICY_LIMITS,
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
        validate_selected_boot_mount_policy_with_test_limits_and_clock(
            selected(&mountinfo),
            BOOT_MOUNTINFO_POLICY_LIMITS,
            deadline,
            &mut terminal_clock,
        )
        .unwrap_err(),
        BootMountInfoPolicyError::DeadlineExceeded { deadline },
    );
    assert_eq!(calls.get(), terminal_call);
}

#[test]
fn zero_option_or_work_limits_fail_before_policy_scan() {
    let mountinfo = parsed(&stable_record());
    let deadline = future_deadline();
    for limits in [
        BootMountInfoPolicyLimits {
            max_options: 0,
            ..BOOT_MOUNTINFO_POLICY_LIMITS
        },
        BootMountInfoPolicyLimits {
            max_work: 0,
            ..BOOT_MOUNTINFO_POLICY_LIMITS
        },
    ] {
        let calls = Cell::new(0usize);
        let mut clock = || {
            calls.set(calls.get() + 1);
            deadline
        };
        assert_eq!(
            validate_selected_boot_mount_policy_with_test_limits_and_clock(
                selected(&mountinfo),
                limits,
                deadline,
                &mut clock,
            )
            .unwrap_err(),
            BootMountInfoPolicyError::InvalidLimits,
        );
        assert_eq!(calls.get(), 1);
    }
}
