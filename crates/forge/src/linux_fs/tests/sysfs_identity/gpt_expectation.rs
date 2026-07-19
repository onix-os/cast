use std::io;

use super::super::super::{
    sysfs_block::SysfsDeviceNumber,
    sysfs_identity::{FixtureSysfsTree, RevalidatedSysfsPartitionIdentity, SysfsGptDeviceExpectation},
};
use super::support::{
    DISK_MAJOR, DISK_MINOR, DISK_NAME, DISK_SEQUENCE, FixtureEntry, PARTITION_MAJOR, PARTITION_MINOR, PARTITION_NUMBER,
    PARTITION_SIZE_512_SECTORS, PARTITION_START_512_SECTORS, PARTITION_UUID, SyntheticSysfs,
};

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

// This signature is a compile-time contract: the expectation cannot outlive
// the exact freshly revalidated view from which it borrowed parent DEVNAME.
fn bind_expectation<'view>(view: &'view RevalidatedSysfsPartitionIdentity<'_>) -> SysfsGptDeviceExpectation<'view> {
    view.gpt_device_expectation()
}

#[test]
fn expectation_binds_every_exact_gpt_fact_to_one_revalidated_view() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();
    let revalidated = prepared.revalidate().unwrap();
    let expectation = bind_expectation(&revalidated);

    assert_eq!(expectation.authenticated_parent_devname(), DISK_NAME.as_bytes());
    assert_eq!(
        expectation.parent_device(),
        SysfsDeviceNumber::from_major_minor(DISK_MAJOR, DISK_MINOR)
    );
    assert_eq!(expectation.partition_number().get(), PARTITION_NUMBER);
    assert_eq!(expectation.partition_uuid().as_str(), PARTITION_UUID);
    assert_eq!(expectation.partition_start_512_sectors(), PARTITION_START_512_SECTORS);
    assert_eq!(expectation.partition_size_512_sectors(), PARTITION_SIZE_512_SECTORS);
    assert_eq!(expectation.disk_sequence().unwrap().get(), DISK_SEQUENCE);
    fixture.assert_outside_unchanged();

    let absent = SyntheticSysfs::without_disk_sequence().unwrap();
    let absent_prepared = admitted(&absent).unwrap().prepare(device()).unwrap();
    let absent_revalidated = absent_prepared.revalidate().unwrap();
    assert_eq!(bind_expectation(&absent_revalidated).disk_sequence(), None);
    absent.assert_outside_unchanged();
}

#[test]
fn parent_device_number_drift_cannot_produce_an_expectation() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();
    let changed_major = DISK_MAJOR - 1;
    let changed_minor = DISK_MINOR - 1;
    fixture
        .overwrite_regular(
            FixtureEntry::DiskDevice,
            format!("{changed_major}:{changed_minor}\n").as_bytes(),
        )
        .unwrap();
    fixture
        .overwrite_regular(
            FixtureEntry::DiskEvent,
            format!(
                "MAJOR={changed_major}\nMINOR={changed_minor}\nDEVNAME={DISK_NAME}\nDEVTYPE=disk\nDISKSEQ={DISK_SEQUENCE}\n"
            )
            .as_bytes(),
        )
        .unwrap();

    let error = prepared.revalidate().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    fixture.assert_outside_unchanged();
}
