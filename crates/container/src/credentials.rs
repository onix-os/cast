use std::fmt;
use std::ptr;

use nix::errno::Errno;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSyscallError {
    Kernel(Errno),
    UnexpectedReturn(nix::libc::c_long),
}

impl fmt::Display for CredentialSyscallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(_) => formatter.write_str("kernel rejected credential syscall"),
            Self::UnexpectedReturn(result) => {
                write!(formatter, "credential syscall returned unexpected value {result}")
            }
        }
    }
}

impl std::error::Error for CredentialSyscallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Kernel(source) => Some(source),
            Self::UnexpectedReturn(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct IdentityCredentials {
    pub(super) real_uid: u32,
    pub(super) effective_uid: u32,
    pub(super) saved_uid: u32,
    pub(super) filesystem_uid: u32,
    pub(super) real_gid: u32,
    pub(super) effective_gid: u32,
    pub(super) saved_gid: u32,
    pub(super) filesystem_gid: u32,
}

impl IdentityCredentials {
    pub(super) fn uniform_ids(&self) -> Option<(u32, u32)> {
        let uid = self.real_uid;
        let gid = self.real_gid;
        (self.effective_uid == uid
            && self.saved_uid == uid
            && self.filesystem_uid == uid
            && self.effective_gid == gid
            && self.saved_gid == gid
            && self.filesystem_gid == gid)
            .then_some((uid, gid))
    }
}

impl fmt::Display for IdentityCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "uid {}/{}/{}/{}, gid {}/{}/{}/{}",
            self.real_uid,
            self.effective_uid,
            self.saved_uid,
            self.filesystem_uid,
            self.real_gid,
            self.effective_gid,
            self.saved_gid,
            self.filesystem_gid,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReadIdentityFailure {
    ReadGroupCredentials(CredentialSyscallError),
    ReadUserCredentials(CredentialSyscallError),
}

/// The complete credential state that crosses into untrusted container code.
///
/// Linux keeps saved-set and filesystem IDs in addition to the commonly
/// inspected real and effective IDs.  All of them are part of the privilege
/// boundary: a nonzero saved-set ID can later be restored, while a retained
/// filesystem ID changes pathname permission checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PayloadCredentials {
    pub(super) real_uid: u32,
    pub(super) effective_uid: u32,
    pub(super) saved_uid: u32,
    pub(super) filesystem_uid: u32,
    pub(super) real_gid: u32,
    pub(super) effective_gid: u32,
    pub(super) saved_gid: u32,
    pub(super) filesystem_gid: u32,
    pub(super) supplementary_group_count: usize,
}

impl PayloadCredentials {
    fn is_isolated(&self) -> bool {
        self.real_uid == 0
            && self.effective_uid == 0
            && self.saved_uid == 0
            && self.filesystem_uid == 0
            && self.real_gid == 0
            && self.effective_gid == 0
            && self.saved_gid == 0
            && self.filesystem_gid == 0
            && self.supplementary_group_count == 0
    }
}

impl fmt::Display for PayloadCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "uid {}/{}/{}/{}, gid {}/{}/{}/{}, supplementary group count {}",
            self.real_uid,
            self.effective_uid,
            self.saved_uid,
            self.filesystem_uid,
            self.real_gid,
            self.effective_gid,
            self.saved_gid,
            self.filesystem_gid,
            self.supplementary_group_count,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum IsolationFailure {
    ClearSupplementaryGroups(CredentialSyscallError),
    NormalizeGroupCredentials(CredentialSyscallError),
    NormalizeUserCredentials(CredentialSyscallError),
    ReadSupplementaryGroups(CredentialSyscallError),
    ReadGroupCredentials(CredentialSyscallError),
    ReadUserCredentials(CredentialSyscallError),
    UnexpectedCredentials(PayloadCredentials),
}

trait CredentialSyscalls {
    fn clear_supplementary_groups(&mut self) -> Result<(), CredentialSyscallError>;
    fn normalize_group_credentials(&mut self) -> Result<(), CredentialSyscallError>;
    fn normalize_user_credentials(&mut self) -> Result<(), CredentialSyscallError>;
    fn supplementary_group_count(&mut self) -> Result<usize, CredentialSyscallError>;
    fn group_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError>;
    fn filesystem_gid(&mut self) -> u32;
    fn user_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError>;
    fn filesystem_uid(&mut self) -> u32;
}

