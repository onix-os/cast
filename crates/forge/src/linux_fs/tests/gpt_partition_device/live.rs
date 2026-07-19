use std::{
    io::{self, Write as _},
    os::fd::AsFd as _,
    time::{Duration, Instant},
};

use super::super::super::{
    gpt_partition_device::{
        BlockDeviceObservation, BlockDeviceObserver, FixtureBlockDeviceSyscall, FixtureBlockDeviceSyscallResult,
        ObservedDeviceAccess, ObservedNodeKind, RetainedBlockDeviceObserver, fixture_block_ioctl_requests,
        observe_retained_block_device_fixture_with_clock_until, retained_read_only_block_image_fixture_until,
    },
    gpt_partition_role::GptPartitionRoleImage,
};

const MOUNT_ID: u64 = 63;
const DEVICE_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn exact_64_bit_linux_block_ioctl_numbers_are_sealed() {
    assert_eq!(fixture_block_ioctl_requests().unwrap(), (0x1268, 0x8008_1272));
}

#[test]
fn injected_one_shot_observation_returns_only_exact_closed_scalars() {
    let file = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut calls = Vec::new();
    let mut respond = |call| {
        calls.push(call);
        Ok(standard_result(call))
    };
    let mut clock = || Instant::now();

    let observed = observe_retained_block_device_fixture_with_clock_until(
        file.as_fd(),
        MOUNT_ID,
        deadline,
        &mut clock,
        &mut respond,
    )
    .unwrap();

    assert_eq!(
        calls,
        [
            FixtureBlockDeviceSyscall::Fstat,
            FixtureBlockDeviceSyscall::FcntlGetFl,
            FixtureBlockDeviceSyscall::BlockLogicalSize,
            FixtureBlockDeviceSyscall::BlockByteLength,
        ]
    );
    assert_eq!(
        observed,
        BlockDeviceObservation::new(
            ObservedNodeKind::BlockDevice,
            ObservedDeviceAccess::ReadOnly,
            41,
            52,
            MOUNT_ID,
            8,
            0,
            512,
            DEVICE_BYTES,
        )
    );
}

#[test]
fn write_capable_and_path_only_descriptors_stop_before_block_queries() {
    for flags in [nix::libc::O_WRONLY, nix::libc::O_RDWR, nix::libc::O_PATH] {
        let file = tempfile::tempfile().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut calls = Vec::new();
        let mut respond = |call| {
            calls.push(call);
            if call == FixtureBlockDeviceSyscall::FcntlGetFl {
                Ok(FixtureBlockDeviceSyscallResult::Flags(flags))
            } else {
                Ok(standard_result(call))
            }
        };
        let mut clock = || Instant::now();

        let error = observe_retained_block_device_fixture_with_clock_until(
            file.as_fd(),
            MOUNT_ID,
            deadline,
            &mut clock,
            &mut respond,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            calls,
            [FixtureBlockDeviceSyscall::Fstat, FixtureBlockDeviceSyscall::FcntlGetFl,]
        );
    }
}

