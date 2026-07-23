//! Test-only ordinary-directory target for promoted owned cleanup.

use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::OpenOptions,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::PathBuf,
};

use crate::{
    client::active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaExpected,
    linux_fs::mount_namespace::{
        PreparedMountNamespaceAnchor, RetainedBootFileMutationFingerprint,
        RevalidatedTaskRootedAttachment, ValidatedRetainedBootFileReplacement,
    },
};

use super::{
    ActiveReblitBootOwnedCleanupError, ActiveReblitBootOwnedCleanupOutcome,
    OwnedCleanupPath, OwnedCleanupTargetIdentity,
    RevalidatedActiveReblitBootPublicationTarget,
    reconcile_and_cleanup_replacement_at, reconcile_and_cleanup_stale_at,
};

thread_local! {
    static ROOTS: RefCell<VecDeque<PathBuf>> = const {
        RefCell::new(VecDeque::new())
    };
}

pub(in crate::client) struct FixtureOwnedCleanupTargetGuard;

impl Drop for FixtureOwnedCleanupTargetGuard {
    fn drop(&mut self) {
        ROOTS.with(|roots| roots.borrow_mut().clear());
    }
}

/// Arm one ordinary publication root for each expected cleanup operation.
pub(in crate::client) fn arm_fixture_owned_cleanup_targets(
    root: PathBuf,
    count: usize,
) -> FixtureOwnedCleanupTargetGuard {
    assert!(count > 0, "fixture owned-cleanup target queue is empty");
    ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        assert!(
            roots.is_empty(),
            "fixture owned-cleanup target queue is already armed",
        );
        roots.extend(std::iter::repeat_n(root, count));
    });
    FixtureOwnedCleanupTargetGuard
}

pub(in crate::client) fn fixture_owned_cleanup_targets_remaining() -> usize {
    ROOTS.with(|roots| roots.borrow().len())
}

pub(super) fn take(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
) -> Option<FixtureOwnedCleanupTarget> {
    let root_path = ROOTS.with(|roots| roots.borrow_mut().pop_front())?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_PATH
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(&root_path)
        .expect("open fixture owned-cleanup root");
    let metadata = root
        .metadata()
        .expect("inspect fixture owned-cleanup root");
    let destination = target.destination();
    assert_eq!(
        metadata.dev(),
        destination.raw_device(),
        "fixture cleanup root device differs from synthetic target",
    );
    assert_eq!(
        metadata.ino(),
        destination.inode(),
        "fixture cleanup root inode differs from synthetic target",
    );
    Some(FixtureOwnedCleanupTarget { root: root_path })
}

pub(super) struct FixtureOwnedCleanupTarget {
    root: PathBuf,
}

impl FixtureOwnedCleanupTarget {
    pub(super) fn reconcile_and_cleanup_replacement(
        self,
        synthetic: &RevalidatedActiveReblitBootPublicationTarget<'_>,
        plan_index: usize,
        path: &OwnedCleanupPath<'_>,
        historical: &ValidatedRetainedBootFileReplacement,
    ) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
        self.with_real_target(synthetic, |target, identity, deadline| {
            reconcile_and_cleanup_replacement_at(
                target,
                identity,
                deadline,
                plan_index,
                path,
                historical,
            )
        })
    }

    pub(super) fn reconcile_and_cleanup_stale(
        self,
        synthetic: &RevalidatedActiveReblitBootPublicationTarget<'_>,
        delta_index: usize,
        path: &OwnedCleanupPath<'_>,
        expected: ActiveReblitBootPublicationDeltaExpected,
        owner: RetainedBootFileMutationFingerprint,
    ) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
        self.with_real_target(synthetic, |target, identity, deadline| {
            reconcile_and_cleanup_stale_at(
                target,
                identity,
                deadline,
                delta_index,
                path,
                expected,
                owner,
            )
        })
    }

    pub(super) fn with_real_target<Result>(
        self,
        synthetic: &RevalidatedActiveReblitBootPublicationTarget<'_>,
        operation: impl FnOnce(
            &RevalidatedTaskRootedAttachment<'_>,
            OwnedCleanupTargetIdentity,
            std::time::Instant,
        ) -> Result,
    ) -> Result {
        let deadline = synthetic.deadline();
        let selector = self
            .root
            .to_str()
            .expect("fixture owned-cleanup root is UTF-8");
        let anchor = PreparedMountNamespaceAnchor::prepare_until(deadline)
            .expect("prepare current-task namespace anchor for fixture cleanup");
        let prepared = anchor
            .revalidate_until(deadline)
            .expect("revalidate fixture cleanup namespace")
            .prepare_task_rooted_attachment_until(selector, deadline)
            .expect("prepare fixture cleanup attachment");
        let target = prepared
            .revalidate_against_until(&anchor, deadline)
            .expect("revalidate fixture cleanup attachment");
        let identity = OwnedCleanupTargetIdentity::from_attachment(&target);
        let destination = synthetic.destination();
        assert_eq!(
            identity.device,
            destination.raw_device(),
            "fresh fixture cleanup target device differs from synthetic target",
        );
        assert_eq!(
            identity.inode,
            destination.inode(),
            "fresh fixture cleanup target inode differs from synthetic target",
        );
        operation(&target, identity, deadline)
    }
}
