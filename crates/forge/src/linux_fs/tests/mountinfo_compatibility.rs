use std::io::Cursor;

use super::super::mountinfo::read_mountinfo_bounded;

const SYNTHETIC_KERNEL_FORMAT_SNAPSHOT: &[u8] =
    b"101 1 0:99 / / rw,relatime shared:1 - overlay overlay rw,lowerdir=/immutable\n\
102 101 0:42 / /run rw,nosuid,nodev shared:2 master:1 - tmpfs tmpfs rw,size=65536k,mode=755\n\
103 101 4294967295:4294967294 / /firmware\\040volume rw,relatime - vfat synthetic-partition rw,fmask=0077,dmask=0077\n\
104 101 0:5 /subtree /bind\\011target ro,nosuid,nodev unbindable - ext4 synthetic\\134source ro\n";

const OBSERVED_NSFS_MOUNTINFO_RECORD: &[u8] =
    b"2561 254 0:5 mnt:[4026532758] /run/snapd/ns/snapd-desktop-integration.mnt rw - nsfs nsfs rw\n";

#[test]
fn synthetic_kernel_format_snapshot_is_parser_compatible_without_host_topology() {
    let mut snapshot = Cursor::new(SYNTHETIC_KERNEL_FORMAT_SNAPSHOT);
    let parsed = read_mountinfo_bounded(&mut snapshot).unwrap();

    assert_eq!(parsed.entries().len(), 4);
    assert_eq!(parsed.entries()[0].mount_point(), b"/");
    assert_eq!(
        parsed.entries()[1].optional_fields().collect::<Vec<_>>(),
        [b"shared:2", b"master:1"]
    );
    assert_eq!(parsed.entries()[2].device().major(), u32::MAX);
    assert_eq!(parsed.entries()[2].device().minor(), u32::MAX - 1);
    assert_eq!(parsed.entries()[2].mount_point(), b"/firmware volume");
    assert_eq!(parsed.entries()[3].root(), b"/subtree");
    assert_eq!(parsed.entries()[3].mount_point(), b"/bind\ttarget");
    assert_eq!(parsed.entries()[3].mount_source(), b"synthetic\\source");
}

#[test]
fn observed_nsfs_mount_root_is_preserved_as_an_opaque_field() {
    let mut snapshot = Cursor::new(OBSERVED_NSFS_MOUNTINFO_RECORD);
    let parsed = read_mountinfo_bounded(&mut snapshot).unwrap();

    assert_eq!(parsed.entries().len(), 1);
    let entry = &parsed.entries()[0];
    assert_eq!(entry.mount_id(), 2561);
    assert_eq!(entry.parent_id(), 254);
    assert_eq!(entry.device().major(), 0);
    assert_eq!(entry.device().minor(), 5);
    assert_eq!(entry.root(), b"mnt:[4026532758]");
    assert_eq!(entry.mount_point(), b"/run/snapd/ns/snapd-desktop-integration.mnt");
    assert_eq!(entry.filesystem_type(), b"nsfs");
    assert_eq!(entry.mount_source(), b"nsfs");
}
