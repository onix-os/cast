//! Restart cleanup reconstructed only from authenticated receipt content.
//!
//! The startup seal, exact cleanup plan, and freshly receipt-validated live
//! targets must all name the same promoted receipt. Only owned replacement and
//! owned-stale entries are admitted. The low-level request is rebuilt from the
//! receipt's length, XXH3, and SHA-256 values plus the current receipt owner;
//! no historical runtime or inode witness participates.

use crate::{
    boot_publication::{
        BootPublicationOutput, BootPublicationReceiptFingerprint,
        BootPublicationRoot,
    },
    client::{
        active_reblit_promoted_boot_cleanup_plan::{
            ActiveReblitPromotedBootCleanupDisposition,
            ActiveReblitPromotedBootCleanupPlan,
            ActiveReblitPromotedBootCleanupPlanEntry,
        },
        startup_gate::ActiveReblitBootSyncStartedCleanupSeal,
    },
    linux_fs::mount_namespace::{
        RetainedBootFileAppliedSidecarCleanupState,
        RetainedBootFileMutationFingerprint, RetainedBootFilePublicationLimits,
        RetainedBootFilePublicationRequest, RetainedBootFileReplacementRequest,
        RetainedBootFileStaleCleanupRequest, RetainedBootFileStaleCleanupState,
    },
};

use super::{
    ActiveReblitBootOwnedCleanupError, ActiveReblitBootOwnedCleanupOutcome,
    OwnedCleanupPath, OwnedCleanupTargetIdentity,
    require_parent_root_identity, split_cleanup_path,
};
use super::super::{
    BootTargetRole, ReceiptValidatedActiveReblitBootPublicationTargets,
    RevalidatedActiveReblitBootPublicationTarget,
    RevalidatedActiveReblitBootPublicationTargets,
};

impl ReceiptValidatedActiveReblitBootPublicationTargets<'_> {
    /// Reconcile and discharge one exact receipt-derived cleanup entry.
    ///
    /// The opaque startup seal cannot be constructed by this adapter. A
    /// matching seal admits only the two owned mutation dispositions. No-op
    /// and borrowed preservation entries are rejected before target routing
    /// or low-level reconciliation.
    pub(in crate::client) fn reconcile_and_cleanup_restart_receipt_entry(
        &self,
        plan: &ActiveReblitPromotedBootCleanupPlan<'_>,
        entry_index: usize,
        cleanup_seal: &ActiveReblitBootSyncStartedCleanupSeal,
    ) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
        let entry = require_restart_cleanup_entry(
            self.promoted_receipt,
            cleanup_seal.promoted_receipt(),
            plan,
            entry_index,
        )?;
        let predecessor = entry.predecessor_output();
        let target = route_restart_cleanup_target(
            self,
            predecessor.root(),
            entry_index,
        )?;
        let path = split_cleanup_path(
            predecessor.relative_path(),
            "restart cleanup",
            entry_index,
        )?;
        let owner = receipt_owner(plan.promoted_receipt());

        match entry.disposition() {
            ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned => {
                let installed = entry.installed_output().ok_or(
                    ActiveReblitBootOwnedCleanupError::RestartEntryShape {
                        entry_index,
                    },
                )?;
                if predecessor.root() != installed.root()
                    || predecessor.relative_path() != installed.relative_path()
                {
                    return Err(
                        ActiveReblitBootOwnedCleanupError::RestartEntryShape {
                            entry_index,
                        },
                    );
                }
                reconcile_restart_replacement_at(
                    target,
                    entry_index,
                    &path,
                    predecessor,
                    installed,
                    owner,
                )
            }
            ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale => {
                if entry.installed_output().is_some() {
                    return Err(
                        ActiveReblitBootOwnedCleanupError::RestartEntryShape {
                            entry_index,
                        },
                    );
                }
                reconcile_restart_stale_at(
                    target,
                    entry_index,
                    &path,
                    predecessor,
                    owner,
                )
            }
            ActiveReblitPromotedBootCleanupDisposition::NoOp
            | ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale => {
                unreachable!("non-mutating cleanup dispositions fail admission")
            }
        }
    }
}

fn require_restart_cleanup_entry<'plan, 'chain>(
    target_receipt: BootPublicationReceiptFingerprint,
    seal_receipt: BootPublicationReceiptFingerprint,
    plan: &'plan ActiveReblitPromotedBootCleanupPlan<'chain>,
    entry_index: usize,
) -> Result<
    &'plan ActiveReblitPromotedBootCleanupPlanEntry<'chain>,
    ActiveReblitBootOwnedCleanupError,
