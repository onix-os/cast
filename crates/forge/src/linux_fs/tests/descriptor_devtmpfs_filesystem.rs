use std::{
    cell::{Cell, RefCell},
    io,
    time::{Duration, Instant},
};

use super::super::{
    descriptor_devtmpfs_filesystem::{
        DevtmpfsDescriptorAuthenticationError, DevtmpfsDescriptorMagicFamily, DevtmpfsDescriptorObservationPhase,
        FIXTURE_TMPFS_MAGIC, FixtureDevtmpfsDescriptorIdentity, FixtureDevtmpfsDescriptorLimits,
        FixtureDevtmpfsDescriptorObservations, FixtureDevtmpfsDescriptorUsage, FixtureDevtmpfsRawObservation,
        ValidatedDevtmpfsSameMountDescriptorEvidence, validate_fixture_devtmpfs_descriptor_authentication,
        validate_fixture_devtmpfs_descriptor_protocol,
    },
    mountinfo::parse_mountinfo_bytes,
    mountinfo_attachment::select_mountinfo_attachment_until,
    mountinfo_devtmpfs_policy::{
        DevtmpfsAccessMode, DevtmpfsFilesystemKind, ValidatedDevtmpfsMountInfoPolicy,
        validate_selected_devtmpfs_mount_policy_until,
    },
};

const MOUNT_ID: u64 = 73;
const MAJOR: u32 = 0;
const MINOR: u32 = 5;
const INODE: u64 = 101;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn device() -> u64 {
    nix::libc::makedev(MAJOR, MINOR)
}

fn policy(mount_id: u64, major: u32, minor: u32, access: &str) -> ValidatedDevtmpfsMountInfoPolicy {
    let record = format!("{mount_id} 1 {major}:{minor} / /dev {access} - devtmpfs devtmpfs {access}\n");
    let parsed = parse_mountinfo_bytes(record.as_bytes()).unwrap();
    let selected = select_mountinfo_attachment_until(&parsed, b"/dev", mount_id, major, minor, deadline()).unwrap();
    validate_selected_devtmpfs_mount_policy_until(selected, deadline()).unwrap()
}

fn stable_policy() -> ValidatedDevtmpfsMountInfoPolicy {
    policy(MOUNT_ID, MAJOR, MINOR, "rw")
}

fn stable_identity() -> FixtureDevtmpfsDescriptorIdentity {
    FixtureDevtmpfsDescriptorIdentity {
        device: device(),
        inode: INODE,
        kind: nix::libc::S_IFDIR,
    }
}

fn stable_observations() -> FixtureDevtmpfsDescriptorObservations {
    FixtureDevtmpfsDescriptorObservations {
        opening_identity: stable_identity(),
        opening_mount_id: MOUNT_ID,
        opening_magic: FIXTURE_TMPFS_MAGIC,
        closing_magic: FIXTURE_TMPFS_MAGIC,
        closing_mount_id: MOUNT_ID,
        closing_identity: stable_identity(),
    }
}

fn validate(
    observations: FixtureDevtmpfsDescriptorObservations,
) -> Result<
    (
        ValidatedDevtmpfsSameMountDescriptorEvidence,
        FixtureDevtmpfsDescriptorUsage,
    ),
    DevtmpfsDescriptorAuthenticationError,
