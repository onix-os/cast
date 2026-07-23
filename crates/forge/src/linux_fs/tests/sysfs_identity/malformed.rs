use std::{ffi::CString, fs, io};

use super::super::super::{sysfs_block::SysfsDeviceNumber, sysfs_identity::FixtureSysfsTree};
use super::support::{
    DISK_MAJOR, DISK_MINOR, DISK_SEQUENCE, FixtureEntry, PARTITION_MAJOR, PARTITION_MINOR, PARTITION_NUMBER,
    PARTITION_UUID, SyntheticSysfs,
};

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

fn error_kind<T>(result: io::Result<T>) -> io::ErrorKind {
    match result {
        Ok(_) => panic!("synthetic sysfs input unexpectedly succeeded"),
        Err(error) => error.kind(),
    }
}

fn assert_invalid<T>(result: io::Result<T>) {
    assert_eq!(error_kind(result), io::ErrorKind::InvalidData);
}

fn assert_refused<T>(result: io::Result<T>) {
    let error = match result {
        Ok(_) => panic!("synthetic sysfs input unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error.kind(),
            io::ErrorKind::InvalidData
                | io::ErrorKind::InvalidInput
                | io::ErrorKind::NotFound
                | io::ErrorKind::NotADirectory
        ) || error.raw_os_error() == Some(nix::libc::ELOOP)
    );
}

#[test]
fn fixture_admission_accepts_only_one_retained_directory_component() {
    let fixture = SyntheticSysfs::stable().unwrap();
    for invalid_name in ["", ".", "..", "nested/name"] {
        let (parent, _) = fixture.admission().unwrap();
        assert_refused(FixtureSysfsTree::admit(parent, CString::new(invalid_name).unwrap()));
    }

    let regular_parent = fs::File::open(fixture.entry(FixtureEntry::PartitionDevice)).unwrap();
    assert_refused(FixtureSysfsTree::admit(regular_parent, CString::new("child").unwrap()));

    let symlink_root = SyntheticSysfs::stable().unwrap();
    symlink_root.replace_root_with_symlink().unwrap();
    assert_refused(admitted(&symlink_root));
}

#[test]
fn every_required_lookup_target_and_attribute_must_exist() {
    for entry in [
        FixtureEntry::Lookup,
        FixtureEntry::PartitionDevice,
        FixtureEntry::PartitionNumber,
        FixtureEntry::PartitionStart,
        FixtureEntry::PartitionSize,
        FixtureEntry::PartitionEvent,
        FixtureEntry::PartitionSubsystem,
        FixtureEntry::DiskDevice,
        FixtureEntry::DiskEvent,
        FixtureEntry::DiskSubsystem,
    ] {
        let fixture = SyntheticSysfs::stable().unwrap();
        fixture.remove(entry).unwrap();
        assert_refused(admitted(&fixture).unwrap().prepare(device()));
        fixture.assert_outside_unchanged();
    }
}

#[test]
fn dev_block_lookup_target_cannot_escape_or_name_a_non_device_path() {
    for target in [
        b"../../../outside".as_slice(),
        b"../../class/block/not-a-device".as_slice(),
        b"../../devices/../outside".as_slice(),
        b"../../devices".as_slice(),
        b"/absolute".as_slice(),
        b"../../devices//empty".as_slice(),
    ] {
        let fixture = SyntheticSysfs::stable().unwrap();
        fixture.replace_symlink(FixtureEntry::Lookup, target).unwrap();
        assert_invalid(admitted(&fixture).unwrap().prepare(device()));
    }
}