struct RawCredentialSyscalls;

impl RawCredentialSyscalls {
    fn zero_result(result: nix::libc::c_long) -> Result<(), CredentialSyscallError> {
        match result {
            -1 => Err(CredentialSyscallError::Kernel(Errno::last())),
            0 => Ok(()),
            result => Err(CredentialSyscallError::UnexpectedReturn(result)),
        }
    }
}

impl CredentialSyscalls for RawCredentialSyscalls {
    fn clear_supplementary_groups(&mut self) -> Result<(), CredentialSyscallError> {
        // SAFETY: setgroups receives a zero count, so the null list is never
        // dereferenced.  Use the syscall directly: glibc's setxid wrappers
        // coordinate with sibling pthreads, which do not exist in this raw
        // clone child and may hold copied userspace locks.
        let result = unsafe { nix::libc::syscall(nix::libc::SYS_setgroups, 0_usize, ptr::null::<nix::libc::gid_t>()) };
        Self::zero_result(result)
    }

    fn normalize_group_credentials(&mut self) -> Result<(), CredentialSyscallError> {
        // SAFETY: direct setresgid takes three scalar namespace GIDs.  The
        // parent has mapped namespace GID zero and deliberately kept
        // setgroups enabled until the child clears the inherited list above.
        let result = unsafe { nix::libc::syscall(nix::libc::SYS_setresgid, 0_u32, 0_u32, 0_u32) };
        Self::zero_result(result)
    }

    fn normalize_user_credentials(&mut self) -> Result<(), CredentialSyscallError> {
        // SAFETY: direct setresuid takes three scalar namespace UIDs.  Avoid
        // the glibc/NPTL setxid wrapper for the same raw-clone reason as
        // setgroups.  Linux also resets fsuid to the new effective UID.
        let result = unsafe { nix::libc::syscall(nix::libc::SYS_setresuid, 0_u32, 0_u32, 0_u32) };
        Self::zero_result(result)
    }

    fn supplementary_group_count(&mut self) -> Result<usize, CredentialSyscallError> {
        // SAFETY: a zero-sized getgroups query does not dereference its null
        // list.  Merely reading the count avoids allocating in the raw child;
        // the only accepted count is zero.
        let result =
            unsafe { nix::libc::syscall(nix::libc::SYS_getgroups, 0_usize, ptr::null_mut::<nix::libc::gid_t>()) };
        if result == -1 {
            return Err(CredentialSyscallError::Kernel(Errno::last()));
        }
        usize::try_from(result).map_err(|_| CredentialSyscallError::Kernel(Errno::EOVERFLOW))
    }

    fn group_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError> {
        let mut real = 0;
        let mut effective = 0;
        let mut saved = 0;
        // SAFETY: getresgid writes one gid_t to each valid local pointer.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getresgid,
                &mut real as *mut nix::libc::gid_t,
                &mut effective as *mut nix::libc::gid_t,
                &mut saved as *mut nix::libc::gid_t,
            )
        };
        Self::zero_result(result)?;
        Ok((real, effective, saved))
    }

    fn filesystem_gid(&mut self) -> u32 {
        // SAFETY: Linux defines setfsgid(-1) as a query: the invalid all-ones
        // GID is ignored and the previous filesystem GID is returned.  This
        // syscall has no separate error return.
        unsafe { nix::libc::syscall(nix::libc::SYS_setfsgid, u32::MAX) as u32 }
    }

    fn user_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError> {
        let mut real = 0;
        let mut effective = 0;
        let mut saved = 0;
        // SAFETY: getresuid writes one uid_t to each valid local pointer.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getresuid,
                &mut real as *mut nix::libc::uid_t,
                &mut effective as *mut nix::libc::uid_t,
                &mut saved as *mut nix::libc::uid_t,
            )
        };
        Self::zero_result(result)?;
        Ok((real, effective, saved))
    }

    fn filesystem_uid(&mut self) -> u32 {
        // SAFETY: as with setfsgid(-1), Linux treats setfsuid(-1) as a query
        // and returns the unchanged previous filesystem UID.
        unsafe { nix::libc::syscall(nix::libc::SYS_setfsuid, u32::MAX) as u32 }
    }
}

