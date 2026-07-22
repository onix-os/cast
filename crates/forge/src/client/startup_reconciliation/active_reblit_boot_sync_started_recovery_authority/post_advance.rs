//! Consuming journal advance and retained successor evidence for promoted
//! ActiveReblit `BootSyncStarted` restart recovery.

use crate::{
    Installation, db,
    client::{
        active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
        startup_gate::ActiveReblitBootSyncStartedCleanupSeal,
    },
    transition_journal::{
        CodecError, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ActiveReblitBootSyncStartedDatabaseEvidence,
    ActiveReblitBootSyncStartedRecoveryAuthority,
    ActiveReblitBootSyncStartedRecoveryAuthorityError,
    ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind,
    inspect_current_database, record_plan_is_exact, require_exact_active_state,
    require_exact_database,
};
use crate::client::startup_reconciliation::activation_namespace::ActiveReblitBootSyncStartedNamespaceProof;

/// Evidence which survives the sole bound advance but grants no second
/// advance. It intentionally implements neither `Clone` nor `Copy`.
pub(in crate::client) struct ActiveReblitBootSyncStartedPostAdvanceAuthority<
    'reservation,
> {
    cleanup_seal: ActiveReblitBootSyncStartedCleanupSeal,
    installation: Installation,
    state_db: db::state::Database,
    started_record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitBootSyncStartedDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitBootSyncStartedNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> ActiveReblitBootSyncStartedRecoveryAuthority<'reservation> {
    /// Consume the source authority through the sole exact caller-supplied
    /// `BootSyncComplete` successor. No retry authority is retained.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        successor: &TransitionRecord,
    ) -> Result<
        (
            TransitionJournalRecordBinding,
            ActiveReblitBootSyncStartedPostAdvanceAuthority<'reservation>,
        ),
        ActiveReblitBootSyncStartedRecordAdvanceError,
    > {
        self.revalidate(journal)?;
        if !exact_boot_sync_complete_successor(
            &self.record,
            successor,
            self.receipt_pair,
            &self.cleanup_seal,
        )? {
            return Err(
                ActiveReblitBootSyncStartedRecordAdvanceError::UnexpectedSuccessor,
            );
        }

        let Self {
            cleanup_seal,
            installation,
            state_db,
            record,
            receipt_pair,
            database,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let cast = installation.retained_mutable_cast_directory()?;
        let successor_binding = journal
            .advance_record_binding(cast, journal_record_binding, successor)
            .map_err(ActiveReblitBootSyncStartedRecordAdvanceError::Storage)?;
        Ok((
            successor_binding,
            ActiveReblitBootSyncStartedPostAdvanceAuthority {
                cleanup_seal,
                installation,
                state_db,
                started_record: record,
                receipt_pair,
                database,
                active_state,
                namespace,
                _active_state_reservation,
            },
        ))
    }
}

impl ActiveReblitBootSyncStartedPostAdvanceAuthority<'_> {
    /// Authenticate the exact successor while the advancing store remains
    /// open and owns its original operation lock.
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
        self.revalidate_successor(
            journal,
            successor_binding,
            successor,
            SuccessorBindingMode::SameStore,
        )
    }

    /// Authenticate the same successor inode after canonical writer reopen.
    pub(in crate::client) fn revalidate_successor_reopened(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
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
    ) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        if !exact_boot_sync_complete_successor(
            &self.started_record,
            successor,
            self.receipt_pair,
            &self.cleanup_seal,
        )
        .map_err(ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Record)?
        {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::UnexpectedSuccessor
                    .into(),
            );
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(
            successor,
            &self.installation,
            &self.active_state,
        )?;
        match binding_mode {
            SuccessorBindingMode::SameStore => {
                self.namespace.revalidate_successor_same_store(
                    &self.installation,
                    journal,
                    successor_binding,
                    &self.started_record,
                    successor,
                )?
            }
            SuccessorBindingMode::Reopened => {
                self.namespace.revalidate_successor_reopened(
                    &self.installation,
                    journal,
                    successor_binding,
                    &self.started_record,
                    successor,
                )?
            }
        }
        let database_after = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(
            successor,
            &self.installation,
            &self.active_state,
        )?;
        if database_before != database_after {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::RouteEvidenceChanged
                    .into(),
            );
        }
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SuccessorBindingMode {
    SameStore,
    Reopened,
}

fn exact_boot_sync_complete_successor(
    started: &TransitionRecord,
    successor: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    cleanup_seal: &ActiveReblitBootSyncStartedCleanupSeal,
) -> Result<bool, CodecError> {
    Ok(record_plan_is_exact(started, receipt_pair, cleanup_seal)
        && started.boot_sync_complete_successor(receipt_pair)? == *successor)
}

fn require_exact_successor_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
    mode: SuccessorBindingMode,
) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
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
        Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::SuccessorRecordBindingChanged
                .into(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitBootSyncStartedRecordAdvanceError {
    #[error("revalidate exact ActiveReblit BootSyncStarted recovery authority before the bound advance")]
    Authority(#[from] ActiveReblitBootSyncStartedRecoveryAuthorityError),
    #[error("validate the caller-supplied ActiveReblit BootSyncComplete successor")]
    Record(#[from] CodecError),
    #[error("the caller-supplied record is not the exact ActiveReblit BootSyncComplete successor")]
    UnexpectedSuccessor,
    #[error("revalidate retained installation before the bound ActiveReblit BootSyncComplete advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActiveReblit BootSyncStarted record")]
    Storage(#[source] StorageError),
}
