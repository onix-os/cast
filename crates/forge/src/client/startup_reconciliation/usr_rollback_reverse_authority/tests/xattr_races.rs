use std::{ffi::CString, fs, io, os::unix::ffi::OsStrExt as _, path::Path};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackReverseAdmission, arm_before_usr_rollback_reverse_fresh_namespace_capture,
        },
        startup_recovery::UsrRollbackReverseEffectSeal,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
};

use super::{
    fixture::OperationKind,
    support::{ReverseFixture, ReverseLayout},
};

#[test]
fn startup_usr_rollback_reverse_apply_handoff_rejects_fresh_usr_xattr_race() {
    require_handoff_xattr_race_rejected(ReverseLayout::Post);
}

#[test]
fn startup_usr_rollback_reverse_finish_handoff_rejects_fresh_usr_xattr_race() {
    require_handoff_xattr_race_rejected(ReverseLayout::Pre);
}

fn require_handoff_xattr_race_rejected(layout: ReverseLayout) {
    let fixture = ReverseFixture::new(OperationKind::Archived, layout);
    if !user_xattrs_supported(fixture.fixture._temporary.path()).unwrap() {
        return;
    }
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let admission = fixture.capture(&journal, &reservation);
    let live_usr = fixture.fixture.installation.root.join("usr");
    arm_before_usr_rollback_reverse_fresh_namespace_capture(move || {
        assert!(set_user_xattr(&live_usr).unwrap());
    });
    reset_retained_exchange_syscall_count();
    let effect_seal = UsrRollbackReverseEffectSeal::new_for_test();

    let rejected = match admission {
        UsrRollbackReverseAdmission::Apply(authority) => {
            assert_eq!(layout, ReverseLayout::Post);
            authority.into_effect_lease(&effect_seal, &journal).is_err()
        }
        UsrRollbackReverseAdmission::Finish(authority) => {
            assert_eq!(layout, ReverseLayout::Pre);
            authority.into_effect_lease(&effect_seal, &journal).is_err()
        }
        _ => panic!("exact {layout:?} evidence was not admitted"),
    };
    assert!(rejected, "fresh /usr xattr was admitted for {layout:?}");
    assert_eq!(retained_exchange_syscall_count(), 0);
}

fn user_xattrs_supported(root: &Path) -> io::Result<bool> {
    let probe = root.join(".rollback-reverse-xattr-support-probe");
    fs::create_dir(&probe)?;
    let supported = set_user_xattr(&probe)?;
    fs::remove_dir(probe)?;
    Ok(supported)
}

fn set_user_xattr(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"noncanonical";
    // SAFETY: both C strings and the value remain live for this call.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.forge-rollback-reverse-test".as_ptr(),
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
