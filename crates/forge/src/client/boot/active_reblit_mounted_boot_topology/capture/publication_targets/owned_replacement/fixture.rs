//! Test-only desired reassessment on an ordinary-directory topology fixture.

use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::OpenOptions,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::PathBuf,
};

use crate::linux_fs::descriptor_boot_namespace::{
    BootNamespaceAssessmentLimits, BootNamespaceDestinationState,
    BootNamespaceRequest, RetainedBootNamespaceAssessmentLimits,
    RetainedBootNamespaceExpectedSource, assess_retained_boot_namespace_until,
};
use crate::linux_fs::mount_namespace::{
    PreparedMountNamespaceAnchor, RetainedBootFileMutationFingerprint,
    RetainedBootFilePublicationLimits, RetainedBootFilePublicationRequest,
    RetainedBootFileReplacementRequest, RetainedBootLeafAssessmentLimits,
    RetainedBootLeafAssessmentRequest, RetainedBootLeafAssessmentState,
    RetainedBootPublicationParent, RevalidatedTaskRootedAttachment,
    ValidatedRetainedBootFileReplacement, ValidatedRetainedBootLeafAssessment,
};

use super::{
    ActiveReblitBootOwnedLeafReplacementError,
    RevalidatedActiveReblitBootPublicationTarget,
};

pub(super) struct FixtureOwnedReplacementAssessment {
    state: BootNamespaceDestinationState,
    root: PathBuf,
    validation_count: usize,
}

impl FixtureOwnedReplacementAssessment {
    pub(super) const fn state(&self) -> BootNamespaceDestinationState {
        self.state
    }

    pub(super) fn replace(
        self,
        parent_components: &[&str],
        installed: RetainedBootFilePublicationRequest<'_>,
        replacement: RetainedBootFilePublicationRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'_>,
        owner: RetainedBootFileMutationFingerprint,
        deadline: std::time::Instant,
    ) -> Result<
        ValidatedRetainedBootFileReplacement,
        ActiveReblitBootOwnedLeafReplacementError,
    > {
        let selector = self
            .root
            .to_str()
            .expect("fixture owned-replacement root is UTF-8");
        let anchor = PreparedMountNamespaceAnchor::prepare()
            .expect("prepare current-task namespace anchor for fixture replacement");
        let attachment = anchor
            .revalidate()
            .expect("revalidate fixture replacement namespace")
            .prepare_task_rooted_attachment(selector)
            .expect("prepare fixture replacement attachment");
        let target = attachment
            .revalidate_against(&anchor)
            .expect("revalidate fixture replacement attachment");
        let assessment = target
            .assess_boot_leaf_below_parent_until(
                parent_components,
                RetainedBootLeafAssessmentRequest::new(
                    installed.canonical_leaf(),
                    installed.expected_length(),
                    installed.expected_xxh3(),
                    installed.expected_sha256(),
                ),
                RetainedBootLeafAssessmentLimits::default(),
                deadline,
            )
            .map_err(
                ActiveReblitBootOwnedLeafReplacementError::InstalledAssessment,
            )?;
        require_installed_assessment(
            &target,
            parent_components,
            installed,
            &assessment,
        )?;
        let parent = target
            .retain_existing_boot_publication_parent_until(
                parent_components,
                deadline,
            )
            .map_err(ActiveReblitBootOwnedLeafReplacementError::PublicationParent)?;
        require_parent_identity(&target, &parent, &assessment)?;
        let evidence = parent
            .replace_exact_boot_file_until(
                RetainedBootFileReplacementRequest::new(
                    installed,
                    replacement,
                    owner,
                ),
                expected_source,
                RetainedBootFilePublicationLimits::default(),
                deadline,
            )
            .map_err(ActiveReblitBootOwnedLeafReplacementError::LeafReplacement)?;
        VALIDATION_ROOTS.with(|roots| {
            roots
                .borrow_mut()
                .extend(std::iter::repeat_n(self.root, self.validation_count));
        });
        Ok(evidence)
    }
}

thread_local! {
    static ROOTS: RefCell<VecDeque<(PathBuf, usize)>> = const {
        RefCell::new(VecDeque::new())
    };
    static VALIDATION_ROOTS: RefCell<VecDeque<PathBuf>> = const {
        RefCell::new(VecDeque::new())
    };
}

pub(in crate::client) struct FixtureOwnedReplacementAssessmentGuard;

impl Drop for FixtureOwnedReplacementAssessmentGuard {
    fn drop(&mut self) {
        ROOTS.with(|roots| roots.borrow_mut().clear());
        VALIDATION_ROOTS.with(|roots| roots.borrow_mut().clear());
    }
}

pub(in crate::client) fn arm_fixture_owned_replacement_assessments(
    root: PathBuf,
    count: usize,
    validation_count: usize,
) -> FixtureOwnedReplacementAssessmentGuard {
    assert!(count > 0, "fixture owned-replacement assessment queue is empty");
    assert!(
        validation_count > 0,
        "fixture owned-replacement validation queue is empty",
    );
    ROOTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_empty(),
            "fixture owned-replacement assessment queue is already armed",
        );
        slot.extend(std::iter::repeat_n((root, validation_count), count));
    });
    FixtureOwnedReplacementAssessmentGuard
}

pub(in crate::client) fn fixture_owned_replacement_assessments_remaining(
) -> usize {
    ROOTS.with(|roots| roots.borrow().len())
}

pub(in crate::client) fn fixture_owned_replacement_validations_remaining(
) -> usize {
    VALIDATION_ROOTS.with(|roots| roots.borrow().len())
}

