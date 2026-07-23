//! Test-only one-leaf observation on an ordinary-directory topology fixture.

use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::OpenOptions,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::PathBuf,
};

use crate::linux_fs::{
    descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState,
        BootNamespaceRequest, RetainedBootNamespaceAssessmentLimits,
        RetainedBootNamespaceExpectedSource, assess_retained_boot_namespace_until,
    },
    mount_namespace::{
        PreparedMountNamespaceAnchor, RetainedBootFilePublicationLimits,
        RetainedBootFilePublicationOutcome, RetainedBootFilePublicationRequest,
        ValidatedRetainedBootFilePublication,
    },
};

use super::{
    ActiveReblitBootImmutableLeafPublicationError,
    RevalidatedActiveReblitBootPublicationTarget,
};

pub(super) struct FixtureImmutableLeafAssessment {
    state: BootNamespaceDestinationState,
    root: PathBuf,
}

impl FixtureImmutableLeafAssessment {
    pub(super) const fn state(&self) -> BootNamespaceDestinationState {
        self.state
    }

    pub(super) fn publish(
        self,
        parent_components: &[&str],
        request: RetainedBootFilePublicationRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'_>,
        expected_outcome: RetainedBootFilePublicationOutcome,
        deadline: std::time::Instant,
    ) -> Result<ValidatedRetainedBootFilePublication, ActiveReblitBootImmutableLeafPublicationError> {
        let selector = self.root.to_str().expect("fixture publication root is UTF-8");
        let anchor = PreparedMountNamespaceAnchor::prepare()
            .expect("prepare current-task namespace anchor for fixture publication");
        let attachment = anchor
            .revalidate()
            .expect("revalidate fixture publication namespace")
            .prepare_task_rooted_attachment(selector)
            .expect("prepare fixture publication attachment");
        let target = attachment
            .revalidate_against(&anchor)
            .expect("revalidate fixture publication attachment");
        let parent = target
            .retain_boot_publication_parent_until(parent_components, deadline)
            .map_err(ActiveReblitBootImmutableLeafPublicationError::PublicationParent)?;
        let evidence = parent
            .publish_immutable_boot_file_until(
                request,
                expected_source,
                RetainedBootFilePublicationLimits::default(),
                deadline,
            )
            .map_err(ActiveReblitBootImmutableLeafPublicationError::LeafPublication)?;
        if !parent.matches_leaf_evidence(&evidence) {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafParentIdentity);
        }
        if evidence.length() != request.expected_length()
            || evidence.xxh3() != request.expected_xxh3()
            || evidence.sha256() != request.expected_sha256()
        {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafContentIdentity);
        }
        if evidence.outcome() != expected_outcome {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafOutcome {
                expected: expected_outcome,
                found: evidence.outcome(),
            });
        }
        Ok(evidence)
    }
}

thread_local! {
    static ROOTS: RefCell<VecDeque<PathBuf>> = const { RefCell::new(VecDeque::new()) };
}

pub(in crate::client) struct FixtureImmutableLeafAssessmentGuard;

impl Drop for FixtureImmutableLeafAssessmentGuard {
    fn drop(&mut self) {
        ROOTS.with(|roots| roots.borrow_mut().clear());
    }
}

pub(in crate::client) fn arm_fixture_immutable_leaf_assessments(
    root: PathBuf,
    count: usize,
) -> FixtureImmutableLeafAssessmentGuard {
    assert!(count > 0, "fixture leaf assessment queue is empty");
    ROOTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_empty(), "fixture leaf assessment queue is already armed");
        slot.extend(std::iter::repeat_n(root, count));
    });
    FixtureImmutableLeafAssessmentGuard
}

pub(in crate::client) fn fixture_immutable_leaf_assessments_remaining() -> usize {
    ROOTS.with(|roots| roots.borrow().len())
}

pub(super) fn take(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    request: BootNamespaceRequest<'_>,
    expected_source: &RetainedBootNamespaceExpectedSource<'_>,
) -> Option<FixtureImmutableLeafAssessment> {
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
        .expect("open fixture immutable-leaf root");
    let metadata = root.metadata().expect("inspect fixture immutable-leaf root");
    let destination = target.destination();
    assert_eq!(metadata.dev(), destination.raw_device());
    assert_eq!(metadata.ino(), destination.inode());
    let assessment = assess_retained_boot_namespace_until(
        &root,
        std::slice::from_ref(&request),
        std::slice::from_ref(expected_source),
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        target.deadline(),
    )
    .expect("observe fixture immutable leaf");
    let [state] = assessment.states() else {
        panic!("one fixture leaf request returned multiple states")
    };
    Some(FixtureImmutableLeafAssessment {
        state: *state,
        root: root_path,
    })
}
