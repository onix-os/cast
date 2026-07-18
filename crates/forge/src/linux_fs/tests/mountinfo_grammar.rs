use std::io;

use super::super::mountinfo::parse_mountinfo_bytes;

const MINIMAL: &[u8] = b"1 1 0:1 / / rw - rootfs rootfs rw\n";

fn invalid(input: &[u8]) {
    assert_eq!(
        parse_mountinfo_bytes(input).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn complete_records_preserve_order_fields_and_exact_decoded_path_bytes() {
    let mut input = b"36 35 98:0 /root\\040dir /mnt\\011point rw,nosuid shared:7 master:1 - ext4 /dev/disk\\134name rw,errors=remount-ro\n"
        .to_vec();
    input.extend_from_slice(b"37 36 0:42 /");
    input.push(0xff);
    input.extend_from_slice(b" /run/");
    input.push(0xfe);
    input.extend_from_slice(b" ro,nodev unbindable - tmpfs none ro,size=64k\n");

    let parsed = parse_mountinfo_bytes(&input).unwrap();
    assert_eq!(parsed.entries().len(), 2);

    let first = &parsed.entries()[0];
    assert_eq!(first.mount_id(), 36);
    assert_eq!(first.parent_id(), 35);
    assert_eq!(first.device().major(), 98);
    assert_eq!(first.device().minor(), 0);
    assert_eq!(first.root(), b"/root dir");
    assert_eq!(first.mount_point(), b"/mnt\tpoint");
    assert_eq!(first.mount_options().collect::<Vec<_>>(), [b"rw".as_slice(), b"nosuid"]);
    assert_eq!(
        first.optional_fields().collect::<Vec<_>>(),
        [b"shared:7".as_slice(), b"master:1"]
    );
    assert_eq!(first.filesystem_type(), b"ext4");
    assert_eq!(first.mount_source(), b"/dev/disk\\name");
    assert_eq!(
        first.super_options().collect::<Vec<_>>(),
        [b"rw".as_slice(), b"errors=remount-ro"]
    );

    let second = &parsed.entries()[1];
    assert_eq!(second.root(), b"/\xff");
    assert_eq!(second.mount_point(), b"/run/\xfe");
    assert_eq!(second.optional_fields().collect::<Vec<_>>(), [b"unbindable".as_slice()]);
    assert_eq!(second.mount_source(), b"none");
}

#[test]
fn all_four_kernel_path_escapes_decode_without_interpreting_utf8() {
    let parsed =
        parse_mountinfo_bytes(b"1 1 0:1 /a\\040b\\011c\\012d\\134e /m\\040n rw - ext4 src\\040x rw\n").unwrap();
    let entry = &parsed.entries()[0];
    assert_eq!(entry.root(), b"/a b\tc\nd\\e");
    assert_eq!(entry.mount_point(), b"/m n");
    assert_eq!(entry.mount_source(), b"src x");
}

#[test]
fn mangle_fields_decode_hash_escape_while_paths_keep_four_escape_grammar() {
    let parsed = parse_mountinfo_bytes(b"1 1 0:1 /literal#root / rw - fuse\\043demo src\\043name rw\n").unwrap();
    let entry = &parsed.entries()[0];
    assert_eq!(entry.root(), b"/literal#root");
    assert_eq!(entry.filesystem_type(), b"fuse#demo");
    assert_eq!(entry.mount_source(), b"src#name");

    invalid(b"1 1 0:1 /bad\\043path / rw - ext4 none rw\n");
    invalid(b"1 1 0:1 / / rw - fuse\\044demo none rw\n");
    invalid(b"1 1 0:1 / / rw - ext4 src\\044name rw\n");
}

#[test]
fn malformed_or_unknown_path_escapes_are_never_reinterpreted() {
    for escape in [
        b"\\".as_slice(),
        b"\\0",
        b"\\04",
        b"\\040".get(..3).unwrap(),
        b"\\041",
        b"\\043",
        b"\\400",
        b"\\777",
        b"\\abc",
    ] {
        let mut input = b"1 1 0:1 /bad".to_vec();
        input.extend_from_slice(escape);
        input.extend_from_slice(b" / rw - ext4 none rw\n");
        invalid(&input);
    }

    invalid(b"1 1 0:1 / / rw - ext4 bad\\041source rw\n");
}

#[test]
fn canonical_numeric_fields_reject_zero_ids_leading_zeroes_signs_and_overflow() {
    for input in [
        b"0 1 0:1 / / rw - rootfs rootfs rw\n".as_slice(),
        b"1 0 0:1 / / rw - rootfs rootfs rw\n",
        b"01 1 0:1 / / rw - rootfs rootfs rw\n",
        b"+1 1 0:1 / / rw - rootfs rootfs rw\n",
        b"18446744073709551616 1 0:1 / / rw - rootfs rootfs rw\n",
        b"1 1 00:1 / / rw - rootfs rootfs rw\n",
        b"1 1 0:01 / / rw - rootfs rootfs rw\n",
        b"1 1 4294967296:1 / / rw - rootfs rootfs rw\n",
        b"1 1 0:4294967296 / / rw - rootfs rootfs rw\n",
        b"1 1 0 / / rw - rootfs rootfs rw\n",
        b"1 1 0:1:2 / / rw - rootfs rootfs rw\n",
    ] {
        invalid(input);
    }
}

#[test]
fn duplicate_mount_ids_are_rejected_without_reordering_records() {
    invalid(b"1 1 0:1 / / rw - rootfs rootfs rw\n1 1 0:2 / / rw - tmpfs none rw\n");

    let parsed =
        parse_mountinfo_bytes(b"2 1 0:2 / /two rw - tmpfs none rw\n1 1 0:1 / /one rw - rootfs rootfs rw\n").unwrap();
    assert_eq!(parsed.entries()[0].mount_id(), 2);
    assert_eq!(parsed.entries()[1].mount_id(), 1);
}

#[test]
fn separator_and_space_grammar_rejects_ambiguous_or_incomplete_lines() {
    for input in [
        b" 1 1 0:1 / / rw - rootfs rootfs rw\n".as_slice(),
        b"1  1 0:1 / / rw - rootfs rootfs rw\n",
        b"1 1 0:1 / / rw - rootfs rootfs rw \n",
        b"1 1 0:1 / / rw rootfs rootfs rw\n",
        b"1 1 0:1 / / rw - - rootfs rootfs rw\n",
        b"1 1 0:1 / / rw - rootfs rootfs rw extra\n",
        b"1 1 0:1 / / rw - rootfs rootfs\n",
        b"1\t1 0:1 / / rw - rootfs rootfs rw\n",
        b"1 1 0:1 relative / rw - rootfs rootfs rw\n",
        b"1 1 0:1 / relative rw - rootfs rootfs rw\n",
        b"1 1 0:1 / / rw,,nodev - rootfs rootfs rw\n",
        b"1 1 0:1 / / rw, - rootfs rootfs rw\n",
        b"1 1 0:1 / / rw - rootfs rootfs rw,\n",
        b"1 1 0:1 / / rw - rootfs root\0fs rw\n",
    ] {
        invalid(input);
    }
}

#[test]
fn opaque_options_keep_filesystem_specific_escapes_and_duplicates() {
    let parsed =
        parse_mountinfo_bytes(b"1 1 0:1 / / rw,rw,opt=one\\054two tag:future - fuse.test source rw,key=a\\075b,rw\n")
            .unwrap();
    let entry = &parsed.entries()[0];
    assert_eq!(
        entry.mount_options().collect::<Vec<_>>(),
        [b"rw".as_slice(), b"rw", b"opt=one\\054two"]
    );
    assert_eq!(
        entry.super_options().collect::<Vec<_>>(),
        [b"rw".as_slice(), b"key=a\\075b", b"rw"]
    );
}

#[test]
fn empty_missing_newline_and_blank_records_are_truncation_or_invalid_data() {
    assert_eq!(
        parse_mountinfo_bytes(b"").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        parse_mountinfo_bytes(&MINIMAL[..MINIMAL.len() - 1]).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    invalid(b"\n");
    invalid(b"1 1 0:1 / / rw - rootfs rootfs rw\n\n");
}
