use std::{
    fs, io,
    time::{Duration, Instant},
};

use super::super::super::{
    sysfs_block::{
        SYSFS_DEV_ATTRIBUTE_MAX_BYTES, SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES, SYSFS_UEVENT_MAX_BYTES, SysfsDeviceNumber,
    },
    sysfs_identity::{FixtureCheckpoint, FixtureSysfsIdentityLimits, FixtureSysfsTree, PreparedSysfsPartitionIdentity},
};
use super::support::{
    DISK_SEQUENCE, FixtureEntry, PARTITION_MAJOR, PARTITION_MINOR, PARTITION_NAME, PARTITION_NUMBER, PARTITION_UUID,
    SyntheticSysfs,
};

#[derive(Clone, Copy)]
enum LimitField {
    Work,
    Ancestors,
    Descriptors,
}

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn with_limit(field: LimitField, value: usize) -> FixtureSysfsIdentityLimits {
    let mut limits = FixtureSysfsIdentityLimits::default();
    match field {
        LimitField::Work => limits.max_work = value,
        LimitField::Ancestors => limits.max_ancestors = value,
        LimitField::Descriptors => limits.max_descriptors = value,
    }
    limits
}

fn prepare_attempt(tree: &FixtureSysfsTree, limits: FixtureSysfsIdentityLimits) -> io::Result<()> {
    let mut hook = |_| Ok(());
    tree.prepare_with(device(), limits, deadline(), &mut hook).map(drop)
}

fn revalidate_attempt(prepared: &PreparedSysfsPartitionIdentity, limits: FixtureSysfsIdentityLimits) -> io::Result<()> {
    let mut hook = |_| Ok(());
    prepared.revalidate_with(limits, deadline(), &mut hook).map(drop)
}

fn minimum_accepting(upper: usize, mut attempt: impl FnMut(usize) -> io::Result<()>) -> usize {
    assert!(upper > 1);
    attempt(upper).unwrap();
    let mut rejected = 0usize;
    let mut accepted = upper;
    while accepted - rejected > 1 {
        let candidate = rejected + (accepted - rejected) / 2;
        match attempt(candidate) {
            Ok(()) => accepted = candidate,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                rejected = candidate;
            }
        }
    }
    accepted
}

fn partition_event(partition_number: u32) -> Vec<u8> {
    format!(
        "MAJOR={PARTITION_MAJOR}\nMINOR={PARTITION_MINOR}\nDEVNAME={PARTITION_NAME}\nDEVTYPE=partition\nPARTN={partition_number}\nPARTUUID={PARTITION_UUID}\nDISKSEQ={DISK_SEQUENCE}\n"
    )
    .into_bytes()
}

fn padded_partition_event(length: usize) -> Vec<u8> {
    let mut bytes = partition_event(PARTITION_NUMBER);
    assert!(length.checked_sub(bytes.len()).unwrap() >= 16);
    let mut index = 0usize;
    while bytes.len() < length {
        let key = format!("OPAQUE_{index}=");
        let remaining = length - bytes.len();
        let minimum = key.len() + 1;
        let line_total = if remaining <= 4_097 {
            remaining
        } else {
            let next_minimum = format!("OPAQUE_{}=", index + 1).len() + 1;
            if remaining - 4_097 < next_minimum {
                remaining - next_minimum
            } else {
                4_097
            }
        };
        assert!((minimum..=4_097).contains(&line_total));
        bytes.extend_from_slice(key.as_bytes());
        bytes.resize(bytes.len() + line_total - minimum, b'x');
        bytes.push(b'\n');
        index += 1;
    }
    assert_eq!(bytes.len(), length);
    bytes
}

#[test]
fn zero_fixture_limits_and_expired_deadlines_fail_before_hooks_or_syscalls() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let tree = admitted(&fixture).unwrap();

    for limits in [
        FixtureSysfsIdentityLimits {
            max_work: 0,
            ..FixtureSysfsIdentityLimits::default()
        },
        FixtureSysfsIdentityLimits {
            max_ancestors: 0,
            ..FixtureSysfsIdentityLimits::default()
        },
        FixtureSysfsIdentityLimits {
            max_descriptors: 0,
            ..FixtureSysfsIdentityLimits::default()
        },
    ] {
        let mut calls = 0usize;
        let error = tree
            .prepare_with(device(), limits, deadline(), &mut |_| {
                calls += 1;
                Ok(())
            })
            .err()
            .expect("zero fixture limit unexpectedly succeeded");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(calls, 0);
    }

    let mut calls = 0usize;
    let error = tree
        .prepare_with(
            device(),
            FixtureSysfsIdentityLimits::default(),
            Instant::now() - Duration::from_millis(1),
            &mut |_| {
                calls += 1;
                Ok(())
            },
        )
        .err()
        .expect("expired fixture deadline unexpectedly succeeded");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls, 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn injected_checkpoint_failure_is_propagated_without_external_effects() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let tree = admitted(&fixture).unwrap();
    let mut reached = false;
    let error = tree
        .prepare_with(
            device(),
            FixtureSysfsIdentityLimits::default(),
            deadline(),
            &mut |checkpoint| {
                if checkpoint == FixtureCheckpoint::TargetPinned {
                    reached = true;
                    return Err(io::Error::other("injected finite-checkpoint failure"));
                }
                Ok(())
            },
        )
        .err()
        .expect("injected checkpoint failure unexpectedly succeeded");
    assert!(reached);
    assert_eq!(error.kind(), io::ErrorKind::Other);
    fixture.assert_outside_unchanged();
}

