//! Receipt-sealed cleanup through one opaque retained boot target.
//!
//! Both operations first retain and identity-check the exact existing parent
//! below this target, then delegate reconciliation and mutation to the
//! descriptor-rooted replacement primitives under the target's original
//! deadline. Callers cannot construct a low-level cleanup request or recover a
//! descriptor. Applied replacement cleanup borrows the historical authority:
//! reconciliation reconstructs a separate, exact authority and only that
//! reconstructed value is consumed, so the original evidence remains retained
//! by the promoted publication.

use thiserror::Error;

use crate::{
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublication,
        active_reblit_boot_publication_preflight::ActiveReblitBootPromotedCleanupSeal,
        active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaExpected,
        active_reblit_publication_plan::ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
    },
    linux_fs::mount_namespace::{
        RevalidatedTaskRootedAttachment,
        RetainedBootFileAppliedSidecarCleanupState,
        RetainedBootFileMutationFingerprint, RetainedBootFilePublicationLimits,
        RetainedBootFilePublicationRequest, RetainedBootFileReplacementError,
        RetainedBootFileReplacementRequest, RetainedBootFileStaleCleanupRequest,
        RetainedBootFileStaleCleanupState, RetainedBootPublicationParent,
        RetainedBootPublicationParentError, ValidatedRetainedBootFileReplacement,
    },
};

use super::RevalidatedActiveReblitBootPublicationTarget;

#[cfg(test)]
#[path = "owned_cleanup/fixture.rs"]
mod fixture;

#[cfg(test)]
pub(in crate::client) use fixture::{
    FixtureOwnedCleanupTargetGuard, arm_fixture_owned_cleanup_targets,
    fixture_owned_cleanup_targets_remaining,
};

/// Exact result of one sealed post-promotion cleanup operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootOwnedCleanupOutcome {
    RemovedReplacementRollback,
    RemovedOwnedStale,
    AlreadyClean,
}

