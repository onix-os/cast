use std::{ffi::CString, io, os::unix::ffi::OsStrExt as _, path::Path};

use super::*;

#[test]
fn startup_activation_inventory_rejects_live_usr_extended_attributes() {
    let fixture = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let live = fixture.installation.root.join("usr");
    if !set_user_xattr(&live).unwrap() {
        return;
    }

    assert_xattr_rejection(fixture.snapshot());
}

#[test]
fn startup_activation_inventory_rejects_staged_usr_extended_attributes() {
    let fixture = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let staged = fixture.installation.staging_path("usr");
    if !set_user_xattr(&staged).unwrap() {
        return;
    }

    assert_xattr_rejection(fixture.snapshot());
}

#[test]
fn startup_activation_retained_revalidation_rejects_new_usr_extended_attributes() {
    let fixture = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let snapshot = fixture.snapshot().unwrap();
    let live = fixture.installation.root.join("usr");
    if !set_user_xattr(&live).unwrap() {
        return;
    }

    assert_retained_xattr_rejection(snapshot.revalidate_retained());
}

#[test]
fn startup_activation_retained_revalidation_rejects_new_staged_usr_extended_attributes() {
    let fixture = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let snapshot = fixture.snapshot().unwrap();
    let staged = fixture.installation.staging_path("usr");
    if !set_user_xattr(&staged).unwrap() {
        return;
    }

    assert_retained_xattr_rejection(snapshot.revalidate_retained());
}

fn assert_xattr_rejection<T>(result: Result<T, CaptureError>) {
    assert!(matches!(
        result,
        Err(CaptureError::Io {
            operation: "reject extended attributes on retained /usr tree",
            ..
        })
    ));
}

fn assert_retained_xattr_rejection<T>(result: Result<T, CaptureError>) {
    assert!(matches!(
        result,
        Err(CaptureError::InodeChanged { .. })
            | Err(CaptureError::Io {
                operation: "reject extended attributes on retained /usr tree",
                ..
            })
    ));
}

pub(super) fn set_user_xattr(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"noncanonical";
    // SAFETY: both C strings and the value remain live for this call.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.forge-activation-test".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let source = io::Error::last_os_error();
    if source
        .raw_os_error()
        .is_some_and(|code| matches!(code, nix::libc::EOPNOTSUPP | nix::libc::EPERM))
    {
        Ok(false)
    } else {
        Err(source)
    }
}