#[test]
fn target_and_attribute_entry_kinds_fail_closed_without_opening_special_files() {
    let target = SyntheticSysfs::stable().unwrap();
    target
        .replace_regular(FixtureEntry::PartitionDirectory, b"not a directory\n")
        .unwrap();
    assert_refused(admitted(&target).unwrap().prepare(device()));

    for entry in [
        FixtureEntry::PartitionDevice,
        FixtureEntry::PartitionNumber,
        FixtureEntry::PartitionStart,
        FixtureEntry::PartitionSize,
        FixtureEntry::PartitionEvent,
        FixtureEntry::DiskDevice,
        FixtureEntry::DiskEvent,
    ] {
        let fixture = SyntheticSysfs::stable().unwrap();
        fixture.replace_fifo(entry).unwrap();
        assert_invalid(admitted(&fixture).unwrap().prepare(device()));
        fixture.assert_outside_unchanged();
    }
}

#[test]
fn partition_attributes_reject_non_utf8_and_cross_file_disagreement() {
    for (entry, contents) in [
        (FixtureEntry::PartitionDevice, b"4294967295:\xff\n".as_slice()),
        (FixtureEntry::PartitionNumber, b"\xff\n".as_slice()),
        (
            FixtureEntry::PartitionEvent,
            b"MAJOR=4294967295\nMINOR=4294967294\nDEVTYPE=partition\nPARTN=7\nPARTUUID=\xff\nDISKSEQ=18446744073709551608\n"
                .as_slice(),
        ),
        (FixtureEntry::PartitionDevice, b"4294967295:4294967293\n".as_slice()),
        (FixtureEntry::PartitionNumber, b"8\n".as_slice()),
        (FixtureEntry::PartitionStart, b"01\n".as_slice()),
        (FixtureEntry::PartitionSize, b"0\n".as_slice()),
    ] {
        let fixture = SyntheticSysfs::stable().unwrap();
        fixture.replace_regular(entry, contents).unwrap();
        assert_invalid(admitted(&fixture).unwrap().prepare(device()));
    }
}

#[test]
fn parent_attributes_reject_non_disk_or_internally_inconsistent_evidence() {
    for event in [
        format!(
            "MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=partition\nPARTN={PARTITION_NUMBER}\nPARTUUID={PARTITION_UUID}\nDISKSEQ={DISK_SEQUENCE}\n"
        ),
        format!(
            "MAJOR={DISK_MAJOR}\nMINOR={}\nDEVTYPE=disk\nDISKSEQ={DISK_SEQUENCE}\n",
            DISK_MINOR - 1
        ),
        format!("MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=disk\nDISKSEQ=0\n"),
    ] {
        let fixture = SyntheticSysfs::stable().unwrap();
        fixture
            .replace_regular(FixtureEntry::DiskEvent, event.as_bytes())
            .unwrap();
        assert_invalid(admitted(&fixture).unwrap().prepare(device()));
    }

    let same_device = SyntheticSysfs::stable().unwrap();
    same_device
        .replace_regular(
            FixtureEntry::DiskDevice,
            format!("{PARTITION_MAJOR}:{PARTITION_MINOR}\n").as_bytes(),
        )
        .unwrap();
    same_device
        .replace_regular(
            FixtureEntry::DiskEvent,
            format!("MAJOR={PARTITION_MAJOR}\nMINOR={PARTITION_MINOR}\nDEVTYPE=disk\nDISKSEQ={DISK_SEQUENCE}\n")
                .as_bytes(),
        )
        .unwrap();
    assert_invalid(admitted(&same_device).unwrap().prepare(device()));

    let non_utf8_subsystem = SyntheticSysfs::stable().unwrap();
    non_utf8_subsystem
        .replace_symlink(FixtureEntry::DiskSubsystem, b"../../../class/bl\xffck")
        .unwrap();
    assert_invalid(admitted(&non_utf8_subsystem).unwrap().prepare(device()));
}

#[test]
fn caller_device_number_is_exact_and_never_triggers_discovery() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let different = SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR - 1, PARTITION_MINOR - 1);
    assert_eq!(
        error_kind(admitted(&fixture).unwrap().prepare(different)),
        io::ErrorKind::NotFound
    );
    fixture.assert_outside_unchanged();
}
