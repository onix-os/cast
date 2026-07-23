use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::descriptor_boot_filesystem::{
    BootFilesystemAuthenticationError, BootFilesystemMagicFamily, BootFilesystemObservationPhase,
    FIXTURE_MSDOS_SUPER_MAGIC, FixtureBootFilesystemIdentity, FixtureBootFilesystemLimits,
    FixtureBootFilesystemObservations, FixtureBootFilesystemUsage, ValidatedBootFilesystemDescriptorEvidence,
    validate_fixture_boot_filesystem_authentication,
};

const DEVICE: u64 = 73;
const INODE: u64 = 101;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn stable_identity() -> FixtureBootFilesystemIdentity {
    FixtureBootFilesystemIdentity {
        device: DEVICE,
        inode: INODE,
        kind: nix::libc::S_IFDIR,
    }
}

fn stable_observations() -> FixtureBootFilesystemObservations {
    FixtureBootFilesystemObservations {
        opening_identity: stable_identity(),
        opening_magic: FIXTURE_MSDOS_SUPER_MAGIC,
        closing_magic: FIXTURE_MSDOS_SUPER_MAGIC,
        closing_identity: stable_identity(),
    }
}

fn validate(
    observations: FixtureBootFilesystemObservations,
) -> Result<(ValidatedBootFilesystemDescriptorEvidence, FixtureBootFilesystemUsage), BootFilesystemAuthenticationError>
{
    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    validate_fixture_boot_filesystem_authentication(
        observations,
        DEVICE,
        INODE,
        FixtureBootFilesystemLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
}

#[test]
fn stable_msdos_family_directory_retains_only_expected_scalar_evidence() {
    let (evidence, usage) = validate(stable_observations()).unwrap();
    assert_eq!(evidence.destination_device(), DEVICE);
    assert_eq!(evidence.destination_inode(), INODE);
    assert_eq!(evidence.magic_family(), BootFilesystemMagicFamily::LinuxMsdos);
    assert_eq!(usage.observations, 4);
    assert!(usage.work > usage.observations);
}

#[test]
fn stable_wrong_magic_is_rejected_without_claiming_exact_vfat() {
    let observations = FixtureBootFilesystemObservations {
        opening_magic: 0xEF53,
        closing_magic: 0xEF53,
        ..stable_observations()
    };
    assert!(matches!(
        validate(observations),
        Err(BootFilesystemAuthenticationError::UnsupportedFilesystemMagic {
            expected: FIXTURE_MSDOS_SUPER_MAGIC,
            found: 0xEF53,
        })
    ));
}

#[test]
fn filesystem_magic_drift_is_rejected_before_family_admission() {
    let observations = FixtureBootFilesystemObservations {
        closing_magic: 0xEF53,
        ..stable_observations()
    };
    assert!(matches!(
        validate(observations),
        Err(BootFilesystemAuthenticationError::FilesystemMagicDrift {
            opening: FIXTURE_MSDOS_SUPER_MAGIC,
            closing: 0xEF53,
        })
    ));
}

#[test]
fn expected_identity_is_exact_nonzero_and_checked_before_observations() {
    let deadline = deadline();
    let hook_calls = Cell::new(0usize);
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            0,
            INODE,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::InvalidExpectedIdentity {
            device: 0,
            inode: INODE,
        })
    ));
    assert_eq!(hook_calls.get(), 0);

    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            0,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::InvalidExpectedIdentity {
            device: DEVICE,
            inode: 0,
        })
    ));
    assert_eq!(hook_calls.get(), 0);

    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE + 1,
            INODE,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity {
            expected_device,
            expected_inode: INODE,
            found_device: DEVICE,
            found_inode: INODE,
        }) if expected_device == DEVICE + 1
    ));

    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE + 1,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity {
            expected_device: DEVICE,
            expected_inode,
            found_device: DEVICE,
            found_inode: INODE,
        }) if expected_inode == INODE + 1
    ));
}

#[test]
fn observed_identity_must_be_nonzero_and_stable() {
    let zero = FixtureBootFilesystemObservations {
        opening_identity: FixtureBootFilesystemIdentity {
            device: 0,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(zero),
        Err(BootFilesystemAuthenticationError::InvalidObservedIdentity {
            phase: BootFilesystemObservationPhase::OpeningDirectoryIdentity,
            device: 0,
            inode: INODE,
        })
    ));

    let closing_zero = FixtureBootFilesystemObservations {
        closing_identity: FixtureBootFilesystemIdentity {
            inode: 0,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(closing_zero),
        Err(BootFilesystemAuthenticationError::InvalidObservedIdentity {
            phase: BootFilesystemObservationPhase::ClosingDirectoryIdentity,
            device: DEVICE,
            inode: 0,
        })
    ));

    let drift = FixtureBootFilesystemObservations {
        closing_identity: FixtureBootFilesystemIdentity {
            inode: INODE + 1,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(BootFilesystemAuthenticationError::DirectoryIdentityDrift {
            opening_device: DEVICE,
            opening_inode: INODE,
            closing_device: DEVICE,
            closing_inode,
        }) if closing_inode == INODE + 1
    ));

    let device_drift = FixtureBootFilesystemObservations {
        closing_identity: FixtureBootFilesystemIdentity {
            device: DEVICE + 1,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(device_drift),
        Err(BootFilesystemAuthenticationError::DirectoryIdentityDrift {
            opening_device: DEVICE,
            opening_inode: INODE,
            closing_device,
            closing_inode: INODE,
        }) if closing_device == DEVICE + 1
    ));
}