#[test]
fn ordinary_file_is_rejected_without_any_storage_discovery_or_block_query() {
    let file = tempfile::tempfile().unwrap();
    let mut observer = RetainedBlockDeviceObserver::new(file.as_fd(), MOUNT_ID).unwrap();
    let error = observer
        .observe_until(Instant::now() + Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn every_injected_syscall_error_propagates_after_exactly_one_attempt() {
    let ordered = [
        FixtureBlockDeviceSyscall::Fstat,
        FixtureBlockDeviceSyscall::FcntlGetFl,
        FixtureBlockDeviceSyscall::BlockLogicalSize,
        FixtureBlockDeviceSyscall::BlockByteLength,
    ];
    for (failed_index, failed) in ordered.into_iter().enumerate() {
        let file = tempfile::tempfile().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut calls = Vec::new();
        let mut respond = |call| {
            calls.push(call);
            if call == failed {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(standard_result(call))
            }
        };
        let mut clock = || Instant::now();

        let error = observe_retained_block_device_fixture_with_clock_until(
            file.as_fd(),
            MOUNT_ID,
            deadline,
            &mut clock,
            &mut respond,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(calls, ordered[..=failed_index]);
        assert_eq!(calls.iter().filter(|call| **call == failed).count(), 1);
    }
}

#[test]
fn deadlines_are_checked_before_work_and_after_each_kernel_call() {
    let file = tempfile::tempfile().unwrap();
    let live = Instant::now();
    let expired = live + Duration::from_secs(2);
    let deadline = live + Duration::from_secs(1);

    let mut no_calls = Vec::new();
    let mut respond = |call| {
        no_calls.push(call);
        Ok(standard_result(call))
    };
    let mut expired_clock = || expired;
    let error = observe_retained_block_device_fixture_with_clock_until(
        file.as_fd(),
        MOUNT_ID,
        deadline,
        &mut expired_clock,
        &mut respond,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(no_calls.is_empty());

    let mut calls = Vec::new();
    let mut respond = |call| {
        calls.push(call);
        Ok(standard_result(call))
    };
    let mut ticks = 0;
    let mut post_call_expiry = || {
        ticks += 1;
        if ticks >= 3 { expired } else { live }
    };
    let error = observe_retained_block_device_fixture_with_clock_until(
        file.as_fd(),
        MOUNT_ID,
        deadline,
        &mut post_call_expiry,
        &mut respond,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls, [FixtureBlockDeviceSyscall::Fstat]);
}

#[test]
fn positional_reads_are_capped_and_never_cross_authenticated_length() {
    let mut file = tempfile::tempfile().unwrap();
    let bytes = vec![0x5a; 70 * 1024];
    file.write_all(&bytes).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);

    let mut capped =
        retained_read_only_block_image_fixture_until(file.as_fd(), bytes.len().try_into().unwrap(), deadline).unwrap();
    let mut large_output = vec![0_u8; bytes.len()];
    assert_eq!(capped.read(0, &mut large_output).unwrap(), 64 * 1024);

    let mut bounded = retained_read_only_block_image_fixture_until(file.as_fd(), 4, deadline).unwrap();
    assert_eq!(bounded.length(), 4);
    let mut output = [0_u8; 8];
    assert_eq!(bounded.read(2, &mut output).unwrap(), 2);
    assert_eq!(&output[..2], &[0x5a, 0x5a]);
    output.fill(0xa5);
    assert_eq!(bounded.read(4, &mut output).unwrap(), 0);
    assert_eq!(output, [0xa5; 8]);
}

#[test]
fn image_rejects_zero_unaddressable_and_expired_authority() {
    let file = tempfile::tempfile().unwrap();
    let live = Instant::now() + Duration::from_secs(5);
    for length in [0, i64::MAX as u64 + 1] {
        assert_eq!(
            retained_read_only_block_image_fixture_until(file.as_fd(), length, live)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        retained_read_only_block_image_fixture_until(file.as_fd(), 512, Instant::now() - Duration::from_secs(1),)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
}

fn standard_result(call: FixtureBlockDeviceSyscall) -> FixtureBlockDeviceSyscallResult {
    match call {
        FixtureBlockDeviceSyscall::Fstat => FixtureBlockDeviceSyscallResult::Stat {
            containing_device: 41,
            inode: 52,
            mode: nix::libc::S_IFBLK | 0o600,
            raw_device: nix::libc::makedev(8, 0),
        },
        FixtureBlockDeviceSyscall::FcntlGetFl => FixtureBlockDeviceSyscallResult::Flags(nix::libc::O_RDONLY),
        FixtureBlockDeviceSyscall::BlockLogicalSize => FixtureBlockDeviceSyscallResult::LogicalBlockSize(512),
        FixtureBlockDeviceSyscall::BlockByteLength => FixtureBlockDeviceSyscallResult::ByteLength(DEVICE_BYTES),
    }
}
