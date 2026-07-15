//! Linux 5.6-compatible operations on retained filesystem capabilities.
//!
//! `O_PATH` descriptors deliberately cannot be passed to `fchmod(2)`, and
//! `fchmodat2(2)` did not exist in Linux 5.6.  The only path resolved here is
//! the live decimal descriptor name below an authenticated procfs instance.

use std::{
    ffi::{CStr, CString},
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
    time::Instant,
};

const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;
const POSIX_ACCESS_ACL_XATTR: &CStr = c"system.posix_acl_access";
const POSIX_DEFAULT_ACL_XATTR: &CStr = c"system.posix_acl_default";
const MAX_DECIMAL_PID_BYTES: usize = 16;
const MAX_THREAD_SELF_BYTES: usize = MAX_DECIMAL_PID_BYTES * 2 + 6;
// Retrying EINTR forever would turn every higher-level timeout into a best-
// effort hint.  Linux syscalls normally make progress immediately after a
// signal, so this generous ceiling is a fail-closed backstop even for callers
// which do not supply an operation deadline.
const MAX_INTERRUPTED_SYSCALL_RETRIES: usize = 1_024;

fn retry_interrupted<T>(deadline: Option<Instant>, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut interruptions = 0usize;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() > deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "retained filesystem operation exceeded its deadline",
            ));
        }
        match operation() {
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {
                if interruptions >= MAX_INTERRUPTED_SYSCALL_RETRIES {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        format!(
                            "retained filesystem operation exceeded {MAX_INTERRUPTED_SYSCALL_RETRIES} interrupted retries"
                        ),
                    ));
                }
                interruptions += 1;
            }
            result => return result,
        }
    }
}

include!("linux_fs/descriptor_metadata.rs");

include!("linux_fs/namespace_operations.rs");

include!("linux_fs/directory_security.rs");

include!("linux_fs/descriptor_access.rs");

#[cfg(test)]
mod tests;
