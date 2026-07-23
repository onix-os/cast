use std::{
    cell::Cell,
    io,
    os::fd::AsRawFd as _,
    time::{Duration, Instant},
};

use super::super::{
    MAX_INTERRUPTED_SYSCALL_RETRIES, require_no_xattrs, require_no_xattrs_until, require_no_xattrs_with_probe,
};

#[test]
fn no_xattr_probe_classifies_empty_positive_unsupported_and_indeterminate_results() {
    let path = std::path::Path::new("/diagnostic/usr");

    require_no_xattrs_with_probe(path, None, || Ok(0)).unwrap();

    let populated = require_no_xattrs_with_probe(path, None, || Ok(17)).unwrap_err();
    assert_eq!(populated.kind(), io::ErrorKind::PermissionDenied);
    assert!(populated.to_string().contains("17 xattr-name bytes"));

    require_no_xattrs_with_probe(path, None, || Err(io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP))).unwrap();

    let indeterminate =
        require_no_xattrs_with_probe(path, None, || Err(io::Error::from_raw_os_error(nix::libc::EIO))).unwrap_err();
    assert_eq!(indeterminate.raw_os_error(), Some(nix::libc::EIO));
}

#[test]
fn no_xattr_probe_bounds_interrupted_retries_and_obeys_its_deadline() {
    let path = std::path::Path::new("/diagnostic/usr");
    let accepted_attempts = Cell::new(0usize);
    require_no_xattrs_with_probe(path, None, || {
        let attempt = accepted_attempts.get();
        accepted_attempts.set(attempt + 1);
        if attempt < MAX_INTERRUPTED_SYSCALL_RETRIES {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(0)
        }
    })
    .unwrap();
    assert_eq!(accepted_attempts.get(), MAX_INTERRUPTED_SYSCALL_RETRIES + 1);

    let rejected_attempts = Cell::new(0usize);
    let retry_error = require_no_xattrs_with_probe(path, None, || {
        rejected_attempts.set(rejected_attempts.get() + 1);
        Err(io::Error::from(io::ErrorKind::Interrupted))
    })
    .unwrap_err();
    assert_eq!(retry_error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(rejected_attempts.get(), MAX_INTERRUPTED_SYSCALL_RETRIES + 1);

    let deadline_attempts = Cell::new(0usize);
    let deadline_error = require_no_xattrs_with_probe(path, Some(Instant::now() - Duration::from_millis(1)), || {
        deadline_attempts.set(deadline_attempts.get() + 1);
        Ok(0)
    })
    .unwrap_err();
    assert_eq!(deadline_error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(deadline_attempts.get(), 0);
}

#[test]
fn retained_no_xattr_probe_rejects_a_real_user_xattr_when_supported() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();
    require_no_xattrs(&directory, temporary.path()).unwrap();
    require_no_xattrs_until(&directory, temporary.path(), Instant::now() + Duration::from_secs(1)).unwrap();

    let value = b"noncanonical";
    // SAFETY: the descriptor, static name, and value remain live for this call.
    let result = unsafe {
        nix::libc::fsetxattr(
            directory.as_raw_fd(),
            c"user.forge-no-xattr-test".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result != 0 {
        let source = io::Error::last_os_error();
        if source
            .raw_os_error()
            .is_some_and(|code| matches!(code, nix::libc::EOPNOTSUPP | nix::libc::EPERM))
        {
            return;
        }
        panic!("set real user xattr: {source}");
    }

    let error = require_no_xattrs(&directory, temporary.path()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
