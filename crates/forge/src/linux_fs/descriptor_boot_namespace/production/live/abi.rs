//! Compile-time checks for the native Linux `linux_dirent64` record prefix.

use std::mem::{offset_of, size_of};

use super::super::model::RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES;

#[cfg(not(target_os = "linux"))]
compile_error!("the retained raw-directory adapter requires Linux getdents64");

#[allow(dead_code)]
#[repr(C)]
struct NativeLinuxDirent64Prefix {
    inode: u64,
    next_offset: i64,
    record_length: u16,
    node_type_hint: u8,
    name: [u8; 0],
}

const _: () = {
    assert!(size_of::<u64>() == 8);
    assert!(size_of::<i64>() == 8);
    assert!(size_of::<u16>() == 2);
    assert!(size_of::<u8>() == 1);
    assert!(offset_of!(NativeLinuxDirent64Prefix, inode) == 0);
    assert!(offset_of!(NativeLinuxDirent64Prefix, next_offset) == 8);
    assert!(offset_of!(NativeLinuxDirent64Prefix, record_length) == 16);
    assert!(offset_of!(NativeLinuxDirent64Prefix, node_type_hint) == 18);
    assert!(offset_of!(NativeLinuxDirent64Prefix, name) == 19);
    assert!(size_of::<nix::libc::c_long>() == size_of::<usize>());
    assert!(RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES == size_of::<nix::libc::c_long>());
};
