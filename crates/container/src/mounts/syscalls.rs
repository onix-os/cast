use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs_err::{self as fs};
use nc::syscalls::syscall5;
use nc::{
    AT_EMPTY_PATH, AT_FDCWD, MOUNT_ATTR_RDONLY, MOVE_MOUNT_F_EMPTY_PATH, OPEN_TREE_CLOEXEC, OPEN_TREE_CLONE,
    SYS_MOUNT_SETATTR, mount_attr_t, move_mount, open_tree,
};
use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::unistd::close;
use snafu::ResultExt as _;

use crate::{ContainerError, FsErrSnafu, MountSnafu, SetCurrentDirSnafu, SetupLocalhostSnafu};

pub(crate) fn errno_to_io(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

pub(crate) fn openat_anchored(
    parent: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    flags: nix::libc::c_int,
    mode: nix::libc::mode_t,
) -> io::Result<OwnedFd> {
    // SAFETY: parent and name remain live for the call and a successful openat
    // returns a fresh descriptor transferred exactly once to OwnedFd.
    let descriptor = unsafe { nix::libc::openat(parent, name.as_ptr(), flags, mode) };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

pub(crate) fn setup_localhost() -> Result<(), ContainerError> {
    // TODO: maybe it's better to hunt down the API to do this instead?
    if PathBuf::from("/usr/sbin/ip").exists() {
        Command::new("/usr/sbin/ip")
            .args(["link", "set", "lo", "up"])
            .output()
            .context(SetupLocalhostSnafu)?;
    }
    Ok(())
}

pub(crate) fn ensure_directory(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    if !path.exists() {
        fs::create_dir_all(path).context(FsErrSnafu)?;
    }
    Ok(())
}

pub(crate) fn ensure_empty_file(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    if !path.exists() {
        fs::File::create_new(path).context(FsErrSnafu)?;
    }
    Ok(())
}

pub(crate) fn prepare_bind_target(source: &Path, target: &Path) -> Result<(), ContainerError> {
    let metadata = fs::metadata(source).context(FsErrSnafu)?;
    if metadata.is_dir() {
        ensure_directory(target)?;
    } else {
        if let Some(parent) = target.parent() {
            ensure_directory(parent)?;
        }

        ensure_empty_file(target)?;
    }
    Ok(())
}

pub(crate) fn bind_mount(source: &Path, target: &Path, read_only: bool) -> Result<(), ContainerError> {
    prepare_bind_target(source, target)?;

    unsafe {
        let inner = || {
            // Bind mount to fd
            let fd = open_tree(AT_FDCWD, source, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC).map_err(Errno::from_i32)?;

            let result = (|| {
                // Set rd flag if applicable
                if read_only {
                    let attr = mount_attr_t {
                        attr_set: MOUNT_ATTR_RDONLY as u64,
                        attr_clr: 0,
                        program: 0,
                        userns_fd: 0,
                    };
                    syscall5(
                        SYS_MOUNT_SETATTR,
                        fd as usize,
                        c"".as_ptr() as usize,
                        AT_EMPTY_PATH as usize,
                        &attr as *const mount_attr_t as usize,
                        size_of::<mount_attr_t>(),
                    )
                    .map_err(Errno::from_i32)?;
                }

                // Move detached mount to target
                move_mount(fd, Path::new(""), AT_FDCWD, target, MOVE_MOUNT_F_EMPTY_PATH).map_err(Errno::from_i32)
            })();
            let close_result = close(fd);

            result?;
            close_result?;
            Ok(())
        };

        inner().context(MountSnafu {
            target: target.to_owned(),
        })
    }
}

pub(crate) fn add_mount<T: AsRef<Path>>(
    source: Option<T>,
    target: T,
    fs_type: Option<&str>,
    flags: MsFlags,
) -> Result<(), ContainerError> {
    add_mount_with_data(source, target, fs_type, flags, None)
}

pub(crate) fn add_mount_with_data<T: AsRef<Path>>(
    source: Option<T>,
    target: T,
    fs_type: Option<&str>,
    flags: MsFlags,
    data: Option<&str>,
) -> Result<(), ContainerError> {
    let target = target.as_ref();
    ensure_directory(target)?;
    mount(source.as_ref().map(AsRef::as_ref), target, fs_type, flags, data).context(MountSnafu {
        target: target.to_owned(),
    })?;
    Ok(())
}

pub(crate) fn set_current_dir(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    std::env::set_current_dir(path).with_context(|_| SetCurrentDirSnafu { path: path.to_owned() })
}
