use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::super::{
    sysfs_block::SysfsDeviceNumber,
    sysfs_identity::{FixtureSysfsIdentityLimits, FixtureSysfsTree, PreparedSysfsPartitionIdentity},
};
use super::support::{PARTITION_MAJOR, PARTITION_MINOR, SyntheticSysfs};

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn measured_preparation_calls(tree: &FixtureSysfsTree, deadline: Instant) -> usize {
    let calls = Cell::new(0usize);
    let mut clock = || {
        calls.set(calls.get() + 1);
        deadline
    };
    let mut hook = |_| Ok(());
    tree.prepare_with_clock(
        device(),
        FixtureSysfsIdentityLimits::default(),
        deadline,
        &mut hook,
        &mut clock,
    )
    .unwrap();
    calls.get()
}

fn measured_revalidation_calls(prepared: &PreparedSysfsPartitionIdentity, deadline: Instant) -> usize {
    let calls = Cell::new(0usize);
    let mut clock = || {
        calls.set(calls.get() + 1);
        deadline
    };
    let mut hook = |_| Ok(());
    let view = prepared
        .revalidate_with_clock(FixtureSysfsIdentityLimits::default(), deadline, &mut hook, &mut clock)
        .unwrap();
    drop(view);
    calls.get()
}

#[test]
fn caller_deadline_equality_is_admitted_and_one_nanosecond_late_fails_before_fixture_work() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let tree = admitted(&fixture).unwrap();
    let deadline = future_deadline();

    assert!(measured_preparation_calls(&tree, deadline) > 1);

    let clock_calls = Cell::new(0usize);
    let hook_calls = Cell::new(0usize);
    let mut late_clock = || {
        clock_calls.set(clock_calls.get() + 1);
        deadline + Duration::from_nanos(1)
    };
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let error = tree
        .prepare_with_clock(
            device(),
            FixtureSysfsIdentityLimits::default(),
            deadline,
            &mut hook,
            &mut late_clock,
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(clock_calls.get(), 1);
    assert_eq!(hook_calls.get(), 0);

    let prepared = tree.prepare(device()).unwrap();
    let clock_calls = Cell::new(0usize);
    let hook_calls = Cell::new(0usize);
    let mut late_clock = || {
        clock_calls.set(clock_calls.get() + 1);
        deadline + Duration::from_nanos(1)
    };
    let mut hook = |_| {
        hook_calls.set(hook_calls.get() + 1);
        Ok(())
    };
    let error = prepared
        .revalidate_with_clock(
            FixtureSysfsIdentityLimits::default(),
            deadline,
            &mut hook,
            &mut late_clock,
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(clock_calls.get(), 1);
    assert_eq!(hook_calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn terminal_deadline_expiry_rejects_preparation_and_revalidation() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let tree = admitted(&fixture).unwrap();
    let deadline = future_deadline();

    let preparation_terminal_check = measured_preparation_calls(&tree, deadline);
    let calls = Cell::new(0usize);
    let mut clock = || {
        let call = calls.get() + 1;
        calls.set(call);
        if call == preparation_terminal_check {
            deadline + Duration::from_nanos(1)
        } else {
            deadline
        }
    };
    let mut hook = |_| Ok(());
    let error = tree
        .prepare_with_clock(
            device(),
            FixtureSysfsIdentityLimits::default(),
            deadline,
            &mut hook,
            &mut clock,
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls.get(), preparation_terminal_check);

    let prepared = tree.prepare(device()).unwrap();
    let revalidation_terminal_check = measured_revalidation_calls(&prepared, deadline);
    let calls = Cell::new(0usize);
    let mut clock = || {
        let call = calls.get() + 1;
        calls.set(call);
        if call == revalidation_terminal_check {
            deadline + Duration::from_nanos(1)
        } else {
            deadline
        }
    };
    let mut hook = |_| Ok(());
    let error = prepared
        .revalidate_with_clock(FixtureSysfsIdentityLimits::default(), deadline, &mut hook, &mut clock)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls.get(), revalidation_terminal_check);
    fixture.assert_outside_unchanged();
}
