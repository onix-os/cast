//! Bound `CommitDecided` to `CommitCleanupComplete` record advance.

use crate::{
    Installation, db,
    client::active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{ActiveReblitCommitCleanupDurableAuthority, ActiveReblitCommitCleanupEffectError};
use super::super::{
    ActiveReblitCommitCleanupAuthorityError, ActiveReblitCommitCleanupAuthorityErrorKind,
    ActiveReblitCommitCleanupCommonEvidence, ActiveReblitCommitCleanupDatabaseEvidence,
    ActiveReblitCommitCleanupRouteEvidence, inspect_current_database, record_plan_is_exact,
    require_exact_active_state, require_exact_database,
};
use crate::client::startup_reconciliation::activation_namespace::DurableActiveReblitCommitCleanupNamespace;

/// Evidence which survives the sole bound advance but grants no second
/// advance. It intentionally implements neither `Clone` nor `Copy`.
pub(in crate::client) struct ActiveReblitCommitCleanupPostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    database: ActiveReblitCommitCleanupDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: DurableActiveReblitCommitCleanupNamespace,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> ActiveReblitCommitCleanupDurableAuthority<'reservation> {
    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.evidence.record
    }

    /// Consume durable cleanup evidence through one exact bound record
    /// advance. No retry authority survives any outcome.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        successor: &TransitionRecord,
    ) -> Result<
        (
            TransitionJournalRecordBinding,
            ActiveReblitCommitCleanupPostAdvanceAuthority<'reservation>,
        ),
        ActiveReblitCommitCleanupRecordAdvanceError,
    > {
        self.revalidate(journal)?;
        if !exact_cleanup_complete_successor(
            &self.evidence.record,
            successor,
            &self.evidence.database.route,
        )? {
            return Err(ActiveReblitCommitCleanupRecordAdvanceError::UnexpectedSuccessor);
        }

        let ActiveReblitCommitCleanupDurableAuthority { evidence, namespace } = self;
        let ActiveReblitCommitCleanupCommonEvidence {
            installation,
            state_db,
            record,
            database,
            active_state,
            journal_record_binding,
            _active_state_reservation,
        } = evidence;
        let cast = installation.retained_mutable_cast_directory()?;
        let successor_binding = journal
            .advance_record_binding(cast, journal_record_binding, successor)
            .map_err(ActiveReblitCommitCleanupRecordAdvanceError::Storage)?;
        Ok((
            successor_binding,
            ActiveReblitCommitCleanupPostAdvanceAuthority {
                installation,
                state_db,
                completed_record: record,
                database,
                active_state,
                namespace,
                _active_state_reservation,
            },
        ))
    }
}

impl ActiveReblitCommitCleanupPostAdvanceAuthority<'_> {
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupEffectError> {
        self.revalidate_successor(
            journal,
            successor_binding,
            successor,
            SuccessorBindingMode::SameStore,
        )
    }

    pub(in crate::client) fn revalidate_successor_reopened(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupEffectError> {
        self.revalidate_successor(
            journal,
            successor_binding,
            successor,
            SuccessorBindingMode::Reopened,
        )
    }

    fn revalidate_successor(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
        binding_mode: SuccessorBindingMode,
    ) -> Result<(), ActiveReblitCommitCleanupEffectError> {
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        let exact = exact_cleanup_complete_successor(
            &self.completed_record,
            successor,
            &self.database.route,
        )
        .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::Record)
        .map_err(ActiveReblitCommitCleanupAuthorityError::from)?;
        if !exact {
            return Err(ActiveReblitCommitCleanupAuthorityError::from(
                ActiveReblitCommitCleanupAuthorityErrorKind::UnexpectedSuccessor,
            )
            .into());
        }
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ActiveReblitCommitCleanupAuthorityError::from)?;
        let database_before = require_exact_database(
            &self.database,
            inspect_current_database(successor, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        self.namespace
            .revalidate(&self.installation, &self.completed_record)?;
        let database_after = require_exact_database(
            &self.database,
            inspect_current_database(successor, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        if database_before != database_after {
            return Err(ActiveReblitCommitCleanupAuthorityError::from(
                ActiveReblitCommitCleanupAuthorityErrorKind::RouteEvidenceChanged,
            )
            .into());
        }
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ActiveReblitCommitCleanupAuthorityError::from)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SuccessorBindingMode {
    SameStore,
    Reopened,
}

fn exact_cleanup_complete_successor(
    completed: &TransitionRecord,
    successor: &TransitionRecord,
    route: &ActiveReblitCommitCleanupRouteEvidence,
) -> Result<bool, CodecError> {
    let completed_pair = completed.boot_publication_receipt_correlation()?;
    let successor_pair = successor.boot_publication_receipt_correlation()?;
    let route_is_preserved = match route {
        ActiveReblitCommitCleanupRouteEvidence::PromotedBoot { pair, .. } => {
            completed_pair == Some(*pair) && successor_pair == Some(*pair)
        }
        ActiveReblitCommitCleanupRouteEvidence::NoBoot { .. } => {
            completed_pair.is_none()
                && successor_pair.is_none()
                && successor.generation == 12
        }
    };
    Ok(record_plan_is_exact(completed, route)
        && successor.operation == Operation::ActiveReblit
        && successor.phase == Phase::CommitCleanupComplete
        && successor.rollback.is_none()
        && successor.candidate.id.is_some()
        && successor.candidate.id == successor.previous.id
        && route_is_preserved
        && successor.generation == completed.generation.checked_add(1).unwrap_or(0)
        && successor.format == completed.format
        && successor.version == completed.version
        && successor.transition_id == completed.transition_id
        && successor.creation_epoch == completed.creation_epoch
        && successor.candidate == completed.candidate
        && successor.previous == completed.previous
        && successor.options == completed.options
        && successor.quarantine_name == completed.quarantine_name)
}

fn require_exact_successor_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
    mode: SuccessorBindingMode,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    let cast = installation.retained_mutable_cast_directory()?;
    let exact = match mode {
        SuccessorBindingMode::SameStore => {
            journal.has_record_store_binding(binding)
                && journal.has_record_binding(cast, binding, successor)?
        }
        SuccessorBindingMode::Reopened => {
            journal.has_reopened_record_binding(cast, binding, successor)?
        }
    };
    if exact {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupAuthorityErrorKind::SuccessorRecordBindingChanged.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupRecordAdvanceError {
    #[error("revalidate exact durable ActiveReblit cleanup authority before the bound advance")]
    Authority(#[from] ActiveReblitCommitCleanupEffectError),
    #[error("validate the derived ActiveReblit CommitCleanupComplete successor")]
    Record(#[from] CodecError),
    #[error("the derived record is not the exact ActiveReblit CommitCleanupComplete successor")]
    UnexpectedSuccessor,
    #[error("revalidate retained installation before the bound cleanup-complete advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActiveReblit CommitDecided record")]
    Storage(#[source] StorageError),
}