> {
    let entry = plan.entries().get(entry_index).ok_or(
        ActiveReblitBootOwnedCleanupError::RestartEntryIndex {
            entry_index,
            entry_count: plan.entries().len(),
        },
    )?;
    require_restart_receipt_join(
        target_receipt,
        seal_receipt,
        plan.promoted_receipt(),
    )?;
    require_mutating_disposition(entry.disposition(), entry_index)?;
    Ok(entry)
}

fn require_restart_receipt_join(
    target_receipt: BootPublicationReceiptFingerprint,
    seal_receipt: BootPublicationReceiptFingerprint,
    plan_receipt: BootPublicationReceiptFingerprint,
) -> Result<(), ActiveReblitBootOwnedCleanupError> {
    if seal_receipt != plan_receipt {
        return Err(
            ActiveReblitBootOwnedCleanupError::RestartSealReceiptMismatch,
        );
    }
    if target_receipt != plan_receipt {
        return Err(
            ActiveReblitBootOwnedCleanupError::RestartTargetReceiptMismatch,
        );
    }
    Ok(())
}

fn require_mutating_disposition(
    disposition: &ActiveReblitPromotedBootCleanupDisposition,
    entry_index: usize,
) -> Result<(), ActiveReblitBootOwnedCleanupError> {
    if matches!(
        disposition,
        ActiveReblitPromotedBootCleanupDisposition::NoOp
            | ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale
    ) {
        Err(
            ActiveReblitBootOwnedCleanupError::RestartDispositionRefused {
                entry_index,
            },
        )
    } else {
        Ok(())
    }
}

fn route_restart_cleanup_target<'borrow, 'prepared>(
    validated: &'borrow ReceiptValidatedActiveReblitBootPublicationTargets<'prepared>,
    root: BootPublicationRoot,
    entry_index: usize,
) -> Result<
    &'borrow RevalidatedActiveReblitBootPublicationTarget<'prepared>,
    ActiveReblitBootOwnedCleanupError,
> {
    let (target, expected_role) = match (
        validated.aliases_esp,
        &validated.targets,
        root,
    ) {
        (
            true,
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp {
                esp,
            },
            BootPublicationRoot::Esp | BootPublicationRoot::Boot,
        ) => (esp, BootTargetRole::Esp),
        (
            false,
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                esp,
                ..
            },
            BootPublicationRoot::Esp,
        ) => (esp, BootTargetRole::Esp),
        (
            false,
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                xbootldr,
                ..
            },
            BootPublicationRoot::Boot,
        ) => (xbootldr, BootTargetRole::Xbootldr),
        _ => {
            return Err(
                ActiveReblitBootOwnedCleanupError::RestartTargetShape {
                    entry_index,
                },
            );
        }
    };
    if target.role() != expected_role {
        return Err(ActiveReblitBootOwnedCleanupError::RestartTargetRole {
            entry_index,
        });
    }
    Ok(target)
}

fn reconcile_restart_replacement_at(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    entry_index: usize,
    path: &OwnedCleanupPath<'_>,
    predecessor: &BootPublicationOutput,
    installed: &BootPublicationOutput,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    #[cfg(test)]
    if let Some(fixture_target) = super::fixture::take(target) {
        return fixture_target.with_real_target(
            target,
            |attachment, identity, deadline| {
                reconcile_restart_replacement_with_attachment(
                    attachment,
                    identity,
                    deadline,
                    entry_index,
                    path,
                    predecessor,
                    installed,
                    owner,
                )
            },
        );
    }
    reconcile_restart_replacement_with_attachment(
        &target.attachment,
        OwnedCleanupTargetIdentity::from_publication_target(target),
        target.deadline,
        entry_index,
        path,
        predecessor,
        installed,
        owner,
    )
}

