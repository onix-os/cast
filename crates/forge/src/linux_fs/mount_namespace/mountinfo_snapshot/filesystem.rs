use std::{io, mem::zeroed, os::fd::AsRawFd as _};

use super::super::filesystem::Operation;
use crate::linux_fs::{PROC_SUPER_MAGIC, controlled_resolution, openat2_file_until, retry_interrupted};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MountInfoFileWitness {
    device: u64,
    inode: u64,
    kind: u32,
}

pub(super) fn open_mountinfo(
    thread: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<(std::fs::File, MountInfoFileWitness)> {
    if !operation.is_production() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture operation cannot enter the production mountinfo file reader",
        ));
    }
    operation.charge_descriptor("opening fixed current-thread mountinfo")?;
    let file = openat2_file_until(
        thread.as_raw_fd(),
        c"mountinfo",
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        operation.deadline(),
    )?;
    let witness = authenticate_mountinfo_file(&file, operation)?;
    Ok((file, witness))
}

pub(super) fn authenticate_mountinfo_file(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
) -> io::Result<MountInfoFileWitness> {
    let before = raw_witness(file, operation, "capturing mountinfo file identity")?;
    let before_magic = filesystem_magic(file, operation, "authenticating mountinfo procfs before identity close")?;
    let after_magic = filesystem_magic(file, operation, "revalidating mountinfo procfs after identity close")?;
    let after = raw_witness(file, operation, "closing mountinfo file identity sandwich")?;
    validate_mountinfo_file_authentication(before_magic, after_magic, before, after)?;
    Ok(before)
}

pub(super) fn require_same_mountinfo_file(
    expected: MountInfoFileWitness,
    actual: MountInfoFileWitness,
    context: &'static str,
) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed mountinfo device, inode, or regular-file kind"),
        ))
    }
}

fn raw_witness(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<MountInfoFileWitness> {
    operation.charge(1, action)?;
    // SAFETY: zeroed stat storage is a valid fstat output buffer and `file`
    // remains retained for the bounded syscall.
    let mut status: nix::libc::stat = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: status is writable and file remains a live descriptor.
        if unsafe { nix::libc::fstat(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    Ok(MountInfoFileWitness {
        device: status.st_dev,
        inode: status.st_ino,
        kind: status.st_mode & nix::libc::S_IFMT,
    })
}

fn filesystem_magic(
    file: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<nix::libc::c_long> {
    operation.charge(1, action)?;
    // SAFETY: zeroed statfs storage is a valid fstatfs output buffer and
    // `file` remains retained for the bounded syscall.
    let mut status: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: status is writable and file remains a live descriptor.
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    operation.checkpoint()?;
    Ok(status.f_type)
}

fn validate_mountinfo_file_authentication(
    before_magic: nix::libc::c_long,
    after_magic: nix::libc::c_long,
    before: MountInfoFileWitness,
    after: MountInfoFileWitness,
) -> io::Result<()> {
    if before_magic != PROC_SUPER_MAGIC || after_magic != PROC_SUPER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "mountinfo descriptor is not stably procfs: expected {PROC_SUPER_MAGIC:#x}, found {before_magic:#x} then {after_magic:#x}"
            ),
        ));
    }
    if before.device == 0 || before.inode == 0 || after.device == 0 || after.inode == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mountinfo descriptor has a zero device or inode identity",
        ));
    }
    if before.kind != nix::libc::S_IFREG || after.kind != nix::libc::S_IFREG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "mountinfo descriptor is not a stable regular file: found {:#o} then {:#o}",
                before.kind, after.kind
            ),
        ));
    }
    require_same_mountinfo_file(before, after, "mountinfo authentication sandwich")
}

#[cfg(test)]
pub(crate) fn validate_fixture_mountinfo_file_authentication(
    before_magic: nix::libc::c_long,
    after_magic: nix::libc::c_long,
    before_device: u64,
    before_inode: u64,
    before_kind: u32,
    after_device: u64,
    after_inode: u64,
    after_kind: u32,
) -> io::Result<()> {
    validate_mountinfo_file_authentication(
        before_magic,
        after_magic,
        MountInfoFileWitness {
            device: before_device,
            inode: before_inode,
            kind: before_kind,
        },
        MountInfoFileWitness {
            device: after_device,
            inode: after_inode,
            kind: after_kind,
        },
    )
}