> {
    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    validate_fixture_devtmpfs_descriptor_authentication(
        observations,
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
}

fn raw_observation(
    observations: FixtureDevtmpfsDescriptorObservations,
    phase: DevtmpfsDescriptorObservationPhase,
) -> FixtureDevtmpfsRawObservation {
    match phase {
        DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity => {
            FixtureDevtmpfsRawObservation::DirectoryIdentity(observations.opening_identity)
        }
        DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId => {
            FixtureDevtmpfsRawObservation::DescriptorMountId(observations.opening_mount_id)
        }
        DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic => {
            FixtureDevtmpfsRawObservation::FilesystemMagic(observations.opening_magic)
        }
        DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic => {
            FixtureDevtmpfsRawObservation::FilesystemMagic(observations.closing_magic)
        }
        DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId => {
            FixtureDevtmpfsRawObservation::DescriptorMountId(observations.closing_mount_id)
        }
        DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity => {
            FixtureDevtmpfsRawObservation::DirectoryIdentity(observations.closing_identity)
        }
    }
}

#[test]
fn stable_policy_and_six_phase_schedule_retain_closed_same_mount_evidence() {
    let deadline = deadline();
    let phases = RefCell::new(Vec::new());
    let mut hook = |phase| {
        phases.borrow_mut().push(phase);
        Ok(())
    };
    let mut clock = || deadline;
    let (evidence, usage) = validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
    .unwrap();

    assert_eq!(
        *phases.borrow(),
        [
            DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity,
            DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId,
            DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic,
            DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic,
            DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId,
            DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity,
        ]
    );
    assert_eq!(evidence.directory_device(), device());
    assert_eq!(evidence.directory_inode(), INODE);
    assert_eq!(evidence.mount_id(), MOUNT_ID);
    assert_eq!(evidence.filesystem(), DevtmpfsFilesystemKind::Devtmpfs);
    assert_eq!(evidence.access_mode(), DevtmpfsAccessMode::ReadWrite);
    assert_eq!(evidence.magic_family(), DevtmpfsDescriptorMagicFamily::LinuxTmpfs);
    assert_eq!(usage.observations, 6);
    assert!(usage.work > usage.observations);
}

#[test]
fn tmpfs_magic_is_shared_and_wrong_magic_is_rejected() {
    let observations = FixtureDevtmpfsDescriptorObservations {
        opening_magic: 0xEF53,
        closing_magic: 0xEF53,
        ..stable_observations()
    };
    assert!(matches!(
        validate(observations),
        Err(DevtmpfsDescriptorAuthenticationError::UnsupportedFilesystemMagic {
            expected: FIXTURE_TMPFS_MAGIC,
            found: 0xEF53,
        })
    ));

    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    let (read_only, _) = validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        policy(MOUNT_ID, MAJOR, MINOR, "ro"),
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
    .unwrap();
    assert_eq!(read_only.access_mode(), DevtmpfsAccessMode::ReadOnly);
}

#[test]
fn policy_identity_is_checked_canonically_before_observations() {
    let deadline = deadline();
    let hook_calls = Cell::new(0usize);
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };

    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            policy(MOUNT_ID + 1, MAJOR, MINOR, "rw"),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::PolicyMountIdMismatch {
            expected_mount_id: MOUNT_ID,
            policy_mount_id,
        }) if policy_mount_id == MOUNT_ID + 1
    ));
    assert_eq!(hook_calls.get(), 0);

    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            policy(MOUNT_ID, MAJOR + 1, MINOR, "rw"),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::PolicyDeviceMismatch {
            expected_major: MAJOR,
            expected_minor: MINOR,
            policy_major,
            policy_minor: MINOR,
        }) if policy_major == MAJOR + 1
    ));
    assert_eq!(hook_calls.get(), 0);

    let boundary = u64::MAX;
    let boundary_major = nix::libc::major(boundary);
    let boundary_minor = nix::libc::minor(boundary);
    assert_eq!(nix::libc::makedev(boundary_major, boundary_minor), boundary);
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            boundary,
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::PolicyDeviceMismatch {
            expected_major,
            expected_minor,
            policy_major: MAJOR,
            policy_minor: MINOR,
        }) if expected_major == boundary_major && expected_minor == boundary_minor
    ));
    assert_eq!(hook_calls.get(), 0);
}

#[test]
fn expected_identity_must_be_nonzero_before_observations() {
    let deadline = deadline();
    for (expected_device, expected_inode, expected_mount_id) in
        [(0, INODE, MOUNT_ID), (device(), 0, MOUNT_ID), (device(), INODE, 0)]
    {
        let hook_calls = Cell::new(0usize);
        let mut hook = |_| {
            hook_calls.set(hook_calls.get() + 1);
            Ok(())
        };
        let mut clock = || deadline;
        assert!(matches!(
            validate_fixture_devtmpfs_descriptor_authentication(
                stable_observations(),
                expected_device,
                expected_inode,
                expected_mount_id,
                stable_policy(),
                FixtureDevtmpfsDescriptorLimits::default(),
                deadline,
                &mut clock,
                &mut hook,
            ),
            Err(DevtmpfsDescriptorAuthenticationError::InvalidExpectedIdentity { .. })
        ));
        assert_eq!(hook_calls.get(), 0);
    }
}