/// Failure while reconciling and discharging receipt-owned boot cleanup.
///
/// Every variant contains inert diagnostics only. No descriptor, low-level
/// request, recovered cleanup authority, or retry callback escapes.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootOwnedCleanupError {
    #[error("replacement plan output {plan_index} has unsupported mode {found:o}")]
    ReplacementMode { plan_index: usize, found: u32 },
    #[error("replacement plan output {plan_index} path is not UTF-8")]
    ReplacementNonUtf8Path { plan_index: usize },
    #[error("{kind} entry {index} path has no retained parent chain")]
    MissingParent { kind: &'static str, index: usize },
    #[error("{kind} entry {index} path contains an invalid component")]
    InvalidPathComponent { kind: &'static str, index: usize },
    #[error("{kind} entry {index} path exceeds the 15-component parent ceiling")]
    ParentDepth { kind: &'static str, index: usize },
    #[error("replacement authority for plan output {plan_index} is not bound to the exact plan leaf")]
    ReplacementLeafIdentity { plan_index: usize },
    #[error("replacement authority for plan output {plan_index} is not bound to the exact desired bytes")]
    ReplacementDesiredIdentity { plan_index: usize },
    #[error("replacement authority for plan output {plan_index} contains identical predecessor and successor bytes")]
    IdenticalReplacementContent { plan_index: usize },
    #[error("replacement authority for plan output {plan_index} contains an invalid or aliased inode pair")]
    ReplacementFileIdentity { plan_index: usize },
    #[error("replacement authority for plan output {plan_index} belongs to a different receipt")]
    ReplacementOwnerMismatch { plan_index: usize },
    #[error("retain the exact existing {kind} parent for entry {index}")]
    PublicationParent {
        kind: &'static str,
        index: usize,
        #[source]
        source: RetainedBootPublicationParentError,
    },
    #[error("the retained {kind} parent for entry {index} is not bound to this exact target root")]
    PublicationParentRootIdentity { kind: &'static str, index: usize },
    #[error("reconcile the applied replacement rollback for plan output {plan_index}")]
    ReplacementReconciliation {
        plan_index: usize,
        #[source]
        source: RetainedBootFileReplacementError,
    },
    #[error("reconciled replacement authority differs from the retained historical authority for plan output {plan_index}")]
    ReplacementAuthorityMismatch { plan_index: usize },
    #[error("remove the reconciled applied replacement rollback for plan output {plan_index}")]
    ReplacementCleanup {
        plan_index: usize,
        #[source]
        source: RetainedBootFileReplacementError,
    },
    #[error("reconcile the predecessor-owned stale leaf for classified entry {delta_index}")]
    StaleReconciliation {
        delta_index: usize,
        #[source]
        source: RetainedBootFileReplacementError,
    },
    #[error("remove the reconciled predecessor-owned stale leaf for classified entry {delta_index}")]
    StaleCleanup {
        delta_index: usize,
        #[source]
        source: RetainedBootFileReplacementError,
    },
}

impl RevalidatedActiveReblitBootPublicationTarget<'_> {
    /// Reconcile and remove one applied replacement rollback sidecar.
    ///
    /// The historical replacement evidence is borrowed and stays owned by the
    /// promoted aggregate. Only freshly reconstructed cleanup authority is
    /// consumed by the low-level unlink primitive.
    pub(in crate::client) fn reconcile_and_cleanup_promoted_owned_replacement(
        &self,
        cleanup_seal: &ActiveReblitBootPromotedCleanupSeal,
        plan_index: usize,
        output: &BoundActiveReblitBlsPublication<'_, '_>,
        historical: &ValidatedRetainedBootFileReplacement,
    ) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
        let path = validate_historical_replacement(
            plan_index,
            output,
            historical,
            cleanup_owner(cleanup_seal),
        )?;
        #[cfg(test)]
        if let Some(fixture_target) = fixture::take(self) {
            return fixture_target.reconcile_and_cleanup_replacement(
                self,
                plan_index,
                &path,
                historical,
            );
        }
        reconcile_and_cleanup_replacement_at(
            &self.attachment,
            OwnedCleanupTargetIdentity::from_publication_target(self),
            self.deadline,
            plan_index,
            &path,
            historical,
        )
    }

    /// Reconcile and remove one exact predecessor-owned stale leaf.
    ///
    /// The promoted seal supplies the non-forgeable receipt owner used for the
    /// deterministic detached cleanup name. Reconciliation admits canonical,
    /// already-detached, and already-clean states only.
    pub(in crate::client) fn reconcile_and_cleanup_promoted_owned_stale(
        &self,
        cleanup_seal: &ActiveReblitBootPromotedCleanupSeal,
        delta_index: usize,
        relative_path: &str,
        expected: ActiveReblitBootPublicationDeltaExpected,
    ) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
        let path = split_cleanup_path(relative_path, "stale", delta_index)?;
        let owner = cleanup_owner(cleanup_seal);
        #[cfg(test)]
        if let Some(fixture_target) = fixture::take(self) {
            return fixture_target.reconcile_and_cleanup_stale(
                self,
                delta_index,
                &path,
                expected,
                owner,
            );
        }
        reconcile_and_cleanup_stale_at(
            &self.attachment,
            OwnedCleanupTargetIdentity::from_publication_target(self),
            self.deadline,
            delta_index,
            &path,
            expected,
            owner,
        )
    }
}

fn reconcile_and_cleanup_replacement_at(
    attachment: &RevalidatedTaskRootedAttachment<'_>,
    target_identity: OwnedCleanupTargetIdentity,
    deadline: std::time::Instant,
    plan_index: usize,
    path: &OwnedCleanupPath<'_>,
    historical: &ValidatedRetainedBootFileReplacement,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    let parent = attachment
        .retain_existing_boot_publication_parent_until(path.parents(), deadline)
        .map_err(|source| ActiveReblitBootOwnedCleanupError::PublicationParent {
            kind: "replacement",
            index: plan_index,
            source,
        })?;
    require_parent_root_identity(
        target_identity,
        &parent,
        "replacement",
        plan_index,
    )?;

    let request = historical_replacement_request(path.leaf, historical);
    match parent
        .reconcile_replaced_boot_file_sidecar_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline,
        )
        .map_err(|source| {
            ActiveReblitBootOwnedCleanupError::ReplacementReconciliation {
                plan_index,
                source,
            }
        })?
    {
        RetainedBootFileAppliedSidecarCleanupState::AlreadyClean => {
            Ok(ActiveReblitBootOwnedCleanupOutcome::AlreadyClean)
        }
        RetainedBootFileAppliedSidecarCleanupState::Pending(recovered) => {
            if &recovered != historical {
                return Err(
                    ActiveReblitBootOwnedCleanupError::ReplacementAuthorityMismatch {
                        plan_index,
                    },
                );
            }
            parent
                .cleanup_replaced_boot_file_sidecar_until(recovered, deadline)
                .map_err(|source| {
                    ActiveReblitBootOwnedCleanupError::ReplacementCleanup {
                        plan_index,
                        source,
                    }
                })?;
            Ok(
                ActiveReblitBootOwnedCleanupOutcome::RemovedReplacementRollback,
            )
        }
    }
}

