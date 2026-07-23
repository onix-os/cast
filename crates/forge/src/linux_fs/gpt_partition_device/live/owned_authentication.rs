//! Closed composition of retained devtmpfs and GPT read authority.
//!
//! The production entry point requires its caller to supply a retained root
//! which the enclosing attachment adapter already authenticated as devtmpfs,
//! together with that root's authenticated mount ID and one exact sysfs
//! expectation. This layer does not authenticate the root or create devtmpfs
//! attachment evidence itself. It privately retains that expectation's parent
//! block node, authenticates both GPT passes through the same descriptor, and
//! consumes the parent capability in a terminal same-name rebind before any
//! scalar evidence can escape.

use std::{fs::File, io, time::Instant};

use crate::linux_fs::{gpt_partition_role::GptPartitionRole, sysfs_identity::SysfsGptDeviceExpectation};

use super::{
    authentication::{
        LiveAuthenticatedGptPartitionDeviceEvidence, authenticate_retained_gpt_partition_device_with_interpass_until,
    },
    retained_parent::{RetainedGptParentBlockDevice, retain_gpt_parent_block_device_until},
};

/// Authenticate one sysfs-selected GPT parent beneath caller-authenticated devtmpfs.
///
/// The same `expected` value supplies the relative parent name, device number,
/// partition geometry, UUID, and partition number to both node retention and
/// GPT reconciliation. Callers cannot replace the mandatory inter-pass name
/// rebind. No active descriptor, observer, callback, path, or reopen authority
/// survives the terminal consuming rebind.
/// This result is GPT read provenance only; it does not independently prove
/// that `devtmpfs_root` is the attachment-authenticated devtmpfs root.
pub(in crate::linux_fs) fn authenticate_retained_devtmpfs_gpt_partition_device_until(
    devtmpfs_root: &File,
    authenticated_root_mount_id: u64,
    expected: &SysfsGptDeviceExpectation<'_>,
    expected_role: GptPartitionRole,
    deadline: Instant,
) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
    let parent = retain_gpt_parent_block_device_until(devtmpfs_root, authenticated_root_mount_id, expected, deadline)?;
    authenticate_owned_parent_source_until(
        ProductionOwnedParentAuthentication { parent, expected },
        expected_role,
        deadline,
    )
}

/// Private contract which keeps active parent authority inside this module.
///
/// Authentication borrows the source. Finalization consumes it, so a caller
/// can receive evidence only after the active capability has performed its
/// terminal same-name rebind and has been released.
trait OwnedParentAuthenticationSource {
    type Evidence;

    fn authenticate_borrowed_until(
        &mut self,
        expected_role: GptPartitionRole,
        deadline: Instant,
    ) -> io::Result<Self::Evidence>;

    fn closing_rebind_until(self, deadline: Instant) -> io::Result<()>;
}

struct ProductionOwnedParentAuthentication<'root, 'expectation, 'view> {
    parent: RetainedGptParentBlockDevice<'root, 'expectation>,
    expected: &'view SysfsGptDeviceExpectation<'expectation>,
}

impl OwnedParentAuthenticationSource for ProductionOwnedParentAuthentication<'_, '_, '_> {
    type Evidence = LiveAuthenticatedGptPartitionDeviceEvidence;

    fn authenticate_borrowed_until(
        &mut self,
        expected_role: GptPartitionRole,
        deadline: Instant,
    ) -> io::Result<Self::Evidence> {
        let retained_opening = self.parent.canonical_opening();
        let mut observer = self.parent.same_descriptor_observer()?;
        let mut rebind_parent_name = |received_deadline| self.parent.rebind_same_name_until(received_deadline);
        let authenticated = authenticate_retained_gpt_partition_device_with_interpass_until(
            &mut observer,
            retained_opening,
            self.expected,
            expected_role,
            deadline,
            &mut rebind_parent_name,
        );
        drop(observer);
        authenticated
    }

    fn closing_rebind_until(self, deadline: Instant) -> io::Result<()> {
        self.parent.closing_rebind_until(deadline)
    }
}

fn authenticate_owned_parent_source_until<Source>(
    mut source: Source,
    expected_role: GptPartitionRole,
    deadline: Instant,
) -> io::Result<Source::Evidence>
where
    Source: OwnedParentAuthenticationSource,
{
    let authenticated = source.authenticate_borrowed_until(expected_role, deadline);
    finalize_owned_parent_authentication_until(source, authenticated, deadline)
}

fn finalize_owned_parent_authentication_until<Source>(
    source: Source,
    authenticated: io::Result<Source::Evidence>,
    deadline: Instant,
) -> io::Result<Source::Evidence>
where
    Source: OwnedParentAuthenticationSource,
{
    let evidence = authenticated?;
    source.closing_rebind_until(deadline)?;
    Ok(evidence)
}

#[cfg(test)]
struct FixtureOwnedParentAuthentication<'callbacks, Parent, Evidence, Authenticate, Close> {
    parent: Parent,
    authenticate: &'callbacks mut Authenticate,
    close: &'callbacks mut Close,
    _evidence: std::marker::PhantomData<fn() -> Evidence>,
}

#[cfg(test)]
impl<Parent, Evidence, Authenticate, Close> OwnedParentAuthenticationSource
    for FixtureOwnedParentAuthentication<'_, Parent, Evidence, Authenticate, Close>
where
    Authenticate: FnMut(&Parent, GptPartitionRole, Instant) -> io::Result<Evidence>,
    Close: FnMut(Parent, Instant) -> io::Result<()>,
{
    type Evidence = Evidence;

    fn authenticate_borrowed_until(
        &mut self,
        expected_role: GptPartitionRole,
        deadline: Instant,
    ) -> io::Result<Self::Evidence> {
        (self.authenticate)(&self.parent, expected_role, deadline)
    }

    fn closing_rebind_until(self, deadline: Instant) -> io::Result<()> {
        let Self { parent, close, .. } = self;
        close(parent, deadline)
    }
}

#[cfg(test)]
pub(in crate::linux_fs) fn authenticate_owned_gpt_parent_fixture_until<Parent, Evidence>(
    parent: Parent,
    expected_role: GptPartitionRole,
    deadline: Instant,
    authenticate: &mut impl FnMut(&Parent, GptPartitionRole, Instant) -> io::Result<Evidence>,
    close: &mut impl FnMut(Parent, Instant) -> io::Result<()>,
) -> io::Result<Evidence> {
    authenticate_owned_parent_source_until(
        FixtureOwnedParentAuthentication {
            parent,
            authenticate,
            close,
            _evidence: std::marker::PhantomData,
        },
        expected_role,
        deadline,
    )
}