#[test]
fn observed_identity_must_be_nonzero_stable_and_exact() {
    let zero = FixtureDevtmpfsDescriptorObservations {
        opening_identity: FixtureDevtmpfsDescriptorIdentity {
            device: 0,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(zero),
        Err(DevtmpfsDescriptorAuthenticationError::InvalidObservedIdentity {
            phase: DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity,
            device: 0,
            inode: INODE,
        })
    ));

    let drift = FixtureDevtmpfsDescriptorObservations {
        closing_identity: FixtureDevtmpfsDescriptorIdentity {
            inode: INODE + 1,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(DevtmpfsDescriptorAuthenticationError::DirectoryIdentityDrift {
            closing_inode,
            ..
        }) if closing_inode == INODE + 1
    ));

    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE + 1,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::UnexpectedDirectoryIdentity {
            expected_inode,
            found_inode: INODE,
            ..
        }) if expected_inode == INODE + 1
    ));
}

#[test]
fn directory_kind_must_be_stable_and_exact() {
    let wrong = FixtureDevtmpfsDescriptorObservations {
        opening_identity: FixtureDevtmpfsDescriptorIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        closing_identity: FixtureDevtmpfsDescriptorIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(wrong),
        Err(DevtmpfsDescriptorAuthenticationError::UnsupportedDirectoryKind {
            expected: nix::libc::S_IFDIR,
            found: nix::libc::S_IFREG,
        })
    ));

    let drift = FixtureDevtmpfsDescriptorObservations {
        closing_identity: FixtureDevtmpfsDescriptorIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(DevtmpfsDescriptorAuthenticationError::DirectoryKindDrift {
            opening: nix::libc::S_IFDIR,
            closing: nix::libc::S_IFREG,
        })
    ));
}

#[test]
fn descriptor_mount_id_must_be_nonzero_stable_and_exact() {
    let zero = FixtureDevtmpfsDescriptorObservations {
        opening_mount_id: 0,
        ..stable_observations()
    };
    assert!(matches!(
        validate(zero),
        Err(DevtmpfsDescriptorAuthenticationError::InvalidObservedMountId {
            phase: DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId,
        })
    ));

    let drift = FixtureDevtmpfsDescriptorObservations {
        closing_mount_id: MOUNT_ID + 1,
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(DevtmpfsDescriptorAuthenticationError::DescriptorMountIdDrift {
            opening: MOUNT_ID,
            closing,
        }) if closing == MOUNT_ID + 1
    ));

    let mismatch = FixtureDevtmpfsDescriptorObservations {
        opening_mount_id: MOUNT_ID + 1,
        closing_mount_id: MOUNT_ID + 1,
        ..stable_observations()
    };
    assert!(matches!(
        validate(mismatch),
        Err(DevtmpfsDescriptorAuthenticationError::UnexpectedDescriptorMountId {
            expected: MOUNT_ID,
            found,
        }) if found == MOUNT_ID + 1
    ));
}

#[test]
fn filesystem_magic_must_be_stable_before_family_admission() {
    let drift = FixtureDevtmpfsDescriptorObservations {
        closing_magic: 0xEF53,
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(DevtmpfsDescriptorAuthenticationError::FilesystemMagicDrift {
            opening: FIXTURE_TMPFS_MAGIC,
            closing: 0xEF53,
        })
    ));
}

#[test]
fn wrong_observation_variant_is_a_protocol_violation() {
    let deadline = deadline();
    let calls = Cell::new(0usize);
    let mut observer = |phase| {
        calls.set(calls.get() + 1);
        Ok(
            if phase == DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId {
                FixtureDevtmpfsRawObservation::FilesystemMagic(FIXTURE_TMPFS_MAGIC)
            } else {
                raw_observation(stable_observations(), phase)
            },
        )
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_protocol(
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut observer,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::ObservationProtocolViolation {
            phase: DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId,
        })
    ));
    assert_eq!(calls.get(), 2);
}

#[test]
fn observation_limit_admits_six_and_rejects_five_before_sixth_hook() {
    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    let (_, usage) = validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        FixtureDevtmpfsDescriptorLimits {
            max_observations: 6,
            ..FixtureDevtmpfsDescriptorLimits::default()
        },
        deadline,
        &mut clock,
        &mut hook,
    )
    .unwrap();
    assert_eq!(usage.observations, 6);

    let hook_calls = Cell::new(0usize);
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits {
                max_observations: 5,
                ..FixtureDevtmpfsDescriptorLimits::default()
            },
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::ObservationLimitExceeded {
            limit: 5,
            phase: DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity,
        })
    ));
    assert_eq!(hook_calls.get(), 5);
}