pub(super) fn take(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    request: BootNamespaceRequest<'_>,
    expected_source: &RetainedBootNamespaceExpectedSource<'_>,
) -> Option<FixtureOwnedReplacementAssessment> {
    let (root_path, validation_count) =
        ROOTS.with(|roots| roots.borrow_mut().pop_front())?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_PATH
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(&root_path)
        .expect("open fixture owned-replacement root");
    let metadata = root
        .metadata()
        .expect("inspect fixture owned-replacement root");
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
    .expect("observe fixture owned replacement");
    let [state] = assessment.states() else {
        panic!("one fixture replacement request returned multiple states")
    };
    Some(FixtureOwnedReplacementAssessment {
        state: *state,
        root: root_path,
        validation_count,
    })
}

pub(super) fn validate_applied(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    parent_components: &[&str],
    evidence: &ValidatedRetainedBootFileReplacement,
    deadline: std::time::Instant,
) -> Option<Result<(), ActiveReblitBootOwnedLeafReplacementError>> {
    let root_path = VALIDATION_ROOTS.with(|roots| roots.borrow_mut().pop_front())?;
    Some(validate_applied_at_root(
        target,
        &root_path,
        parent_components,
        evidence,
        deadline,
    ))
}

fn validate_applied_at_root(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    root_path: &std::path::Path,
    parent_components: &[&str],
    evidence: &ValidatedRetainedBootFileReplacement,
    deadline: std::time::Instant,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_PATH
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(root_path)
        .expect("open fixture owned-replacement validation root");
    let metadata = root
        .metadata()
        .expect("inspect fixture owned-replacement validation root");
    let destination = target.destination();
    if metadata.dev() != destination.raw_device()
        || metadata.ino() != destination.inode()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentRootIdentity,
        );
    }
    let selector = root_path
        .to_str()
        .expect("fixture owned-replacement validation root is UTF-8");
    let anchor = PreparedMountNamespaceAnchor::prepare()
        .expect("prepare current-task namespace anchor for fixture validation");
    let attachment = anchor
        .revalidate()
        .expect("revalidate fixture validation namespace")
        .prepare_task_rooted_attachment(selector)
        .expect("prepare fixture validation attachment");
    let fixture_target = attachment
        .revalidate_against(&anchor)
        .expect("revalidate fixture validation attachment");
    let parent = fixture_target
        .retain_existing_boot_publication_parent_until(
            parent_components,
            deadline,
        )
        .map_err(ActiveReblitBootOwnedLeafReplacementError::PublicationParent)?;
    require_validation_parent_identity(target, &fixture_target, &parent)?;
    parent
        .validate_applied_boot_file_replacement_until(evidence, deadline)
        .map_err(
            ActiveReblitBootOwnedLeafReplacementError::LeafReplacementValidation,
        )
}

fn require_validation_parent_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    fixture_target: &RevalidatedTaskRootedAttachment<'_>,
    parent: &RetainedBootPublicationParent<'_, '_>,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    let destination = target.destination();
    if parent.root_device() != destination.raw_device()
        || parent.root_inode() != destination.inode()
        || parent.root_device() != fixture_target.destination_device()
        || parent.root_inode() != fixture_target.destination_inode()
        || parent.root_mount_id() != fixture_target.destination_mount_id()
    {
        Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentRootIdentity,
        )
    } else {
        Ok(())
    }
}

fn require_installed_assessment(
    target: &RevalidatedTaskRootedAttachment<'_>,
    parent_components: &[&str],
    installed: RetainedBootFilePublicationRequest<'_>,
    assessment: &ValidatedRetainedBootLeafAssessment,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    if assessment.state() != RetainedBootLeafAssessmentState::Exact {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledNotExact {
                found: assessment.state(),
            },
        );
    }
    if assessment.assessment_root_device() != target.destination_device()
        || assessment.assessment_root_inode() != target.destination_inode()
        || assessment.assessment_root_mount_id() != target.destination_mount_id()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentRootIdentity,
        );
    }
    if assessment.canonical_leaf() != installed.canonical_leaf()
        || !assessment
            .parent_components()
            .eq(parent_components.iter().copied())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentPathIdentity,
        );
    }
    if assessment.expected_length() != installed.expected_length()
        || assessment.expected_xxh3() != installed.expected_xxh3()
        || assessment.expected_sha256() != installed.expected_sha256()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentContentIdentity,
        );
    }
    let exact_file = (
        assessment.exact_file_device(),
        assessment.exact_file_inode(),
        assessment.exact_file_mount_id(),
    );
    if !matches!(exact_file, (Some(device), Some(inode), Some(mount_id))
        if device == target.destination_device()
            && inode != 0
            && mount_id == target.destination_mount_id())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentFileIdentity,
        );
    }
    Ok(())
}

fn require_parent_identity(
    target: &RevalidatedTaskRootedAttachment<'_>,
    parent: &RetainedBootPublicationParent<'_, '_>,
    assessment: &ValidatedRetainedBootLeafAssessment,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    if parent.root_device() != target.destination_device()
        || parent.root_inode() != target.destination_inode()
        || parent.root_mount_id() != target.destination_mount_id()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentRootIdentity,
        );
    }
    if assessment.retained_parent_device() != Some(parent.destination_device())
        || assessment.retained_parent_inode() != Some(parent.destination_inode())
        || assessment.retained_parent_mount_id()
            != Some(parent.destination_mount_id())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentIdentityChanged,
        );
    }
    Ok(())
}