#[test]
fn directory_kind_must_be_stable_and_exact() {
    let wrong = FixtureBootFilesystemObservations {
        opening_identity: FixtureBootFilesystemIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        closing_identity: FixtureBootFilesystemIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(wrong),
        Err(BootFilesystemAuthenticationError::UnsupportedDirectoryKind {
            expected: nix::libc::S_IFDIR,
            found: nix::libc::S_IFREG,
        })
    ));

    let drift = FixtureBootFilesystemObservations {
        closing_identity: FixtureBootFilesystemIdentity {
            kind: nix::libc::S_IFREG,
            ..stable_identity()
        },
        ..stable_observations()
    };
    assert!(matches!(
        validate(drift),
        Err(BootFilesystemAuthenticationError::DirectoryKindDrift {
            opening: nix::libc::S_IFDIR,
            closing: nix::libc::S_IFREG,
        })
    ));
}

#[test]
fn observation_limit_admits_four_and_rejects_three_before_the_fourth_hook() {
    let deadline = deadline();
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    let (_, usage) = validate_fixture_boot_filesystem_authentication(
        stable_observations(),
        DEVICE,
        INODE,
        FixtureBootFilesystemLimits {
            max_observations: 4,
            ..FixtureBootFilesystemLimits::default()
        },
        deadline,
        &mut clock,
        &mut hook,
    )
    .unwrap();
    assert_eq!(usage.observations, 4);

    let hook_calls = Cell::new(0usize);
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE,
            FixtureBootFilesystemLimits {
                max_observations: 3,
                ..FixtureBootFilesystemLimits::default()
            },
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::ObservationLimitExceeded {
            limit: 3,
            phase: BootFilesystemObservationPhase::ClosingDirectoryIdentity,
        })
    ));
    assert_eq!(hook_calls.get(), 3);
}

#[test]
fn work_limit_admits_exact_consumption_and_rejects_one_less() {
    let (_, measured) = validate(stable_observations()).unwrap();
    let deadline = deadline();
    let exact = FixtureBootFilesystemLimits {
        max_observations: measured.observations,
        max_work: measured.work,
    };
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    let (_, exact_usage) = validate_fixture_boot_filesystem_authentication(
        stable_observations(),
        DEVICE,
        INODE,
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
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE,
            FixtureBootFilesystemLimits {
                max_work: measured.work - 1,
                ..exact
            },
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::WorkLimitExceeded { limit, .. }) if limit == measured.work - 1
    ));
}

#[test]
fn deadline_equality_is_admitted_and_later_time_fails_before_observation() {
    let deadline = deadline();
    let mut equal_clock = || deadline;
    let mut hook = |_| Ok(());
    validate_fixture_boot_filesystem_authentication(
        stable_observations(),
        DEVICE,
        INODE,
        FixtureBootFilesystemLimits::default(),
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
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut late_clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::DeadlineExceeded { .. })
    ));
    assert_eq!(hook_calls.get(), 0);
}

#[test]
fn deadline_expiring_at_terminal_checkpoint_rejects_evidence() {
    let deadline = deadline();
    let calls = Cell::new(0usize);
    let mut counting_clock = || {
        calls.set(calls.get() + 1);
        deadline
    };
    let mut hook = |_| Ok(());
    validate_fixture_boot_filesystem_authentication(
        stable_observations(),
        DEVICE,
        INODE,
        FixtureBootFilesystemLimits::default(),
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
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut terminal_clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::DeadlineExceeded { .. })
    ));
    assert_eq!(calls.get(), terminal_call);
    assert_eq!(hook_calls.get(), 4);
}

#[test]
fn zero_limits_fail_before_fixture_observations() {
    let deadline = deadline();
    for limits in [
        FixtureBootFilesystemLimits {
            max_observations: 0,
            ..FixtureBootFilesystemLimits::default()
        },
        FixtureBootFilesystemLimits {
            max_work: 0,
            ..FixtureBootFilesystemLimits::default()
        },
    ] {
        let hook_calls = Cell::new(0usize);
        let mut hook = |_| {
            hook_calls.set(hook_calls.get() + 1);
            Ok(())
        };
        let mut clock = || deadline;
        assert!(matches!(
            validate_fixture_boot_filesystem_authentication(
                stable_observations(),
                DEVICE,
                INODE,
                limits,
                deadline,
                &mut clock,
                &mut hook,
            ),
            Err(BootFilesystemAuthenticationError::InvalidLimits)
        ));
        assert_eq!(hook_calls.get(), 0);
    }
}

#[test]
fn injected_fixture_observation_failure_propagates_without_fallback() {
    let deadline = deadline();
    let calls = Cell::new(0usize);
    let mut hook = |phase| {
        calls.set(calls.get() + 1);
        if phase == BootFilesystemObservationPhase::OpeningFilesystemMagic {
            Err(io::Error::other("injected pure observation failure"))
        } else {
            Ok(())
        }
    };
    let mut clock = || deadline;
    assert!(matches!(
        validate_fixture_boot_filesystem_authentication(
            stable_observations(),
            DEVICE,
            INODE,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        ),
        Err(BootFilesystemAuthenticationError::ObservationFailed {
            phase: BootFilesystemObservationPhase::OpeningFilesystemMagic,
            ..
        })
    ));
    assert_eq!(calls.get(), 2);
}