pub(super) fn isolate_payload_credentials() -> Result<(), IsolationFailure> {
    isolate_payload_credentials_with(&mut RawCredentialSyscalls)
}

pub(super) fn read_current_identity() -> Result<IdentityCredentials, ReadIdentityFailure> {
    read_identity_with(&mut RawCredentialSyscalls)
}

fn read_identity_with(syscalls: &mut impl CredentialSyscalls) -> Result<IdentityCredentials, ReadIdentityFailure> {
    let (real_gid, effective_gid, saved_gid) = syscalls
        .group_credentials()
        .map_err(ReadIdentityFailure::ReadGroupCredentials)?;
    let filesystem_gid = syscalls.filesystem_gid();
    let (real_uid, effective_uid, saved_uid) = syscalls
        .user_credentials()
        .map_err(ReadIdentityFailure::ReadUserCredentials)?;
    let filesystem_uid = syscalls.filesystem_uid();
    Ok(IdentityCredentials {
        real_uid,
        effective_uid,
        saved_uid,
        filesystem_uid,
        real_gid,
        effective_gid,
        saved_gid,
        filesystem_gid,
    })
}

fn isolate_payload_credentials_with(syscalls: &mut impl CredentialSyscalls) -> Result<(), IsolationFailure> {
    // An empty inherited group list already satisfies the boundary.  Avoid a
    // no-op setgroups call in that exact case because some LSM policies deny
    // the mutation even though there is nothing to clear.  Any nonempty list
    // must still be cleared successfully; there is no permissive fallback.
    let inherited_group_count = syscalls
        .supplementary_group_count()
        .map_err(IsolationFailure::ReadSupplementaryGroups)?;
    if inherited_group_count != 0 {
        syscalls
            .clear_supplementary_groups()
            .map_err(IsolationFailure::ClearSupplementaryGroups)?;
    }

    // GIDs must be normalized while the child still certainly possesses the
    // namespace authority needed to change them.  UIDs come second.  Both
    // operations set the filesystem ID to their new effective ID on Linux.
    syscalls
        .normalize_group_credentials()
        .map_err(IsolationFailure::NormalizeGroupCredentials)?;
    syscalls
        .normalize_user_credentials()
        .map_err(IsolationFailure::NormalizeUserCredentials)?;

    let supplementary_group_count = syscalls
        .supplementary_group_count()
        .map_err(IsolationFailure::ReadSupplementaryGroups)?;
    let identity = read_identity_with(syscalls).map_err(|failure| match failure {
        ReadIdentityFailure::ReadGroupCredentials(source) => IsolationFailure::ReadGroupCredentials(source),
        ReadIdentityFailure::ReadUserCredentials(source) => IsolationFailure::ReadUserCredentials(source),
    })?;

    let credentials = PayloadCredentials {
        real_uid: identity.real_uid,
        effective_uid: identity.effective_uid,
        saved_uid: identity.saved_uid,
        filesystem_uid: identity.filesystem_uid,
        real_gid: identity.real_gid,
        effective_gid: identity.effective_gid,
        saved_gid: identity.saved_gid,
        filesystem_gid: identity.filesystem_gid,
        supplementary_group_count,
    };
    if !credentials.is_isolated() {
        return Err(IsolationFailure::UnexpectedCredentials(credentials));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialSyscallError, CredentialSyscalls, IsolationFailure, PayloadCredentials, RawCredentialSyscalls,
        isolate_payload_credentials_with,
    };
    use nix::errno::Errno;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Step {
        ClearGroups,
        NormalizeGids,
        NormalizeUids,
        ReadInheritedGroups,
        ReadIsolatedGroups,
        ReadGids,
        ReadFilesystemGid,
        ReadUids,
        ReadFilesystemUid,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RetainedSlot {
        RealUid,
        EffectiveUid,
        SavedUid,
        FilesystemUid,
        RealGid,
        EffectiveGid,
        SavedGid,
        FilesystemGid,
        SupplementaryGroups,
    }

    struct FakeCredentialSyscalls {
        credentials: PayloadCredentials,
        calls: Vec<Step>,
        fail_at: Option<Step>,
        retained_slot: Option<RetainedSlot>,
        group_reads: usize,
    }

    impl FakeCredentialSyscalls {
        fn inherited() -> Self {
            Self {
                credentials: PayloadCredentials {
                    real_uid: 1000,
                    effective_uid: 1000,
                    saved_uid: 65534,
                    filesystem_uid: 65533,
                    real_gid: 1000,
                    effective_gid: 1000,
                    saved_gid: 65532,
                    filesystem_gid: 65531,
                    supplementary_group_count: 3,
                },
                calls: Vec::new(),
                fail_at: None,
                retained_slot: None,
                group_reads: 0,
            }
        }

        fn visit(&mut self, step: Step) -> Result<(), CredentialSyscallError> {
            self.calls.push(step);
            if self.fail_at == Some(step) {
                Err(CredentialSyscallError::Kernel(Errno::EPERM))
            } else {
                Ok(())
            }
        }
    }

    impl CredentialSyscalls for FakeCredentialSyscalls {
        fn clear_supplementary_groups(&mut self) -> Result<(), CredentialSyscallError> {
            self.visit(Step::ClearGroups)?;
            if self.retained_slot != Some(RetainedSlot::SupplementaryGroups) {
                self.credentials.supplementary_group_count = 0;
            }
            Ok(())
        }

        fn normalize_group_credentials(&mut self) -> Result<(), CredentialSyscallError> {
            self.visit(Step::NormalizeGids)?;
            if self.retained_slot != Some(RetainedSlot::RealGid) {
                self.credentials.real_gid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::EffectiveGid) {
                self.credentials.effective_gid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::SavedGid) {
                self.credentials.saved_gid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::FilesystemGid) {
                self.credentials.filesystem_gid = 0;
            }
            Ok(())
        }

        fn normalize_user_credentials(&mut self) -> Result<(), CredentialSyscallError> {
            self.visit(Step::NormalizeUids)?;
            if self.retained_slot != Some(RetainedSlot::RealUid) {
                self.credentials.real_uid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::EffectiveUid) {
                self.credentials.effective_uid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::SavedUid) {
                self.credentials.saved_uid = 0;
            }
            if self.retained_slot != Some(RetainedSlot::FilesystemUid) {
                self.credentials.filesystem_uid = 0;
            }
            Ok(())
        }

        fn supplementary_group_count(&mut self) -> Result<usize, CredentialSyscallError> {
            let step = if self.group_reads == 0 {
                Step::ReadInheritedGroups
            } else {
                Step::ReadIsolatedGroups
            };
            self.group_reads += 1;
            self.visit(step)?;
            Ok(self.credentials.supplementary_group_count)
        }

        fn group_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError> {
            self.visit(Step::ReadGids)?;
            Ok((
                self.credentials.real_gid,
                self.credentials.effective_gid,
                self.credentials.saved_gid,
            ))
        }

        fn filesystem_gid(&mut self) -> u32 {
            self.calls.push(Step::ReadFilesystemGid);
            self.credentials.filesystem_gid
        }

        fn user_credentials(&mut self) -> Result<(u32, u32, u32), CredentialSyscallError> {
            self.visit(Step::ReadUids)?;
            Ok((
                self.credentials.real_uid,
                self.credentials.effective_uid,
                self.credentials.saved_uid,
            ))
        }

        fn filesystem_uid(&mut self) -> u32 {
            self.calls.push(Step::ReadFilesystemUid);
            self.credentials.filesystem_uid
        }
    }

    #[test]
    fn isolation_normalizes_and_verifies_every_credential_slot_in_security_order() {
        let mut syscalls = FakeCredentialSyscalls::inherited();
        isolate_payload_credentials_with(&mut syscalls).unwrap();
        assert_eq!(
            syscalls.calls,
            [
                Step::ReadInheritedGroups,
                Step::ClearGroups,
                Step::NormalizeGids,
                Step::NormalizeUids,
                Step::ReadIsolatedGroups,
                Step::ReadGids,
                Step::ReadFilesystemGid,
                Step::ReadUids,
                Step::ReadFilesystemUid,
            ]
        );
        assert!(syscalls.credentials.is_isolated());
    }

    #[test]
    fn already_empty_supplementary_groups_skip_the_mutating_syscall() {
        let mut syscalls = FakeCredentialSyscalls::inherited();
        syscalls.credentials.supplementary_group_count = 0;
        isolate_payload_credentials_with(&mut syscalls).unwrap();
        assert_eq!(syscalls.calls.first(), Some(&Step::ReadInheritedGroups));
        assert!(!syscalls.calls.contains(&Step::ClearGroups));
        assert!(syscalls.credentials.is_isolated());
    }

    #[test]
    fn isolation_stops_at_every_fallible_syscall_failure() {
        let denied = CredentialSyscallError::Kernel(Errno::EPERM);
        for (step, expected) in [
            (
                Step::ClearGroups,
                IsolationFailure::ClearSupplementaryGroups(denied.clone()),
            ),
            (
                Step::NormalizeGids,
                IsolationFailure::NormalizeGroupCredentials(denied.clone()),
            ),
            (
                Step::NormalizeUids,
                IsolationFailure::NormalizeUserCredentials(denied.clone()),
            ),
            (
                Step::ReadInheritedGroups,
                IsolationFailure::ReadSupplementaryGroups(denied.clone()),
            ),
            (
                Step::ReadIsolatedGroups,
                IsolationFailure::ReadSupplementaryGroups(denied.clone()),
            ),
            (Step::ReadGids, IsolationFailure::ReadGroupCredentials(denied.clone())),
            (Step::ReadUids, IsolationFailure::ReadUserCredentials(denied.clone())),
        ] {
            let mut syscalls = FakeCredentialSyscalls::inherited();
            syscalls.fail_at = Some(step);
            assert_eq!(isolate_payload_credentials_with(&mut syscalls), Err(expected));
            assert_eq!(syscalls.calls.last(), Some(&step));
        }
    }

    #[test]
    fn zero_return_contract_rejects_unexpected_positive_results() {
        assert_eq!(RawCredentialSyscalls::zero_result(0), Ok(()));
        assert_eq!(
            RawCredentialSyscalls::zero_result(1),
            Err(CredentialSyscallError::UnexpectedReturn(1))
        );
    }

    #[test]
    fn isolation_rejects_each_non_root_or_supplementary_credential_slot() {
        let isolated = PayloadCredentials {
            real_uid: 0,
            effective_uid: 0,
            saved_uid: 0,
            filesystem_uid: 0,
            real_gid: 0,
            effective_gid: 0,
            saved_gid: 0,
            filesystem_gid: 0,
            supplementary_group_count: 0,
        };
        assert!(isolated.is_isolated());

        for mutate in [
            |credentials: &mut PayloadCredentials| credentials.real_uid = 1,
            |credentials: &mut PayloadCredentials| credentials.effective_uid = 1,
            |credentials: &mut PayloadCredentials| credentials.saved_uid = 1,
            |credentials: &mut PayloadCredentials| credentials.filesystem_uid = 1,
            |credentials: &mut PayloadCredentials| credentials.real_gid = 1,
            |credentials: &mut PayloadCredentials| credentials.effective_gid = 1,
            |credentials: &mut PayloadCredentials| credentials.saved_gid = 1,
            |credentials: &mut PayloadCredentials| credentials.filesystem_gid = 1,
            |credentials: &mut PayloadCredentials| credentials.supplementary_group_count = 1,
        ] {
            let mut credentials = isolated.clone();
            mutate(&mut credentials);
            assert!(!credentials.is_isolated(), "accepted {credentials}");
        }
    }

    #[test]
    fn post_normalization_verification_rejects_every_retained_slot() {
        for slot in [
            RetainedSlot::RealUid,
            RetainedSlot::EffectiveUid,
            RetainedSlot::SavedUid,
            RetainedSlot::FilesystemUid,
            RetainedSlot::RealGid,
            RetainedSlot::EffectiveGid,
            RetainedSlot::SavedGid,
            RetainedSlot::FilesystemGid,
            RetainedSlot::SupplementaryGroups,
        ] {
            let mut syscalls = FakeCredentialSyscalls::inherited();
            syscalls.retained_slot = Some(slot);
            assert!(matches!(
                isolate_payload_credentials_with(&mut syscalls),
                Err(IsolationFailure::UnexpectedCredentials(credentials)) if !credentials.is_isolated()
            ));
            assert_eq!(syscalls.calls.last(), Some(&Step::ReadFilesystemUid));
        }
    }
}
