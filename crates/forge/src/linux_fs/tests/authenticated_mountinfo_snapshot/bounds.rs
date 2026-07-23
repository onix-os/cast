use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::super::mount_namespace::{FixtureMountInfoSnapshotLimits, FixtureMountNamespaceCheckpoint};
use super::support::{RECORD, SyntheticMountInfoContext, assert_error_kind, deadline};

#[test]
fn zero_limits_and_expired_deadline_fail_before_fixture_hooks() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let mut hooks = 0usize;
    assert_error_kind(
        prepared.read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits {
                max_work: 0,
                max_descriptors: 1,
            },
            deadline(),
            &mut |_| {
                hooks += 1;
                Ok(())
            },
        ),
        io::ErrorKind::InvalidInput,
    );
    assert_error_kind(
        prepared.read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits::default(),
            Instant::now() - Duration::from_millis(1),
            &mut |_| {
                hooks += 1;
                Ok(())
            },
        ),
        io::ErrorKind::TimedOut,
    );
    assert_eq!(hooks, 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn exact_work_and_descriptor_budgets_admit_n_and_reject_n_minus_one() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let defaults = FixtureMountInfoSnapshotLimits::default();
    let (_snapshot, usage) = prepared
        .measure_fixture_mountinfo_bytes_with(RECORD, defaults, deadline())
        .unwrap();
    assert!(usage.work > 1);
    assert!(usage.descriptors > 1);

    prepared
        .read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits {
                max_work: usage.work,
                max_descriptors: usage.descriptors,
            },
            deadline(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert_error_kind(
        prepared.read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits {
                max_work: usage.work - 1,
                max_descriptors: usage.descriptors,
            },
            deadline(),
            &mut |_| Ok(()),
        ),
        io::ErrorKind::InvalidData,
    );
    assert_error_kind(
        prepared.read_fixture_mountinfo_bytes_with(
            RECORD,
            FixtureMountInfoSnapshotLimits {
                max_work: usage.work,
                max_descriptors: usage.descriptors - 1,
            },
            deadline(),
            &mut |_| Ok(()),
        ),
        io::ErrorKind::InvalidData,
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn injected_inner_reader_failure_propagates_without_fallback() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let mut reached = false;
    let result = prepared.read_fixture_mountinfo_bytes_with(
        RECORD,
        FixtureMountInfoSnapshotLimits::default(),
        deadline(),
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::MountInfoSnapshotBeforeRead {
                reached = true;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected reader denial",
                ));
            }
            Ok(())
        },
    );
    assert_error_kind(result, io::ErrorKind::PermissionDenied);
    assert!(reached);
    fixture.assert_outside_unchanged();
}

#[test]
fn deadline_expiring_at_terminal_checkpoint_rejects_snapshot() {
    let fixture = SyntheticMountInfoContext::stable().unwrap();
    let prepared = fixture.prepared().unwrap();
    let terminal_deadline = Instant::now() + Duration::from_secs(10);
    let terminal = Cell::new(false);
    let terminal_clock_calls = Cell::new(0usize);
    let mut hook = |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::MountInfoSnapshotComplete {
            terminal.set(true);
        }
        Ok(())
    };
    let mut clock = || {
        if terminal.get() {
            let call = terminal_clock_calls.get();
            terminal_clock_calls.set(call + 1);
            if call == 0 {
                terminal_deadline
            } else {
                terminal_deadline + Duration::from_nanos(1)
            }
        } else {
            terminal_deadline
        }
    };
    let result = prepared.read_fixture_mountinfo_bytes_with_clock(
        RECORD,
        FixtureMountInfoSnapshotLimits::default(),
        terminal_deadline,
        &mut hook,
        &mut clock,
    );
    assert_error_kind(result, io::ErrorKind::TimedOut);
    assert!(terminal.get());
    assert_eq!(terminal_clock_calls.get(), 2);
    fixture.assert_outside_unchanged();
}