#[test]
fn work_limit_admits_exact_consumption_and_rejects_one_less() {
    let (_, measured) = validate(stable_observations()).unwrap();
    let deadline = deadline();
    let exact = FixtureDevtmpfsDescriptorLimits {
        max_observations: measured.observations,
        max_work: measured.work,
    };
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    let (_, exact_usage) = validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        exact,
        deadline,
        &mut clock,
        &mut hook,
    )
    .unwrap();
    assert_eq!(exact_usage, measured);

    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits {
                max_work: measured.work - 1,
                ..exact
            },
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::WorkLimitExceeded { limit, .. })
            if limit == measured.work - 1
    ));
}

#[test]
fn deadline_equality_is_admitted_and_expired_time_fails_before_observation() {
    let deadline = deadline();
    let mut equal_clock = || deadline;
    let mut hook = |_| Ok(());
    validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut equal_clock,
        &mut hook,
    )
    .unwrap();

    let hook_calls = Cell::new(0usize);
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let mut late_clock = || deadline + Duration::from_nanos(1);
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut late_clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { .. })
    ));
    assert_eq!(hook_calls.get(), 0);
}

#[test]
fn deadline_expiry_between_observations_rejects_evidence() {
    let deadline = deadline();
    let expired = Cell::new(false);
    let phases = Cell::new(0usize);
    let mut hook = |phase| {
        phases.set(phases.get() + 1);
        if phase == DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic {
            expired.set(true);
        }
        Ok(())
    };
    let mut clock = || {
        if expired.get() {
            deadline + Duration::from_nanos(1)
        } else {
            deadline
        }
    };
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { .. })
    ));
    assert_eq!(phases.get(), 3);
}

#[test]
fn terminal_deadline_checkpoint_rejects_evidence() {
    let deadline = deadline();
    let calls = Cell::new(0usize);
    let mut counting_clock = || {
        calls.set(calls.get() + 1);
        deadline
    };
    let mut hook = |_| Ok(());
    validate_fixture_devtmpfs_descriptor_authentication(
        stable_observations(),
        device(),
        INODE,
        MOUNT_ID,
        stable_policy(),
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut counting_clock,
        &mut hook,
    )
    .unwrap();
    let terminal_call = calls.get();

    let calls = Cell::new(0usize);
    let hook_calls = Cell::new(0usize);
    let mut terminal_clock = || {
        let call = calls.get() + 1;
        calls.set(call);
        if call == terminal_call {
            deadline + Duration::from_nanos(1)
        } else {
            deadline
        }
    };
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut terminal_clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { .. })
    ));
    assert_eq!(calls.get(), terminal_call);
    assert_eq!(hook_calls.get(), 6);
}

#[test]
fn zero_or_above_ceiling_limits_fail_before_observations() {
    let deadline = deadline();
    for limits in [
        FixtureDevtmpfsDescriptorLimits {
            max_observations: 0,
            ..FixtureDevtmpfsDescriptorLimits::default()
        },
        FixtureDevtmpfsDescriptorLimits {
            max_work: 0,
            ..FixtureDevtmpfsDescriptorLimits::default()
        },
        FixtureDevtmpfsDescriptorLimits {
            max_observations: 7,
            ..FixtureDevtmpfsDescriptorLimits::default()
        },
        FixtureDevtmpfsDescriptorLimits {
            max_work: 65,
            ..FixtureDevtmpfsDescriptorLimits::default()
        },
    ] {
        let hook_calls = Cell::new(0usize);
        let mut hook = |_| {
            hook_calls.set(hook_calls.get() + 1);
            Ok(())
        };
        let mut clock = || deadline;
        assert!(matches!(
            validate_fixture_devtmpfs_descriptor_authentication(
                stable_observations(),
                device(),
                INODE,
                MOUNT_ID,
                stable_policy(),
                limits,
                deadline,
                &mut clock,
                &mut hook,
            ),
            Err(DevtmpfsDescriptorAuthenticationError::InvalidLimits)
        ));
        assert_eq!(hook_calls.get(), 0);
    }
}

#[test]
fn injected_observation_failure_propagates_without_fallback() {
    let deadline = deadline();
    let calls = Cell::new(0usize);
    let mut hook = |phase| {
        calls.set(calls.get() + 1);
        if phase == DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic {
            Err(io::Error::other("injected pure observation failure"))
        } else {
            Ok(())
        }
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_devtmpfs_descriptor_authentication(
            stable_observations(),
            device(),
            INODE,
            MOUNT_ID,
            stable_policy(),
            FixtureDevtmpfsDescriptorLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(DevtmpfsDescriptorAuthenticationError::ObservationFailed {
            phase: DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic,
            ..
        })
    ));
    assert_eq!(calls.get(), 3);
}