fn reconcile_and_cleanup_stale_at(
    attachment: &RevalidatedTaskRootedAttachment<'_>,
    target_identity: OwnedCleanupTargetIdentity,
    deadline: std::time::Instant,
    delta_index: usize,
    path: &OwnedCleanupPath<'_>,
    expected: ActiveReblitBootPublicationDeltaExpected,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    let parent = attachment
        .retain_existing_boot_publication_parent_until(path.parents(), deadline)
        .map_err(|source| ActiveReblitBootOwnedCleanupError::PublicationParent {
            kind: "stale",
            index: delta_index,
            source,
        })?;
    require_parent_root_identity(target_identity, &parent, "stale", delta_index)?;

    let stale = RetainedBootFilePublicationRequest::new(
        path.leaf,
        expected.length(),
        expected.checksum(),
        *expected.content_identity().as_bytes(),
    );
    let request = RetainedBootFileStaleCleanupRequest::new(stale, owner);
    let state = parent
        .reconcile_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline,
        )
        .map_err(|source| {
            ActiveReblitBootOwnedCleanupError::StaleReconciliation {
                delta_index,
                source,
            }
        })?;
    let recovered = match state {
        RetainedBootFileStaleCleanupState::AlreadyClean => {
            return Ok(ActiveReblitBootOwnedCleanupOutcome::AlreadyClean);
        }
        RetainedBootFileStaleCleanupState::Canonical(recovered)
        | RetainedBootFileStaleCleanupState::Detached(recovered) => recovered,
    };
    parent
        .cleanup_authenticated_stale_boot_file_until(recovered, deadline)
        .map_err(|source| ActiveReblitBootOwnedCleanupError::StaleCleanup {
            delta_index,
            source,
        })?;
    Ok(ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale)
}

fn validate_historical_replacement<'path>(
    plan_index: usize,
    output: &'path BoundActiveReblitBlsPublication<'_, '_>,
    historical: &ValidatedRetainedBootFileReplacement,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<OwnedCleanupPath<'path>, ActiveReblitBootOwnedCleanupError> {
    if output.mode() != ACTIVE_REBLIT_BOOT_OUTPUT_MODE {
        return Err(ActiveReblitBootOwnedCleanupError::ReplacementMode {
            plan_index,
            found: output.mode(),
        });
    }
    let relative_path = output.relative_path().to_str().ok_or(
        ActiveReblitBootOwnedCleanupError::ReplacementNonUtf8Path {
            plan_index,
        },
    )?;
    let path = split_cleanup_path(relative_path, "replacement", plan_index)?;
    if historical.canonical_leaf() != path.leaf {
        return Err(ActiveReblitBootOwnedCleanupError::ReplacementLeafIdentity {
            plan_index,
        });
    }
    if historical.replacement_length() != output.expected_length()
        || historical.replacement_xxh3() != output.expected_digest()
        || historical.replacement_sha256()
            != *output.expected_content_identity().as_bytes()
    {
        return Err(
            ActiveReblitBootOwnedCleanupError::ReplacementDesiredIdentity {
                plan_index,
            },
        );
    }
    if historical.installed_length() == historical.replacement_length()
        && historical.installed_xxh3() == historical.replacement_xxh3()
        && historical.installed_sha256() == historical.replacement_sha256()
    {
        return Err(
            ActiveReblitBootOwnedCleanupError::IdenticalReplacementContent {
                plan_index,
            },
        );
    }
    if historical.installed_file_inode() == 0
        || historical.replacement_file_inode() == 0
        || historical.installed_file_inode()
            == historical.replacement_file_inode()
    {
        return Err(
            ActiveReblitBootOwnedCleanupError::ReplacementFileIdentity {
                plan_index,
            },
        );
    }
    if historical.owner() != owner {
        return Err(
            ActiveReblitBootOwnedCleanupError::ReplacementOwnerMismatch {
                plan_index,
            },
        );
    }
    Ok(path)
}

