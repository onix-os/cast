use std::{
    io,
    time::{Duration, Instant},
};

use super::support::{FixtureInput, FixtureObserver, ObservationFields, PRODUCTION_LIMITS, authenticate};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn logical_block_size_and_device_length_are_strictly_bounded_and_aligned() {
    for logical_block_size in [0, 256, 1_000, 131_072] {
        let mut fields = ObservationFields::standard();
        fields.logical_block_size = logical_block_size;
        let mut input = FixtureInput::standard();
        input.validated_logical_block_size = logical_block_size;
        let mut observer = FixtureObserver::stable(fields);
        assert_eq!(
            authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    let mut fields = ObservationFields::standard();
    fields.byte_length += 1;
    let mut input = FixtureInput::standard();
    input.validated_image_bytes = fields.byte_length;
    let mut observer = FixtureObserver::stable(fields);
    assert_eq!(
        authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn sysfs_and_gpt_start_or_size_disagreement_is_rejected() {
    for mismatch in 0..4 {
        let mut input = FixtureInput::standard();
        match mismatch {
            0 => input.start_512_sectors += 1,
            1 => input.size_512_sectors += 1,
            2 => input.start_lba += 1,
            _ => input.size_lba += 1,
        }
        let mut observer = FixtureObserver::stable(ObservationFields::standard());
        assert_eq!(
            authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn every_sector_to_byte_multiplication_overflow_fails_closed() {
    let mut sysfs_start = FixtureInput::standard();
    sysfs_start.start_512_sectors = u64::MAX;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, sysfs_start, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let mut sysfs_size = FixtureInput::standard();
    sysfs_size.size_512_sectors = u64::MAX;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, sysfs_size, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let mut fields = ObservationFields::standard();
    fields.logical_block_size = 4_096;
    fields.byte_length = u64::MAX - 4_095;
    for is_start in [true, false] {
        let mut input = FixtureInput::standard();
        input.start_512_sectors = 0;
        input.start_lba = if is_start { u64::MAX } else { 0 };
        input.size_512_sectors = 1;
        input.size_lba = if is_start { 1 } else { u64::MAX };
        input.validated_logical_block_size = fields.logical_block_size;
        input.validated_image_bytes = fields.byte_length;
        let mut observer = FixtureObserver::stable(fields);
        assert_eq!(
            authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn partition_range_must_fit_without_overflow_inside_parent_length() {
    let mut outside = FixtureInput::standard();
    outside.start_512_sectors = (64 * 1024 * 1024 / 512) - 1;
    outside.start_lba = outside.start_512_sectors;
    outside.size_512_sectors = 2;
    outside.size_lba = 2;
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    assert_eq!(
        authenticate(&mut observer, outside, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let aligned_max = u64::MAX - 511;
    let mut fields = ObservationFields::standard();
    fields.byte_length = aligned_max;
    let mut overflowing = FixtureInput::standard();
    overflowing.start_512_sectors = (u64::MAX / 512) - 1;
    overflowing.start_lba = overflowing.start_512_sectors;
    overflowing.size_512_sectors = 4;
    overflowing.size_lba = 4;
    overflowing.validated_image_bytes = aligned_max;
    let mut observer = FixtureObserver::stable(fields);
    assert_eq!(
        authenticate(&mut observer, overflowing, PRODUCTION_LIMITS, deadline())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
