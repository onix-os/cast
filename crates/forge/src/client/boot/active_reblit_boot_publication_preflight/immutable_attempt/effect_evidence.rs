//! Non-cloneable, plan-ordered evidence for classified desired-output effects.

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaAction,
    linux_fs::mount_namespace::{
        RetainedBootFileMutationFingerprint, RetainedBootFilePublicationOutcome,
        ValidatedRetainedBootFilePublication,
        ValidatedRetainedBootFileReplacement,
    },
};

/// Exact effect evidence for one desired plan output.
///
/// Replacement authority is owned directly by its variant and therefore
/// cannot be cloned, detached, or silently projected into scalar evidence.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) enum ValidatedActiveReblitBootPublicationEffect {
    Published {
        plan_index: usize,
        evidence: ValidatedRetainedBootFilePublication,
    },
    RetainedOwned {
        plan_index: usize,
        evidence: ValidatedRetainedBootFilePublication,
    },
    PreservedBorrowed {
        plan_index: usize,
        evidence: ValidatedRetainedBootFilePublication,
    },
    ReplacedOwned {
        plan_index: usize,
        evidence: ValidatedRetainedBootFileReplacement,
    },
}

impl ValidatedActiveReblitBootPublicationEffect {
    pub(in crate::client) const fn plan_index(&self) -> usize {
        match self {
            Self::Published { plan_index, .. }
            | Self::RetainedOwned { plan_index, .. }
            | Self::PreservedBorrowed { plan_index, .. }
            | Self::ReplacedOwned { plan_index, .. } => *plan_index,
        }
    }

    pub(in crate::client) const fn action(&self) -> ActiveReblitBootPublicationDeltaAction {
        match self {
            Self::Published { .. } => ActiveReblitBootPublicationDeltaAction::PublishDesired,
            Self::RetainedOwned { .. } => {
                ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
            }
            Self::PreservedBorrowed { .. } => {
                ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired
            }
            Self::ReplacedOwned { .. } => {
                ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired
            }
        }
    }

    pub(in crate::client) const fn length(&self) -> u64 {
        match self {
            Self::Published { evidence, .. }
            | Self::RetainedOwned { evidence, .. }
            | Self::PreservedBorrowed { evidence, .. } => evidence.length(),
            Self::ReplacedOwned { evidence, .. } => evidence.replacement_length(),
        }
    }

    pub(in crate::client) const fn xxh3(&self) -> u128 {
        match self {
            Self::Published { evidence, .. }
            | Self::RetainedOwned { evidence, .. }
            | Self::PreservedBorrowed { evidence, .. } => evidence.xxh3(),
            Self::ReplacedOwned { evidence, .. } => evidence.replacement_xxh3(),
        }
    }

    pub(in crate::client) const fn sha256(&self) -> [u8; 32] {
        match self {
            Self::Published { evidence, .. }
            | Self::RetainedOwned { evidence, .. }
            | Self::PreservedBorrowed { evidence, .. } => evidence.sha256(),
            Self::ReplacedOwned { evidence, .. } => evidence.replacement_sha256(),
        }
    }

    pub(in crate::client) const fn installed_length(&self) -> Option<u64> {
        match self {
            Self::ReplacedOwned { evidence, .. } => Some(evidence.installed_length()),
            _ => None,
        }
    }

    pub(in crate::client) const fn immutable_outcome(
        &self,
    ) -> Option<RetainedBootFilePublicationOutcome> {
        match self {
            Self::Published { evidence, .. }
            | Self::RetainedOwned { evidence, .. }
            | Self::PreservedBorrowed { evidence, .. } => Some(evidence.outcome()),
            Self::ReplacedOwned { .. } => None,
        }
    }

    pub(in crate::client) const fn immutable_evidence(
        &self,
    ) -> Option<&ValidatedRetainedBootFilePublication> {
        match self {
            Self::Published { evidence, .. }
            | Self::RetainedOwned { evidence, .. }
            | Self::PreservedBorrowed { evidence, .. } => Some(evidence),
            Self::ReplacedOwned { .. } => None,
        }
    }

    pub(in crate::client) const fn installed_xxh3(&self) -> Option<u128> {
        match self {
            Self::ReplacedOwned { evidence, .. } => Some(evidence.installed_xxh3()),
            _ => None,
        }
    }

    pub(in crate::client) const fn installed_sha256(&self) -> Option<[u8; 32]> {
        match self {
            Self::ReplacedOwned { evidence, .. } => Some(evidence.installed_sha256()),
            _ => None,
        }
    }

    pub(in crate::client) const fn replacement_owner(
        &self,
    ) -> Option<RetainedBootFileMutationFingerprint> {
        match self {
            Self::ReplacedOwned { evidence, .. } => Some(evidence.owner()),
            _ => None,
        }
    }

    pub(in crate::client) fn replacement_authority(
        &self,
    ) -> Option<&ValidatedRetainedBootFileReplacement> {
        match self {
            Self::ReplacedOwned { evidence, .. } => Some(evidence),
            _ => None,
        }
    }

    pub(in crate::client) fn owner_matches_receipt(
        &self,
        receipt: BootPublicationReceiptFingerprint,
    ) -> bool {
        self.replacement_owner().is_none_or(|owner| owner.as_bytes() == *receipt.as_bytes())
    }
}
