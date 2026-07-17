//! Shared fixtures and security-boundary helpers for target normalization.

use std::{
    ffi::{CStr, CString},
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
            new_state_candidate_preserve_move_attempt_count, new_state_target_normalize_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count, reset_new_state_target_normalize_attempt_count,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::super::UsrRollbackNewStateCandidatePreserveNormalizeTargetLease;
use super::super::{
    fixture::OperationKind,
    support::{CandidatePreserveFixture, CandidateSource, transition_quarantine_path},
};

pub(super) const RESTRICTIVE_RESIDUE_MODES: [u32; 7] = [0o000, 0o100, 0o200, 0o300, 0o400, 0o500, 0o600];

pub(super) fn residue_fixture(
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    mode: u32,
) -> CandidatePreserveFixture {
    CandidatePreserveFixture::new_state_target_residue(source, usr_outcome, mode)
}

pub(super) fn normal_fixture(mode: u32) -> CandidatePreserveFixture {
    residue_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied, mode)
}

pub(super) fn normalize_target_lease<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'reservation> {
    assert_eq!(fixture.fixture.kind, OperationKind::NewState);
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact restrictive NewState target did not admit Apply authority");
    };
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease) =
        authority.into_effect_selection(&seal, journal).unwrap()
    else {
        panic!("exact restrictive NewState target did not select its normalize lease");
    };
    lease
}

pub(super) fn target_path(fixture: &CandidatePreserveFixture) -> PathBuf {
    transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent)
}

pub(super) fn reset_effect_attempts() {
    reset_new_state_target_normalize_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
}

pub(super) fn assert_effect_attempts(fixture: &CandidatePreserveFixture, expected_normalize: usize) {
    assert_eq!(new_state_target_normalize_attempt_count(), expected_normalize);
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TargetIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) owner: u32,
    pub(super) group: u32,
}

pub(super) fn target_identity(path: &Path) -> TargetIdentity {
    let metadata = fs::symlink_metadata(path).unwrap();
    TargetIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        owner: metadata.uid(),
        group: metadata.gid(),
    }
}

pub(super) fn install_acl_or_payload(path: &Path, name: &CStr) -> bool {
    if install_test_posix_acl(path, name) {
        true
    } else {
        fs::write(path.join("acl-unavailable-payload"), b"foreign").unwrap();
        false
    }
}

fn install_test_posix_acl(path: &Path, name: &CStr) -> bool {
    const ACL_UNDEFINED_ID: u32 = u32::MAX;
    // A named-user entry keeps this ACL distinct from ordinary mode bits.
    // SAFETY: geteuid has no arguments and cannot fail.
    let named_user = unsafe { nix::libc::geteuid() };
    let entries = [
        (0x01_u16, 0o7_u16, ACL_UNDEFINED_ID),
        (0x02, 0o4, named_user),
        (0x04, 0o5, ACL_UNDEFINED_ID),
        (0x10, 0o5, ACL_UNDEFINED_ID),
        (0x20, 0o5, ACL_UNDEFINED_ID),
    ];
    let mut value = Vec::with_capacity(4 + entries.len() * 8);
    value.extend_from_slice(&2_u32.to_le_bytes());
    for (tag, permissions, id) in entries {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&permissions.to_le_bytes());
        value.extend_from_slice(&id.to_le_bytes());
    }
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: both C strings and the complete ACL value remain live.
    if unsafe { nix::libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) } == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
    ) {
        eprintln!("skipping POSIX ACL assertion for {}: {error}", path.to_string_lossy());
        false
    } else {
        panic!("install target-normalization test ACL: {error}");
    }
}

pub(super) fn install_user_xattr(path: &Path) -> bool {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"diagnostic-only";
    // SAFETY: the path, static name, and complete value remain live.
    if unsafe {
        nix::libc::setxattr(
            path.as_ptr(),
            c"user.cast.target-normalize-boundary".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    } == 0
    {
        return true;
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
    ) {
        false
    } else {
        panic!("install target-normalization test user xattr: {error}");
    }
}