fn historical_replacement_request<'leaf>(
    leaf: &'leaf str,
    historical: &ValidatedRetainedBootFileReplacement,
) -> RetainedBootFileReplacementRequest<'leaf> {
    let installed = RetainedBootFilePublicationRequest::new(
        leaf,
        historical.installed_length(),
        historical.installed_xxh3(),
        historical.installed_sha256(),
    );
    let replacement = RetainedBootFilePublicationRequest::new(
        leaf,
        historical.replacement_length(),
        historical.replacement_xxh3(),
        historical.replacement_sha256(),
    );
    RetainedBootFileReplacementRequest::new(
        installed,
        replacement,
        historical.owner(),
    )
}

fn cleanup_owner(
    seal: &ActiveReblitBootPromotedCleanupSeal,
) -> RetainedBootFileMutationFingerprint {
    RetainedBootFileMutationFingerprint::new(
        *seal.promoted_receipt().as_bytes(),
    )
}

fn require_parent_root_identity(
    target: OwnedCleanupTargetIdentity,
    parent: &RetainedBootPublicationParent<'_, '_>,
    kind: &'static str,
    index: usize,
) -> Result<(), ActiveReblitBootOwnedCleanupError> {
    if parent.root_device() != target.device
        || parent.root_inode() != target.inode
        || parent.root_mount_id() != target.mount_id
    {
        Err(
            ActiveReblitBootOwnedCleanupError::PublicationParentRootIdentity {
                kind,
                index,
            },
        )
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct OwnedCleanupTargetIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

impl OwnedCleanupTargetIdentity {
    fn from_publication_target(
        target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    ) -> Self {
        let destination = target.destination();
        Self {
            device: destination.raw_device(),
            inode: destination.inode(),
            mount_id: target.mount_id(),
        }
    }

    fn from_attachment(target: &RevalidatedTaskRootedAttachment<'_>) -> Self {
        Self {
            device: target.destination_device(),
            inode: target.destination_inode(),
            mount_id: target.destination_mount_id(),
        }
    }
}

struct OwnedCleanupPath<'path> {
    parent_components: [&'path str; 15],
    parent_count: usize,
    leaf: &'path str,
}

impl<'path> OwnedCleanupPath<'path> {
    fn parents(&self) -> &[&'path str] {
        &self.parent_components[..self.parent_count]
    }
}

fn split_cleanup_path<'path>(
    path: &'path str,
    kind: &'static str,
    index: usize,
) -> Result<OwnedCleanupPath<'path>, ActiveReblitBootOwnedCleanupError> {
    let mut components = path.split('/');
    let mut prior = components.next().ok_or(
        ActiveReblitBootOwnedCleanupError::InvalidPathComponent {
            kind,
            index,
        },
    )?;
    require_component(prior, kind, index)?;
    let mut parent_components = [""; 15];
    let mut parent_count = 0usize;
    for component in components {
        require_component(component, kind, index)?;
        if parent_count == parent_components.len() {
            return Err(ActiveReblitBootOwnedCleanupError::ParentDepth {
                kind,
                index,
            });
        }
        parent_components[parent_count] = prior;
        parent_count += 1;
        prior = component;
    }
    if parent_count == 0 {
        return Err(ActiveReblitBootOwnedCleanupError::MissingParent {
            kind,
            index,
        });
    }
    Ok(OwnedCleanupPath {
        parent_components,
        parent_count,
        leaf: prior,
    })
}

fn require_component(
    component: &str,
    kind: &'static str,
    index: usize,
) -> Result<(), ActiveReblitBootOwnedCleanupError> {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.len() > 255
        || component.as_bytes().contains(&0)
    {
        Err(ActiveReblitBootOwnedCleanupError::InvalidPathComponent {
            kind,
            index,
        })
    } else {
        Ok(())
    }
}
