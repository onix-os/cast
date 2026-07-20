use std::{
    cell::Cell,
    fs::File,
    io,
    time::{Duration, Instant},
};

use super::super::super::{
    descriptor_mount_id_until,
    gpt_partition_device::{
        BlockDeviceObservation, FixtureRetainedParentProtocolCall, FixtureRetainedParentProtocolResult,
        ObservedDeviceAccess, ObservedNodeKind, close_retained_gpt_parent_fixture_with_clock_until,
        rebind_retained_gpt_parent_fixture_with_clock_until, retain_gpt_parent_block_device_fixture_with_clock_until,
        retain_gpt_parent_block_device_linux_fixture_until,
    },
};

const ROOT_MOUNT_ID: u64 = 63;
const PARENT_MAJOR: u32 = 8;
const PARENT_MINOR: u32 = 0;
const PARTITION_START: u64 = 2_048;
const PARTITION_SIZE: u64 = 4_096;
const DEVICE_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn exact_relative_open_policy_and_protocol_order_are_sealed_across_all_rebinds() {
    let root = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut calls = Vec::new();
    let mut respond = |call: FixtureRetainedParentProtocolCall| {
        calls.push(call.clone());
        standard_result(call, standard_observation())
    };
    let mut clock = || Instant::now();

    let retained = retain_fixture(&root, b"mapper/parent-disk", deadline, &mut clock, &mut respond).unwrap();
    let observer = retained.same_descriptor_observer().unwrap();
    drop(observer);
    rebind_retained_gpt_parent_fixture_with_clock_until(&retained, deadline, &mut clock, &mut respond).unwrap();
    close_retained_gpt_parent_fixture_with_clock_until(retained, deadline, &mut clock, &mut respond).unwrap();

    assert_eq!(calls.len(), 9);
    for pass in 0..3 {
        assert_eq!(
            calls[pass * 3],
            FixtureRetainedParentProtocolCall::Open {
                name: b"mapper/parent-disk".to_vec(),
                flags: nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                mode: 0,
                resolve: (nix::libc::RESOLVE_BENEATH
                    | nix::libc::RESOLVE_NO_MAGICLINKS
                    | nix::libc::RESOLVE_NO_SYMLINKS
                    | nix::libc::RESOLVE_NO_XDEV) as u64,
                deadline,
            }
        );
        assert_eq!(
            calls[pass * 3 + 1],
            FixtureRetainedParentProtocolCall::DescriptorMountId { deadline }
        );
        assert_eq!(
            calls[pass * 3 + 2],
            FixtureRetainedParentProtocolCall::Observe {
                authenticated_mount_id: ROOT_MOUNT_ID,
                deadline,
            }
        );
    }
}

#[test]
fn child_mount_id_must_equal_the_authenticated_root_before_observation() {
    let root = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut calls = Vec::new();
    let mut respond = |call: FixtureRetainedParentProtocolCall| {
        calls.push(call.clone());
        match call {
            FixtureRetainedParentProtocolCall::Open { .. } => Ok(FixtureRetainedParentProtocolResult::Opened(
                tempfile::tempfile().unwrap(),
            )),
            FixtureRetainedParentProtocolCall::DescriptorMountId { .. } => Ok(
                FixtureRetainedParentProtocolResult::DescriptorMountId(ROOT_MOUNT_ID + 1),
            ),
            FixtureRetainedParentProtocolCall::Observe { .. } => {
                panic!("mount drift must stop before block observation")
            }
        }
    };
    let mut clock = || Instant::now();

    let error = retain_fixture(&root, b"parent", deadline, &mut clock, &mut respond)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(calls.len(), 2);
}

#[test]
fn block_observation_mount_id_must_equal_the_captured_descriptor_mount_id() {
    let root = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut calls = Vec::new();
    let changed = observation(41, 52, ROOT_MOUNT_ID + 1, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES);
    let mut respond = |call: FixtureRetainedParentProtocolCall| {
        calls.push(call.clone());
        standard_result(call, changed)
    };
    let mut clock = || Instant::now();

    let error = retain_fixture(&root, b"parent", deadline, &mut clock, &mut respond)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(calls.len(), 3);
}

