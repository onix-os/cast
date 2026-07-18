use std::io;

use super::super::sysfs_block::{parse_sysfs_dev, parse_sysfs_partition_number};

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
