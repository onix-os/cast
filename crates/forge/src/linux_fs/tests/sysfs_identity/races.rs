use std::{
    fs, io,
    time::{Duration, Instant},
};

use super::super::super::{
    sysfs_block::SysfsDeviceNumber,
    sysfs_identity::{FixtureAttribute, FixtureCheckpoint, FixtureNode, FixtureSysfsIdentityLimits, FixtureSysfsTree},
};
use super::support::{
    DISK_MAJOR, DISK_MINOR, DISK_SEQUENCE, FixtureEntry, PARTITION_MAJOR, PARTITION_MINOR, PARTITION_NUMBER,
    SyntheticSysfs,
};

fn device() -> SysfsDeviceNumber {
    SysfsDeviceNumber::from_major_minor(PARTITION_MAJOR, PARTITION_MINOR)
}

fn admitted(fixture: &SyntheticSysfs) -> io::Result<FixtureSysfsTree> {
    let (parent, root_name) = fixture.admission()?;
    FixtureSysfsTree::admit(parent, root_name)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn assert_prepare_race(fixture: &SyntheticSysfs, mut hook: impl FnMut(FixtureCheckpoint) -> io::Result<()>) {
    let tree = admitted(fixture).unwrap();
    let result = tree.prepare_with(device(), FixtureSysfsIdentityLimits::default(), deadline(), &mut hook);
    let error = match result {
        Ok(_) => panic!("descriptor race unexpectedly produced prepared sysfs evidence"),
        Err(error) => error,
    };
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    fixture.assert_outside_unchanged();
}

#[test]
fn retained_root_and_lookup_name_races_fail_closed() {
    let root = SyntheticSysfs::stable().unwrap();
    let mut root_seen = false;
    assert_prepare_race(&root, |checkpoint| {
        if checkpoint == FixtureCheckpoint::RootRebind && !root_seen {
            root_seen = true;
            root.replace_root_directory()?;
        }
        Ok(())
    });
    assert!(root_seen);

    let lookup = SyntheticSysfs::stable().unwrap();
    let mut lookup_seen = false;
    assert_prepare_race(&lookup, |checkpoint| {
        if checkpoint == FixtureCheckpoint::LookupPinned && !lookup_seen {
            lookup_seen = true;
            lookup.replace_symlink(FixtureEntry::Lookup, &lookup.lookup_target())?;
        }
        Ok(())
    });
    assert!(lookup_seen);
}

#[test]
fn lookup_link_and_normalized_target_races_fail_closed_between_passes() {
    let link = SyntheticSysfs::stable().unwrap();
    let mut rebound_count = 0usize;
    assert_prepare_race(&link, |checkpoint| {
        if checkpoint == FixtureCheckpoint::LookupRebound {
            rebound_count += 1;
            if rebound_count == 1 {
                link.replace_symlink(FixtureEntry::Lookup, &link.lookup_target())?;
            }
        }
        Ok(())
    });
    assert_eq!(rebound_count, 1);

    let target = SyntheticSysfs::stable().unwrap();
    let mut target_seen = false;
    assert_prepare_race(&target, |checkpoint| {
        if checkpoint == FixtureCheckpoint::TargetPinned && !target_seen {
            target_seen = true;
            target.replace_partition_directory()?;
        }
        Ok(())
    });
    assert!(target_seen);
}

#[test]
fn partition_attribute_inode_and_content_races_fail_closed() {
    let inode = SyntheticSysfs::stable().unwrap();
    let mut pinned = false;
    assert_prepare_race(&inode, |checkpoint| {
        if checkpoint
            == (FixtureCheckpoint::AttributePinned {
                node: FixtureNode::Partition,
                attribute: FixtureAttribute::Dev,
            })
            && !pinned
        {
            pinned = true;
            inode.replace_regular(
                FixtureEntry::PartitionDevice,
                format!("{PARTITION_MAJOR}:{PARTITION_MINOR}\n").as_bytes(),
            )?;
        }
        Ok(())
    });
    assert!(pinned);

    let contents = SyntheticSysfs::stable().unwrap();
    let mut read = false;
    assert_prepare_race(&contents, |checkpoint| {
        if checkpoint
            == (FixtureCheckpoint::AttributeRead {
                node: FixtureNode::Partition,
                attribute: FixtureAttribute::Partition,
            })
            && !read
        {
            read = true;
            contents.overwrite_regular(FixtureEntry::PartitionNumber, b"8\n")?;
        }
        Ok(())
    });
    assert!(read);

    let geometry = SyntheticSysfs::stable().unwrap();
    let mut size_read = false;
    assert_prepare_race(&geometry, |checkpoint| {
        if checkpoint
            == (FixtureCheckpoint::AttributeRead {
                node: FixtureNode::Partition,
                attribute: FixtureAttribute::Size,
            })
            && !size_read
        {
            size_read = true;
            geometry.overwrite_regular(FixtureEntry::PartitionSize, b"1048575\n")?;
        }
        Ok(())
    });
    assert!(size_read);
}

#[test]
fn subsystem_ancestor_and_selected_parent_races_fail_closed() {
    let subsystem = SyntheticSysfs::stable().unwrap();
    let mut subsystem_seen = false;
    assert_prepare_race(&subsystem, |checkpoint| {
        if matches!(checkpoint, FixtureCheckpoint::SubsystemPinned { .. }) && !subsystem_seen {
            subsystem_seen = true;
            subsystem.replace_symlink(FixtureEntry::PartitionSubsystem, b"../../../class/block")?;
        }
        Ok(())
    });
    assert!(subsystem_seen);

    let parent_subsystem = SyntheticSysfs::stable().unwrap();
    let mut parent_subsystem_seen = false;
    assert_prepare_race(&parent_subsystem, |checkpoint| {
        if checkpoint == (FixtureCheckpoint::SubsystemPinned { depth: 2 }) && !parent_subsystem_seen {
            parent_subsystem_seen = true;
            parent_subsystem.replace_symlink(FixtureEntry::DiskSubsystem, b"../../../class/block")?;
        }
        Ok(())
    });
    assert!(parent_subsystem_seen);

    let ancestor = SyntheticSysfs::stable().unwrap();
    let mut ancestor_seen = false;
    assert_prepare_race(&ancestor, |checkpoint| {
        if matches!(checkpoint, FixtureCheckpoint::AncestorExamined { .. }) && !ancestor_seen {
            ancestor_seen = true;
            ancestor.replace_intermediate_directory()?;
        }
        Ok(())
    });
    assert!(ancestor_seen);

    let parent = SyntheticSysfs::stable().unwrap();
    let mut parent_seen = false;
    assert_prepare_race(&parent, |checkpoint| {
        if matches!(checkpoint, FixtureCheckpoint::ParentSelected { .. }) && !parent_seen {
            parent_seen = true;
            parent.replace_disk_directory()?;
        }
        Ok(())
    });
    assert!(parent_seen);
}

#[test]
fn parent_attribute_and_terminal_rebind_races_fail_closed() {
    let parent_attribute = SyntheticSysfs::stable().unwrap();
    let mut parent_read = false;
    assert_prepare_race(&parent_attribute, |checkpoint| {
        if checkpoint
            == (FixtureCheckpoint::AttributeRead {
                node: FixtureNode::Parent,
                attribute: FixtureAttribute::Uevent,
            })
            && !parent_read
        {
            parent_read = true;
            parent_attribute.overwrite_regular(
                FixtureEntry::DiskEvent,
                format!(
                    "MAJOR={DISK_MAJOR}\nMINOR={DISK_MINOR}\nDEVTYPE=disk\nDISKSEQ={}\n",
                    DISK_SEQUENCE - 1
                )
                .as_bytes(),
            )?;
        }
        Ok(())
    });
    assert!(parent_read);

    let parent_partition = SyntheticSysfs::stable().unwrap();
    let mut absence_raced = false;
    assert_prepare_race(&parent_partition, |checkpoint| {
        if checkpoint
            == (FixtureCheckpoint::AttributeRebound {
                node: FixtureNode::Parent,
                attribute: FixtureAttribute::Uevent,
            })
            && !absence_raced
        {
            absence_raced = true;
            fs::write(
                parent_partition.entry(FixtureEntry::DiskDirectory).join("partition"),
                b"9\n",
            )?;
        }
        Ok(())
    });
    assert!(absence_raced);

    let terminal = SyntheticSysfs::stable().unwrap();
    let mut terminal_seen = false;
    assert_prepare_race(&terminal, |checkpoint| {
        if checkpoint == FixtureCheckpoint::TerminalRebind && !terminal_seen {
            terminal_seen = true;
            terminal.replace_partition_directory()?;
        }
        Ok(())
    });
    assert!(terminal_seen);

    let final_root = SyntheticSysfs::stable().unwrap();
    let mut final_name_seen = false;
    assert_prepare_race(&final_root, |checkpoint| {
        if checkpoint == FixtureCheckpoint::FinalNameRebind && !final_name_seen {
            final_name_seen = true;
            final_root.replace_root_directory()?;
        }
        Ok(())
    });
    assert!(final_name_seen);
}

#[test]
fn revalidation_repeats_the_complete_race_resistant_capture() {
    let fixture = SyntheticSysfs::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare(device()).unwrap();
    let mut count = 0usize;
    let result = prepared.revalidate_with(
        FixtureSysfsIdentityLimits::default(),
        deadline(),
        &mut |checkpoint| {
            if checkpoint
                == (FixtureCheckpoint::AttributeRebound {
                    node: FixtureNode::Partition,
                    attribute: FixtureAttribute::Uevent,
                })
            {
                count += 1;
                if count == 1 {
                    fixture.replace_regular(
                        FixtureEntry::PartitionEvent,
                        format!(
                            "MAJOR={PARTITION_MAJOR}\nMINOR={PARTITION_MINOR}\nDEVTYPE=partition\nPARTN={PARTITION_NUMBER}\nPARTUUID=0e85a94f-b115-41c5-9d72-9d23958b5edc\nDISKSEQ={DISK_SEQUENCE}\n"
                        )
                        .as_bytes(),
                    )?;
                }
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(count, 1);
    fixture.assert_outside_unchanged();
}