#[test]
fn invalid_root_mount_id_and_devname_forms_fail_before_open() {
    let root = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut clock = || Instant::now();
    let mut calls = 0usize;
    let mut respond = |_call: FixtureRetainedParentProtocolCall| {
        calls += 1;
        Err(io::Error::other("protocol must not run"))
    };
    let error = retain_gpt_parent_block_device_fixture_with_clock_until(
        &root,
        0,
        b"parent",
        PARENT_MAJOR,
        PARENT_MINOR,
        PARTITION_START,
        PARTITION_SIZE,
        deadline,
        &mut clock,
        &mut respond,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(calls, 0);

    let too_many_components = std::iter::repeat_n("a", 129).collect::<Vec<_>>().join("/").into_bytes();
    let oversized_component = vec![b'a'; 256];
    let oversized_name = vec![b'a'; 4 * 1024];
    let invalid_names: [&[u8]; 9] = [
        b"",
        b"/parent",
        b"parent\0node",
        b".",
        b"..",
        b"parent//node",
        &oversized_component,
        &too_many_components,
        &oversized_name,
    ];
    for name in invalid_names {
        let mut calls = 0usize;
        let mut respond = |_call: FixtureRetainedParentProtocolCall| {
            calls += 1;
            Err(io::Error::other("protocol must not run"))
        };
        let error = retain_fixture(&root, name, deadline, &mut clock, &mut respond)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(calls, 0);
    }
}

#[test]
fn same_name_rebind_rejects_every_identity_access_and_geometry_drift() {
    let changes = [
        BlockDeviceObservation::new(
            ObservedNodeKind::Other,
            ObservedDeviceAccess::ReadOnly,
            41,
            52,
            ROOT_MOUNT_ID,
            PARENT_MAJOR,
            PARENT_MINOR,
            512,
            DEVICE_BYTES,
        ),
        BlockDeviceObservation::new(
            ObservedNodeKind::BlockDevice,
            ObservedDeviceAccess::WriteCapable,
            41,
            52,
            ROOT_MOUNT_ID,
            PARENT_MAJOR,
            PARENT_MINOR,
            512,
            DEVICE_BYTES,
        ),
        observation(42, 52, ROOT_MOUNT_ID, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES),
        observation(41, 53, ROOT_MOUNT_ID, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES),
        observation(41, 52, ROOT_MOUNT_ID + 1, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES),
        observation(41, 52, ROOT_MOUNT_ID, PARENT_MAJOR, 1, 512, DEVICE_BYTES),
        observation(41, 52, ROOT_MOUNT_ID, PARENT_MAJOR, PARENT_MINOR, 1_024, DEVICE_BYTES),
        observation(
            41,
            52,
            ROOT_MOUNT_ID,
            PARENT_MAJOR,
            PARENT_MINOR,
            512,
            DEVICE_BYTES + 512,
        ),
    ];

    for changed in changes {
        let root = tempfile::tempfile().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut opening = |call| standard_result(call, standard_observation());
        let mut clock = || Instant::now();
        let retained = retain_fixture(&root, b"parent", deadline, &mut clock, &mut opening).unwrap();
        let mut rebound = |call| standard_result(call, changed);

        let error = rebind_retained_gpt_parent_fixture_with_clock_until(&retained, deadline, &mut clock, &mut rebound)
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied
        ));
    }
}

#[test]
fn consuming_closing_rebind_rejects_full_observation_drift_when_invoked() {
    let root = tempfile::tempfile().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut opening = |call| standard_result(call, standard_observation());
    let mut clock = || Instant::now();
    let retained = retain_fixture(&root, b"parent", deadline, &mut clock, &mut opening).unwrap();
    let changed = observation(41, 53, ROOT_MOUNT_ID, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES);
    let mut closing = |call| standard_result(call, changed);

    let error =
        close_retained_gpt_parent_fixture_with_clock_until(retained, deadline, &mut clock, &mut closing).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn deadlines_stop_before_open_and_immediately_after_one_injected_call() {
    let root = tempfile::tempfile().unwrap();
    let live = Instant::now();
    let deadline = live + Duration::from_secs(1);
    let expired = deadline + Duration::from_secs(1);
    let mut calls = Vec::new();
    let mut respond = |call: FixtureRetainedParentProtocolCall| {
        calls.push(call.clone());
        standard_result(call, standard_observation())
    };
    let mut expired_clock = || expired;
    let error = retain_fixture(&root, b"parent", deadline, &mut expired_clock, &mut respond)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(calls.is_empty());

    let expired_after_open = Cell::new(false);
    let mut calls = Vec::new();
    let mut respond = |call: FixtureRetainedParentProtocolCall| {
        calls.push(call.clone());
        if matches!(call, FixtureRetainedParentProtocolCall::Open { .. }) {
            expired_after_open.set(true);
        }
        standard_result(call, standard_observation())
    };
    let mut clock = || if expired_after_open.get() { expired } else { live };
    let error = retain_fixture(&root, b"parent", deadline, &mut clock, &mut respond)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0], FixtureRetainedParentProtocolCall::Open { .. }));
}