#[test]
fn preparation_work_ancestor_and_descriptor_limits_have_exact_boundaries() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let tree = admitted(&fixture).unwrap();
    let defaults = FixtureSysfsIdentityLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Ancestors, defaults.max_ancestors),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| prepare_attempt(&tree, with_limit(field, value)));
        assert!(exact > 1);
        prepare_attempt(&tree, with_limit(field, exact)).unwrap();
        assert_eq!(
            prepare_attempt(&tree, with_limit(field, exact - 1)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn revalidation_budget_is_global_across_both_recaptures_and_terminal_checks() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();
    let defaults = FixtureSysfsIdentityLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| revalidate_attempt(&prepared, with_limit(field, value)));
        assert!(exact > 1);
        revalidate_attempt(&prepared, with_limit(field, exact)).unwrap();
        assert_eq!(
            revalidate_attempt(&prepared, with_limit(field, exact - 1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn attribute_read_ceilings_accept_exact_bytes_and_reject_one_more() {
    let exact_dev = SyntheticSysfs::stable().unwrap();
    assert_eq!(
        fs::read(exact_dev.entry(FixtureEntry::PartitionDevice)).unwrap().len(),
        SYSFS_DEV_ATTRIBUTE_MAX_BYTES
    );
    admitted(&exact_dev).unwrap().prepare(device()).unwrap();

    let oversized_dev = SyntheticSysfs::stable().unwrap();
    oversized_dev
        .replace_regular(
            FixtureEntry::PartitionDevice,
            format!("{PARTITION_MAJOR}:{PARTITION_MINOR}\nX").as_bytes(),
        )
        .unwrap();
    assert_eq!(
        fs::read(oversized_dev.entry(FixtureEntry::PartitionDevice))
            .unwrap()
            .len(),
        SYSFS_DEV_ATTRIBUTE_MAX_BYTES + 1
    );
    assert_eq!(
        admitted(&oversized_dev)
            .unwrap()
            .prepare(device())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let exact_partition = SyntheticSysfs::stable().unwrap();
    exact_partition
        .replace_regular(FixtureEntry::PartitionNumber, format!("{}\n", u32::MAX).as_bytes())
        .unwrap();
    exact_partition
        .replace_regular(FixtureEntry::PartitionEvent, &partition_event(u32::MAX))
        .unwrap();
    assert_eq!(
        fs::read(exact_partition.entry(FixtureEntry::PartitionNumber))
            .unwrap()
            .len(),
        SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES
    );
    admitted(&exact_partition).unwrap().prepare(device()).unwrap();

    let oversized_partition = SyntheticSysfs::stable().unwrap();
    oversized_partition
        .replace_regular(FixtureEntry::PartitionNumber, b"4294967295\nX")
        .unwrap();
    assert_eq!(
        admitted(&oversized_partition)
            .unwrap()
            .prepare(device())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn uevent_and_link_bounds_are_enforced_before_resolution_or_parsing() {
    let exact = SyntheticSysfs::stable().unwrap();
    exact
        .replace_regular(
            FixtureEntry::PartitionEvent,
            &padded_partition_event(SYSFS_UEVENT_MAX_BYTES),
        )
        .unwrap();
    admitted(&exact).unwrap().prepare(device()).unwrap();

    let oversized = SyntheticSysfs::stable().unwrap();
    oversized
        .replace_regular(
            FixtureEntry::PartitionEvent,
            &padded_partition_event(SYSFS_UEVENT_MAX_BYTES + 1),
        )
        .unwrap();
    assert_eq!(
        admitted(&oversized).unwrap().prepare(device()).err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );

    let too_many_components = SyntheticSysfs::stable().unwrap();
    let mut target = b"../../devices".to_vec();
    for _ in 0..126 {
        target.extend_from_slice(b"/a");
    }
    too_many_components
        .replace_symlink(FixtureEntry::Lookup, &target)
        .unwrap();
    assert_eq!(
        admitted(&too_many_components)
            .unwrap()
            .prepare(device())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let oversized_subsystem_component = SyntheticSysfs::stable().unwrap();
    let mut subsystem = b"../../../class/".to_vec();
    subsystem.resize(subsystem.len() + 256, b'b');
    oversized_subsystem_component
        .replace_symlink(FixtureEntry::PartitionSubsystem, &subsystem)
        .unwrap();
    assert_eq!(
        admitted(&oversized_subsystem_component)
            .unwrap()
            .prepare(device())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
