use std::{
    io,
    time::{Duration, Instant},
};

use super::super::super::gpt_partition_device::{ObservedDeviceAccess, ObservedNodeKind};
use super::support::{FixtureInput, FixtureObserver, ObservationFields, PRODUCTION_LIMITS, authenticate};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn every_opening_closing_identity_or_geometry_change_is_rejected() {
    let opening = ObservationFields::standard();
    let mut closings = [opening; 7];
    closings[0].containing_device += 1;
    closings[1].inode += 1;
    closings[2].mount_id += 1;
    closings[3].block_major += 1;
    closings[4].block_minor += 1;
    closings[5].logical_block_size = 4_096;
    closings[6].byte_length += 512;

    for closing in closings {
        let mut observer = FixtureObserver::changing(opening, closing);
        let error = authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(observer.calls(), 2);
    }
}

#[test]
fn non_block_and_write_capable_descriptors_fail_before_second_observation() {
    let mut non_block = ObservationFields::standard();
    non_block.node_kind = ObservedNodeKind::Other;
    let mut observer = FixtureObserver::stable(non_block);
    assert_eq!(
        authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(observer.calls(), 1);

    let mut write_capable = ObservationFields::standard();
    write_capable.access = ObservedDeviceAccess::WriteCapable;
    let mut observer = FixtureObserver::stable(write_capable);
    assert_eq!(
        authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(observer.calls(), 1);
}

#[test]
fn zero_containing_device_inode_mount_id_and_length_are_not_admitted_as_identity() {
    for mutation in 0..4 {
        let mut fields = ObservationFields::standard();
        match mutation {
            0 => fields.containing_device = 0,
            1 => fields.inode = 0,
            2 => fields.mount_id = 0,
            _ => fields.byte_length = 0,
        }
        let mut observer = FixtureObserver::stable(fields);
        assert_eq!(
            authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(observer.calls(), 1);
    }
}

#[test]
fn parent_rdev_must_match_the_authenticated_sysfs_parent_exactly() {
    for (major, minor) in [(9, 0), (8, 1)] {
        let mut input = FixtureInput::standard();
        input.parent_major = major;
        input.parent_minor = minor;
        let mut observer = FixtureObserver::stable(ObservationFields::standard());
        assert_eq!(
            authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(observer.calls(), 1);
    }
}

#[test]
fn gpt_uuid_and_partition_number_are_rechecked() {
    let mut wrong_uuid = FixtureInput::standard();
    wrong_uuid.validated_partition_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, wrong_uuid, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(observer.calls(), 1);

    let mut wrong_number = FixtureInput::standard();
    wrong_number.validated_partition_number = 2;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, wrong_number, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(observer.calls(), 1);
}

#[test]
fn gpt_image_size_and_logical_block_size_are_bound_to_the_observation() {
    let mut wrong_block_size = FixtureInput::standard();
    wrong_block_size.validated_logical_block_size = 4_096;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, wrong_block_size, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(observer.calls(), 1);

    let mut wrong_length = FixtureInput::standard();
    wrong_length.validated_image_bytes += 512;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, wrong_length, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(observer.calls(), 1);
}

#[test]
fn observer_errors_propagate_without_retry_or_extra_calls() {
    for call in [0, 1] {
        let mut observer = FixtureObserver::failing(ObservationFields::standard(), call);
        let error = authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(observer.calls(), call + 1);
    }
}