#[test]
fn sysfs_partition_geometry_is_preflighted_against_logical_blocks_and_capacity() {
    let cases = [
        (
            PARTITION_START,
            PARTITION_SIZE,
            observation(41, 52, ROOT_MOUNT_ID, 8, 0, 256, DEVICE_BYTES),
        ),
        (PARTITION_START, 0, standard_observation()),
        (u64::MAX, PARTITION_SIZE, standard_observation()),
        (
            PARTITION_START,
            PARTITION_SIZE,
            observation(41, 52, ROOT_MOUNT_ID, 8, 0, 512, 1024 * 1024),
        ),
    ];
    for (start, size, observed) in cases {
        let root = tempfile::tempfile().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut respond = |call| standard_result(call, observed);
        let mut clock = || Instant::now();
        let error = retain_gpt_parent_block_device_fixture_with_clock_until(
            &root,
            ROOT_MOUNT_ID,
            b"parent",
            PARENT_MAJOR,
            PARENT_MINOR,
            start,
            size,
            deadline,
            &mut clock,
            &mut respond,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

#[test]
fn ordinary_temporary_file_is_rejected_by_the_real_read_only_opener() {
    let directory = tempfile::tempdir().unwrap();
    File::create(directory.path().join("ordinary-parent")).unwrap();
    let root = File::open(directory.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mount_id = descriptor_mount_id_until(&root, deadline).unwrap();

    let error = retain_gpt_parent_block_device_linux_fixture_until(
        &root,
        mount_id,
        b"ordinary-parent",
        PARENT_MAJOR,
        PARENT_MINOR,
        PARTITION_START,
        PARTITION_SIZE,
        deadline,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

fn retain_fixture<'root, 'name>(
    root: &'root File,
    name: &'name [u8],
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    respond: &mut impl FnMut(FixtureRetainedParentProtocolCall) -> io::Result<FixtureRetainedParentProtocolResult>,
) -> io::Result<super::super::super::gpt_partition_device::RetainedGptParentBlockDevice<'root, 'name>> {
    retain_gpt_parent_block_device_fixture_with_clock_until(
        root,
        ROOT_MOUNT_ID,
        name,
        PARENT_MAJOR,
        PARENT_MINOR,
        PARTITION_START,
        PARTITION_SIZE,
        deadline,
        clock,
        respond,
    )
}

fn standard_result(
    call: FixtureRetainedParentProtocolCall,
    observed: BlockDeviceObservation,
) -> io::Result<FixtureRetainedParentProtocolResult> {
    Ok(match call {
        FixtureRetainedParentProtocolCall::Open { .. } => {
            FixtureRetainedParentProtocolResult::Opened(tempfile::tempfile().unwrap())
        }
        FixtureRetainedParentProtocolCall::DescriptorMountId { .. } => {
            FixtureRetainedParentProtocolResult::DescriptorMountId(ROOT_MOUNT_ID)
        }
        FixtureRetainedParentProtocolCall::Observe { .. } => FixtureRetainedParentProtocolResult::Observation(observed),
    })
}

fn standard_observation() -> BlockDeviceObservation {
    observation(41, 52, ROOT_MOUNT_ID, PARENT_MAJOR, PARENT_MINOR, 512, DEVICE_BYTES)
}

#[allow(clippy::too_many_arguments)]
fn observation(
    containing_device: u64,
    inode: u64,
    mount_id: u64,
    major: u32,
    minor: u32,
    logical_block_size: u32,
    byte_length: u64,
) -> BlockDeviceObservation {
    BlockDeviceObservation::new(
        ObservedNodeKind::BlockDevice,
        ObservedDeviceAccess::ReadOnly,
        containing_device,
        inode,
        mount_id,
        major,
        minor,
        logical_block_size,
        byte_length,
    )
}