#[allow(clippy::too_many_arguments)]
fn reconcile_restart_replacement_with_attachment(
    attachment: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
    target_identity: OwnedCleanupTargetIdentity,
    deadline: std::time::Instant,
    entry_index: usize,
    path: &OwnedCleanupPath<'_>,
    predecessor: &BootPublicationOutput,
    installed: &BootPublicationOutput,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    let parent = attachment
        .retain_existing_boot_publication_parent_until(path.parents(), deadline)
        .map_err(|source| ActiveReblitBootOwnedCleanupError::PublicationParent {
            kind: "restart replacement",
            index: entry_index,
            source,
        })?;
    require_parent_root_identity(
        target_identity,
        &parent,
        "restart replacement",
        entry_index,
    )?;
    let request = replacement_request(path.leaf, predecessor, installed, owner);
    match parent
        .reconcile_replaced_boot_file_sidecar_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline,
        )
        .map_err(|source| {
            ActiveReblitBootOwnedCleanupError::ReplacementReconciliation {
                plan_index: entry_index,
                source,
            }
        })?
    {
        RetainedBootFileAppliedSidecarCleanupState::AlreadyClean => {
            Ok(ActiveReblitBootOwnedCleanupOutcome::AlreadyClean)
        }
        RetainedBootFileAppliedSidecarCleanupState::Pending(recovered) => {
            parent
                .cleanup_replaced_boot_file_sidecar_until(recovered, deadline)
                .map_err(|source| {
                    ActiveReblitBootOwnedCleanupError::ReplacementCleanup {
                        plan_index: entry_index,
                        source,
                    }
                })?;
            Ok(
                ActiveReblitBootOwnedCleanupOutcome::RemovedReplacementRollback,
            )
        }
    }
}

fn reconcile_restart_stale_at(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    entry_index: usize,
    path: &OwnedCleanupPath<'_>,
    predecessor: &BootPublicationOutput,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    #[cfg(test)]
    if let Some(fixture_target) = super::fixture::take(target) {
        return fixture_target.with_real_target(
            target,
            |attachment, identity, deadline| {
                reconcile_restart_stale_with_attachment(
                    attachment,
                    identity,
                    deadline,
                    entry_index,
                    path,
                    predecessor,
                    owner,
                )
            },
        );
    }
    reconcile_restart_stale_with_attachment(
        &target.attachment,
        OwnedCleanupTargetIdentity::from_publication_target(target),
        target.deadline,
        entry_index,
        path,
        predecessor,
        owner,
    )
}

fn reconcile_restart_stale_with_attachment(
    attachment: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
    target_identity: OwnedCleanupTargetIdentity,
    deadline: std::time::Instant,
    entry_index: usize,
    path: &OwnedCleanupPath<'_>,
    predecessor: &BootPublicationOutput,
    owner: RetainedBootFileMutationFingerprint,
) -> Result<ActiveReblitBootOwnedCleanupOutcome, ActiveReblitBootOwnedCleanupError> {
    let parent = attachment
        .retain_existing_boot_publication_parent_until(path.parents(), deadline)
        .map_err(|source| ActiveReblitBootOwnedCleanupError::PublicationParent {
            kind: "restart stale",
            index: entry_index,
            source,
        })?;
    require_parent_root_identity(
        target_identity,
        &parent,
        "restart stale",
        entry_index,
    )?;
    let request = RetainedBootFileStaleCleanupRequest::new(
        output_request(path.leaf, predecessor),
        owner,
    );
    let state = parent
        .reconcile_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline,
        )
        .map_err(|source| {
            ActiveReblitBootOwnedCleanupError::StaleReconciliation {
                delta_index: entry_index,
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
            delta_index: entry_index,
            source,
        })?;
    Ok(ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale)
}

fn replacement_request<'leaf>(
    leaf: &'leaf str,
    predecessor: &BootPublicationOutput,
    installed: &BootPublicationOutput,
    owner: RetainedBootFileMutationFingerprint,
) -> RetainedBootFileReplacementRequest<'leaf> {
    RetainedBootFileReplacementRequest::new(
        output_request(leaf, predecessor),
        output_request(leaf, installed),
        owner,
    )
}

fn output_request<'leaf>(
    leaf: &'leaf str,
    output: &BootPublicationOutput,
) -> RetainedBootFilePublicationRequest<'leaf> {
    RetainedBootFilePublicationRequest::new(
        leaf,
        output.length(),
        output.xxh3().as_u128(),
        *output.content_sha256().as_bytes(),
    )
}

fn receipt_owner(
    receipt: BootPublicationReceiptFingerprint,
) -> RetainedBootFileMutationFingerprint {
    RetainedBootFileMutationFingerprint::new(*receipt.as_bytes())
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
