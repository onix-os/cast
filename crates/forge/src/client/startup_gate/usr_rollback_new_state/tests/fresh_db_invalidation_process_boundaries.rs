//! Real SQLite and journal-update boundaries for invalidation process death.

use crate::{
    db::state::{
        ExactFreshTransitionRemovalBoundary, arm_exact_fresh_transition_removal_callback,
    },
    transition_journal::{
        JournalUpdateDurabilityBoundary, arm_journal_update_durability_callback,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDbInvalidationProcessBoundary {
    PreimageValidated,
    ProvenanceDeleted,
    SelectionsDeleted,
    StateRowDeletedBeforeCommit,
    CommitReturnedBeforeReconciliation,
    TemporaryFullySynced,
    CanonicalExchanged,
    UpdateFirstDirectorySynced,
    DisplacedUnlinked,
    UpdateFinalDirectorySynced,
}

impl FreshDbInvalidationProcessBoundary {
    pub(super) const DATABASE: [Self; 5] = [
        Self::PreimageValidated,
        Self::ProvenanceDeleted,
        Self::SelectionsDeleted,
        Self::StateRowDeletedBeforeCommit,
        Self::CommitReturnedBeforeReconciliation,
    ];

    pub(super) const JOURNAL: [Self; 5] = [
        Self::TemporaryFullySynced,
        Self::CanonicalExchanged,
        Self::UpdateFirstDirectorySynced,
        Self::DisplacedUnlinked,
        Self::UpdateFinalDirectorySynced,
    ];

    pub(super) const ALL: [Self; 10] = [
        Self::PreimageValidated,
        Self::ProvenanceDeleted,
        Self::SelectionsDeleted,
        Self::StateRowDeletedBeforeCommit,
        Self::CommitReturnedBeforeReconciliation,
        Self::TemporaryFullySynced,
        Self::CanonicalExchanged,
        Self::UpdateFirstDirectorySynced,
        Self::DisplacedUnlinked,
        Self::UpdateFinalDirectorySynced,
    ];

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "preimage-validated" => Self::PreimageValidated,
            "provenance-deleted" => Self::ProvenanceDeleted,
            "selections-deleted" => Self::SelectionsDeleted,
            "state-row-deleted-before-commit" => Self::StateRowDeletedBeforeCommit,
            "commit-returned-before-reconciliation" => Self::CommitReturnedBeforeReconciliation,
            "temporary-fully-synced" => Self::TemporaryFullySynced,
            "canonical-exchanged" => Self::CanonicalExchanged,
            "update-first-directory-synced" => Self::UpdateFirstDirectorySynced,
            "displaced-unlinked" => Self::DisplacedUnlinked,
            "update-final-directory-synced" => Self::UpdateFinalDirectorySynced,
            other => panic!("invalid fresh-database invalidation process boundary {other:?}"),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PreimageValidated => "preimage-validated",
            Self::ProvenanceDeleted => "provenance-deleted",
            Self::SelectionsDeleted => "selections-deleted",
            Self::StateRowDeletedBeforeCommit => "state-row-deleted-before-commit",
            Self::CommitReturnedBeforeReconciliation => "commit-returned-before-reconciliation",
            Self::TemporaryFullySynced => "temporary-fully-synced",
            Self::CanonicalExchanged => "canonical-exchanged",
            Self::UpdateFirstDirectorySynced => "update-first-directory-synced",
            Self::DisplacedUnlinked => "displaced-unlinked",
            Self::UpdateFinalDirectorySynced => "update-final-directory-synced",
        }
    }

    pub(super) fn is_database(self) -> bool {
        Self::DATABASE.contains(&self)
    }

    pub(super) fn database_commit_survives(self) -> bool {
        self == Self::CommitReturnedBeforeReconciliation
    }

    pub(super) fn canonical_is_source(self) -> bool {
        self.is_database() || self == Self::TemporaryFullySynced
    }

    pub(super) fn temporary_contents(self) -> Option<TemporaryRecordContents> {
        match self {
            Self::TemporaryFullySynced => Some(TemporaryRecordContents::Successor),
            Self::CanonicalExchanged | Self::UpdateFirstDirectorySynced => {
                Some(TemporaryRecordContents::Source)
            }
            Self::PreimageValidated
            | Self::ProvenanceDeleted
            | Self::SelectionsDeleted
            | Self::StateRowDeletedBeforeCommit
            | Self::CommitReturnedBeforeReconciliation
            | Self::DisplacedUnlinked
            | Self::UpdateFinalDirectorySynced => None,
        }
    }

    pub(super) fn arm(self, callback: fn()) {
        match self {
            Self::PreimageValidated => arm_exact_fresh_transition_removal_callback(
                ExactFreshTransitionRemovalBoundary::PreimageValidated,
                callback,
            ),
            Self::ProvenanceDeleted => arm_exact_fresh_transition_removal_callback(
                ExactFreshTransitionRemovalBoundary::ProvenanceDeleted,
                callback,
            ),
            Self::SelectionsDeleted => arm_exact_fresh_transition_removal_callback(
                ExactFreshTransitionRemovalBoundary::SelectionsDeleted,
                callback,
            ),
            Self::StateRowDeletedBeforeCommit => arm_exact_fresh_transition_removal_callback(
                ExactFreshTransitionRemovalBoundary::StateRowDeletedBeforeCommit,
                callback,
            ),
            Self::CommitReturnedBeforeReconciliation => arm_exact_fresh_transition_removal_callback(
                ExactFreshTransitionRemovalBoundary::CommitReturnedBeforeReconciliation,
                callback,
            ),
            Self::TemporaryFullySynced => arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::TemporaryFullySynced,
                callback,
            ),
            Self::CanonicalExchanged => arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::CanonicalExchanged,
                callback,
            ),
            Self::UpdateFirstDirectorySynced => arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced,
                callback,
            ),
            Self::DisplacedUnlinked => arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::DisplacedUnlinked,
                callback,
            ),
            Self::UpdateFinalDirectorySynced => arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
                callback,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TemporaryRecordContents {
    Source,
    Successor,
}
