use std::{
    io,
    time::{Duration, Instant},
};

use super::super::sysfs_block::{
    parse_sysfs_disk_identity, parse_sysfs_disk_identity_until, parse_sysfs_partition_identity,
    parse_sysfs_partition_identity_until, require_matching_disk_sequence, require_matching_disk_sequence_until,
};

const PARTUUID: &str = "5e85a94f-b115-41c5-9d72-9d23958b5edc";

fn partition_event(major: &str, minor: &str, partn: &str, partuuid: &str, diskseq: Option<&str>) -> Vec<u8> {
    let mut bytes = format!(
        "MAJOR={major}\nMINOR={minor}\nDEVNAME=nvme1n1p1\nDEVTYPE=partition\nPARTN={partn}\nPARTUUID={partuuid}\n"
    )
    .into_bytes();
    if let Some(diskseq) = diskseq {
        bytes.extend_from_slice(format!("DISKSEQ={diskseq}\n").as_bytes());
    }
    bytes.extend_from_slice(b"FUTURE=retained=exactly\n");
    bytes
}

fn disk_event(major: &str, minor: &str, diskseq: Option<&str>) -> Vec<u8> {
    let mut bytes = format!("MAJOR={major}\nMINOR={minor}\nDEVNAME=nvme1n1\nDEVTYPE=disk\n").into_bytes();
    if let Some(diskseq) = diskseq {
        bytes.extend_from_slice(format!("DISKSEQ={diskseq}\n").as_bytes());
    }
    bytes
}

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn partition_identity_cross_checks_all_required_attributes_and_retains_event() {
    let identity = parse_sysfs_partition_identity(
        b"259:1\n",
        b"1\n",
        &partition_event("259", "1", "1", PARTUUID, Some("9")),
    )
    .unwrap();

    assert_eq!((identity.device().major(), identity.device().minor()), (259, 1));
    assert_eq!(identity.partition_number().get(), 1);
    assert_eq!(identity.partition_uuid().as_str(), PARTUUID);
    assert_eq!(identity.partition_uuid().as_bytes(), PARTUUID.as_bytes());
    assert_eq!(identity.disk_sequence().unwrap().get(), 9);
    assert_eq!(identity.uevent().value(b"FUTURE"), Some(b"retained=exactly".as_slice()));
}

#[test]
fn partition_identity_rejects_cross_file_disagreement() {
    invalid_data(parse_sysfs_partition_identity(
        b"259:2\n",
        b"1\n",
        &partition_event("259", "1", "1", PARTUUID, None),
    ));
    invalid_data(parse_sysfs_partition_identity(
        b"259:1\n",
        b"2\n",
        &partition_event("259", "1", "1", PARTUUID, None),
    ));

    let wrong_type = disk_event("259", "1", None);
    invalid_data(parse_sysfs_partition_identity(b"259:1\n", b"1\n", &wrong_type));
}

#[test]
fn partition_identity_requires_canonical_known_fields() {
    for event in [
        partition_event("0259", "1", "1", PARTUUID, None),
        partition_event("259", "01", "1", PARTUUID, None),
        partition_event("259", "1", "01", PARTUUID, None),
        partition_event("259", "1", "1", "5E85A94F-b115-41c5-9d72-9d23958b5edc", None),
        partition_event("259", "1", "1", "00000000-0000-0000-0000-000000000000", None),
        partition_event("259", "1", "1", PARTUUID, Some("0")),
        partition_event("259", "1", "1", PARTUUID, Some("09")),
    ] {
        invalid_data(parse_sysfs_partition_identity(b"259:1\n", b"1\n", &event));
    }

    let missing_uuid = b"MAJOR=259\nMINOR=1\nDEVTYPE=partition\nPARTN=1\n";
    invalid_data(parse_sysfs_partition_identity(b"259:1\n", b"1\n", missing_uuid));
}

#[test]
fn disk_identity_requires_disk_type_and_matching_device_number() {
    let identity = parse_sysfs_disk_identity(b"259:0\n", &disk_event("259", "0", Some("9"))).unwrap();
    assert_eq!((identity.device().major(), identity.device().minor()), (259, 0));
    assert_eq!(identity.disk_sequence().unwrap().get(), 9);
    assert_eq!(identity.uevent().value(b"DEVTYPE"), Some(b"disk".as_slice()));

    invalid_data(parse_sysfs_disk_identity(
        b"259:0\n",
        &disk_event("259", "1", Some("9")),
    ));
    invalid_data(parse_sysfs_disk_identity(
        b"259:0\n",
        &partition_event("259", "0", "1", PARTUUID, Some("9")),
    ));

    let partition_field_on_disk = b"MAJOR=259\nMINOR=0\nDEVTYPE=disk\nPARTN=1\n";
    invalid_data(parse_sysfs_disk_identity(b"259:0\n", partition_field_on_disk));
}

#[test]
fn optional_disk_sequence_must_be_absent_on_both_or_equal_on_both() {
    let partition_without =
        parse_sysfs_partition_identity(b"259:1\n", b"1\n", &partition_event("259", "1", "1", PARTUUID, None)).unwrap();
    let disk_without = parse_sysfs_disk_identity(b"259:0\n", &disk_event("259", "0", None)).unwrap();
    assert_eq!(
        require_matching_disk_sequence(&partition_without, &disk_without).unwrap(),
        None
    );

    let partition_nine = parse_sysfs_partition_identity(
        b"259:1\n",
        b"1\n",
        &partition_event("259", "1", "1", PARTUUID, Some("9")),
    )
    .unwrap();
    let disk_nine = parse_sysfs_disk_identity(b"259:0\n", &disk_event("259", "0", Some("9"))).unwrap();
    assert_eq!(
        require_matching_disk_sequence(&partition_nine, &disk_nine)
            .unwrap()
            .unwrap()
            .get(),
        9
    );

    let disk_ten = parse_sysfs_disk_identity(b"259:0\n", &disk_event("259", "0", Some("10"))).unwrap();
    invalid_data(require_matching_disk_sequence(&partition_nine, &disk_ten));
    invalid_data(require_matching_disk_sequence(&partition_nine, &disk_without));
    invalid_data(require_matching_disk_sequence(&partition_without, &disk_nine));
}

#[test]
fn identity_deadline_entrypoints_share_one_deadline_and_never_succeed_after_expiry() {
    let live = Instant::now() + Duration::from_secs(1);
    let partition = parse_sysfs_partition_identity_until(
        b"259:1\n",
        b"1\n",
        &partition_event("259", "1", "1", PARTUUID, Some("9")),
        live,
    )
    .unwrap();
    let disk = parse_sysfs_disk_identity_until(b"259:0\n", &disk_event("259", "0", Some("9")), live).unwrap();
    assert_eq!(
        require_matching_disk_sequence_until(&partition, &disk, live)
            .unwrap()
            .unwrap()
            .get(),
        9
    );

    let expired = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        parse_sysfs_partition_identity_until(
            b"259:1\n",
            b"1\n",
            &partition_event("259", "1", "1", PARTUUID, Some("9")),
            expired,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        parse_sysfs_disk_identity_until(b"259:0\n", &disk_event("259", "0", Some("9")), expired)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        require_matching_disk_sequence_until(&partition, &disk, expired)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
}
