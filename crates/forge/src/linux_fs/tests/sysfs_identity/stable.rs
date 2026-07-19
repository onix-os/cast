use std::{fs, io};

use super::super::super::{sysfs_block::SysfsDeviceNumber, sysfs_identity::FixtureSysfsTree};
use super::support::{
    DISK_MAJOR, DISK_MINOR, DISK_NAME, DISK_SEQUENCE, FixtureEntry, PARTITION_MAJOR, PARTITION_MINOR, PARTITION_NAME,
    PARTITION_NUMBER, PARTITION_UUID, SIBLING_PARTITION_MINOR, SIBLING_PARTITION_UUID, SyntheticSysfs,
};

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn stable_fixture_captures_and_revalidates_exact_partition_identity() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();

    let view = prepared.revalidate().unwrap();
    assert_eq!(view.device(), device());
    assert_eq!(view.partition_number().get(), PARTITION_NUMBER);
    assert_eq!(view.partition_uuid().as_str(), PARTITION_UUID);
    assert_eq!(view.disk_sequence().unwrap().get(), DISK_SEQUENCE);
    assert_eq!(view.normalized_devpath(), fixture.logical_device_path());
    assert_eq!(view.partition_device_name(), PARTITION_NAME.as_bytes());
    assert_eq!(view.parent_device_name(), DISK_NAME.as_bytes());
    fixture.assert_outside_unchanged();
}

#[test]
fn revalidated_views_compare_only_retained_block_parent_snapshots() {
    let fixture = SyntheticSysfs::stable().unwrap();
    fixture.add_sibling_partition().unwrap();
    let tree = admitted(&fixture).unwrap();
    let first = tree.prepare(device()).unwrap();
    let second = tree.prepare(device()).unwrap();
    let sibling_device = SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, SIBLING_PARTITION_MINOR);
    let sibling = tree.prepare(sibling_device).unwrap();
    let first_view = first.revalidate().unwrap();
    let second_view = second.revalidate().unwrap();
    let sibling_view = sibling.revalidate().unwrap();
    assert!(first_view.has_same_revalidated_block_parent_snapshot(&second_view));
    assert_eq!(sibling_view.device(), sibling_device);
    assert_eq!(sibling_view.partition_uuid().as_str(), SIBLING_PARTITION_UUID);
    assert!(first_view.has_same_revalidated_block_parent_snapshot(&sibling_view));

    let other_fixture = SyntheticSysfs::stable().unwrap();
    let other = admitted(&other_fixture).unwrap().prepare(device()).unwrap();
    let other_view = other.revalidate().unwrap();
    assert!(!first_view.has_same_revalidated_block_parent_snapshot(&other_view));
    fixture.assert_outside_unchanged();
    other_fixture.assert_outside_unchanged();
}

#[test]
fn non_block_intermediate_ancestors_are_skipped_without_lexical_parent_assumptions() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();
    assert_eq!(prepared.revalidate().unwrap().device(), device());

    let missing_subsystem = SyntheticSysfs::stable().unwrap();
    missing_subsystem.remove(FixtureEntry::IntermediateSubsystem).unwrap();
    assert_eq!(
        admitted(&missing_subsystem)
            .unwrap()
            .prepare(device())
            .unwrap()
            .revalidate()
            .unwrap()
            .device(),
        device()
    );
}

#[test]
fn nearest_block_ancestor_must_itself_be_an_exact_disk() {
    let non_disk = SyntheticSysfs::stable().unwrap();
    non_disk
        .add_nearer_block_ancestor(
            format!(
                "MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=partition\nPARTN={PARTITION_NUMBER}\nPARTUUID={PARTITION_UUID}\nDISKSEQ={DISK_SEQUENCE}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    invalid_data(admitted(&non_disk).unwrap().prepare(device()));

    let malformed = SyntheticSysfs::stable().unwrap();
    malformed
        .add_nearer_block_ancestor(
            format!("MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDISKSEQ={DISK_SEQUENCE}\n").as_bytes(),
        )
        .unwrap();
    invalid_data(admitted(&malformed).unwrap().prepare(device()));
}

#[test]
fn selected_parent_must_not_itself_have_a_partition_attribute() {
    let fixture = SyntheticSysfs::stable().unwrap();
    fs::write(fixture.entry(FixtureEntry::DiskDirectory).join("partition"), b"9\n").unwrap();
    invalid_data(admitted(&fixture).unwrap().prepare(device()));
}

#[test]
fn disk_sequence_is_either_absent_on_both_nodes_or_equal_on_both() {
    let absent = SyntheticSysfs::without_disk_sequence().unwrap();
    let prepared = admitted(&absent).unwrap().prepare(device()).unwrap();
    assert_eq!(prepared.revalidate().unwrap().disk_sequence(), None);

    let missing_parent = SyntheticSysfs::stable().unwrap();
    missing_parent
        .replace_regular(
            FixtureEntry::DiskEvent,
            format!("MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=disk\n").as_bytes(),
        )
        .unwrap();
    invalid_data(admitted(&missing_parent).unwrap().prepare(device()));

    let mismatch = SyntheticSysfs::stable().unwrap();
    mismatch
        .replace_regular(
            FixtureEntry::DiskEvent,
            format!(
                "MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=disk\nDISKSEQ={}\n",
                DISK_SEQUENCE - 1
            )
            .as_bytes(),
        )
        .unwrap();
    invalid_data(admitted(&mismatch).unwrap().prepare(device()));
}
