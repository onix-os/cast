use std::{
    io,
    time::{Duration, Instant},
};

use super::super::sysfs_block::{
    SYSFS_DEV_ATTRIBUTE_MAX_BYTES, SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES, SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES,
    parse_sysfs_dev, parse_sysfs_dev_until, parse_sysfs_partition_geometry, parse_sysfs_partition_geometry_until,
    parse_sysfs_partition_number, parse_sysfs_partition_number_until,
};

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn dev_attribute_accepts_exact_canonical_u32_pairs() {
    let minimum = parse_sysfs_dev(b"0:0\n").unwrap();
    assert_eq!((minimum.major(), minimum.minor()), (0, 0));

    let ordinary = parse_sysfs_dev(b"259:1048575\n").unwrap();
    assert_eq!((ordinary.major(), ordinary.minor()), (259, 1_048_575));

    let maximum = parse_sysfs_dev(b"4294967295:4294967295\n").unwrap();
    assert_eq!((maximum.major(), maximum.minor()), (u32::MAX, u32::MAX));
}

#[test]
fn dev_attribute_rejects_noncanonical_or_out_of_range_numbers() {
    for input in [
        b":1\n".as_slice(),
        b"1:\n",
        b"01:1\n",
        b"1:01\n",
        b"+1:1\n",
        b"1:-1\n",
        b"1 1\n",
        b"1:1:1\n",
        b"4294967296:1\n",
        b"1:4294967296\n",
        b"1:1\n\n",
        b"1:\xff\n",
    ] {
        invalid_data(parse_sysfs_dev(input));
    }

    assert_eq!(parse_sysfs_dev(b"").unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        parse_sysfs_dev(b"1:1").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn dev_attribute_enforces_the_exact_maximum_length_boundary() {
    let at_limit = b"4294967295:4294967295\n";
    assert_eq!(at_limit.len(), 22);
    parse_sysfs_dev(at_limit).unwrap();

    let beyond_limit = b"04294967295:4294967295\n";
    assert_eq!(beyond_limit.len(), 23);
    invalid_data(parse_sysfs_dev(beyond_limit));
}

#[test]
fn partition_attribute_accepts_only_positive_canonical_u32() {
    assert_eq!(parse_sysfs_partition_number(b"1\n").unwrap().get(), 1);
    assert_eq!(parse_sysfs_partition_number(b"4294967295\n").unwrap().get(), u32::MAX);

    for input in [
        b"0\n".as_slice(),
        b"00\n",
        b"01\n",
        b"+1\n",
        b"-1\n",
        b"1 \n",
        b"4294967296\n",
        b"1\n\n",
    ] {
        invalid_data(parse_sysfs_partition_number(input));
    }
    assert_eq!(
        parse_sysfs_partition_number(b"1").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn partition_attribute_enforces_the_exact_maximum_length_boundary() {
    let at_limit = b"4294967295\n";
    assert_eq!(at_limit.len(), 11);
    assert_eq!(parse_sysfs_partition_number(at_limit).unwrap().get(), u32::MAX);

    let beyond_limit = b"04294967295\n";
    assert_eq!(beyond_limit.len(), 12);
    invalid_data(parse_sysfs_partition_number(beyond_limit));
}

#[test]
fn numeric_deadline_entrypoints_reject_expired_work_and_expose_exact_read_ceilings() {
    assert_eq!(SYSFS_DEV_ATTRIBUTE_MAX_BYTES, 22);
    assert_eq!(SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES, 11);
    let live = Instant::now() + Duration::from_secs(1);
    assert_eq!(parse_sysfs_dev_until(b"259:1\n", live).unwrap().minor(), 1);
    assert_eq!(parse_sysfs_partition_number_until(b"1\n", live).unwrap().get(), 1);

    let expired = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        parse_sysfs_dev_until(b"259:1\n", expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        parse_sysfs_partition_number_until(b"1\n", expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn partition_geometry_retains_canonical_512_byte_sector_units() {
    let ordinary = parse_sysfs_partition_geometry(b"2048\n", b"1048576\n").unwrap();
    assert_eq!(ordinary.start_512_sectors(), 2_048);
    assert_eq!(ordinary.size_512_sectors(), 1_048_576);

    let boundary = parse_sysfs_partition_geometry(b"0\n", b"18446744073709551615\n").unwrap();
    assert_eq!(boundary.start_512_sectors(), 0);
    assert_eq!(boundary.size_512_sectors(), u64::MAX);
}

#[test]
fn partition_geometry_rejects_noncanonical_zero_size_and_overflow() {
    for start in [
        b"".as_slice(),
        b"00\n",
        b"01\n",
        b"+1\n",
        b"-1\n",
        b"1 \n",
        b"18446744073709551616\n",
    ] {
        let result = parse_sysfs_partition_geometry(start, b"1\n");
        assert!(matches!(
            result.unwrap_err().kind(),
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
        ));
    }
    for size in [
        b"0\n".as_slice(),
        b"00\n",
        b"01\n",
        b"+1\n",
        b"-1\n",
        b"1 \n",
        b"18446744073709551616\n",
    ] {
        invalid_data(parse_sysfs_partition_geometry(b"0\n", size));
    }
    assert_eq!(
        parse_sysfs_partition_geometry(b"0", b"1\n").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        parse_sysfs_partition_geometry(b"0\n", b"1").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn partition_geometry_deadline_and_attribute_ceiling_are_exact() {
    assert_eq!(SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES, 21);
    let maximum = b"18446744073709551615\n";
    assert_eq!(maximum.len(), SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES);
    let live = Instant::now() + Duration::from_secs(1);
    assert_eq!(
        parse_sysfs_partition_geometry_until(maximum, maximum, live)
            .unwrap()
            .size_512_sectors(),
        u64::MAX
    );

    let overbound = b"018446744073709551615\n";
    assert_eq!(overbound.len(), SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES + 1);
    invalid_data(parse_sysfs_partition_geometry(overbound, b"1\n"));
    let expired = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        parse_sysfs_partition_geometry_until(b"0\n", b"1\n", expired)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
}
