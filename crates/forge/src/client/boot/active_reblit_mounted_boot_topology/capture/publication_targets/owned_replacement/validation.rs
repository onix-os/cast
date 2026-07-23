//! Borrowed validation of one already-applied receipt-owned replacement.

use crate::{
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublication,
        active_reblit_publication_plan::ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
    },
    linux_fs::mount_namespace::{
        RetainedBootPublicationParent, ValidatedRetainedBootFileReplacement,
    },
};

use super::{
    ActiveReblitBootOwnedLeafReplacementError, BoundReplacementPath,
    RevalidatedActiveReblitBootPublicationTarget, split_bound_replacement_path,
};

impl RevalidatedActiveReblitBootPublicationTarget<'_> {
    /// Borrow and freshly validate one applied replacement against its exact
    /// plan path and opaque retained target.
    ///
    /// This operation performs no namespace mutation and neither consumes nor
    /// recreates the low-level replacement authority.
    pub(in crate::client) fn validate_applied_owned_leaf_replacement(
        &self,
        plan_index: usize,
        output: &BoundActiveReblitBlsPublication<'_, '_>,
        evidence: &ValidatedRetainedBootFileReplacement,
    ) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
        if output.mode() != ACTIVE_REBLIT_BOOT_OUTPUT_MODE {
            return Err(ActiveReblitBootOwnedLeafReplacementError::PublicationMode {
                plan_index,
                found: output.mode(),
            });
        }
        let relative_path = output.relative_path().to_str().ok_or(
            ActiveReblitBootOwnedLeafReplacementError::NonUtf8Path { plan_index },
        )?;
        let path = split_bound_replacement_path(relative_path, plan_index)?;
        require_evidence_matches_plan(plan_index, output, &path, evidence)?;

        #[cfg(test)]
        if let Some(result) = super::fixture::validate_applied(
            self,
            path.parents(),
            evidence,
            self.deadline,
        ) {
            return result;
        }

        let parent = self
            .attachment
            .retain_existing_boot_publication_parent_until(
                path.parents(),
                self.deadline,
            )
            .map_err(ActiveReblitBootOwnedLeafReplacementError::PublicationParent)?;
        require_parent_root_identity(self, &parent)?;
        parent
            .validate_applied_boot_file_replacement_until(evidence, self.deadline)
            .map_err(
                ActiveReblitBootOwnedLeafReplacementError::LeafReplacementValidation,
            )
    }
}

fn require_evidence_matches_plan(
    plan_index: usize,
    output: &BoundActiveReblitBlsPublication<'_, '_>,
    path: &BoundReplacementPath<'_>,
    evidence: &ValidatedRetainedBootFileReplacement,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    if evidence.canonical_leaf() != path.leaf {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::ReplacementLeafIdentity,
        );
    }
    if evidence.replacement_length() != output.expected_length()
        || evidence.replacement_xxh3() != output.expected_digest()
        || evidence.replacement_sha256()
            != *output.expected_content_identity().as_bytes()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::ReplacementPlanIdentity {
                plan_index,
            },
        );
    }
    if evidence.installed_file_inode() == 0
        || evidence.replacement_file_inode() == 0
        || evidence.installed_file_inode() == evidence.replacement_file_inode()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::ReplacementFileIdentity,
        );
    }
    Ok(())
}

pub(super) fn require_parent_root_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    parent: &RetainedBootPublicationParent<'_, '_>,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    let destination = target.destination();
    if parent.root_device() != destination.raw_device()
        || parent.root_inode() != destination.inode()
        || parent.root_mount_id() != target.mount_id()
    {
        Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentRootIdentity,
        )
    } else {
        Ok(())
    }
}
