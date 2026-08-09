//! Sealed consumption of previous-restore effect leases.
//!
//! The journal binding is always checked before any retained database or
//! namespace evidence. The `Archived` path builds a recovery identity for the
//! post-archive topology, adopts the archive attempt from the parking name the
//! record itself carries, and makes exactly one compensating move. The
//! `Restored` path makes none. Neither interprets a raw syscall result as the
//! outcome: the namespace is re-read, and only a `Restored` layout counts.

use crate::{
    Installation, db, state,
    transition_journal::{
        CodecError, Phase, RollbackActionOutcome, StorageError, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::{
    UsrRollbackPreviousRestoreApplyEffectLease, UsrRollbackPreviousRestoreAuthorityError,
    UsrRollbackPreviousRestoreAuthorityErrorKind, UsrRollbackPreviousRestoreEffectLease,
    UsrRollbackPreviousRestoreFinishEffectLease, inspect_current_database, previous_restore_plan_is_exact,
    require_exact_database, require_journal_record_binding,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{DatabaseEvidence, UsrRollbackPreviousRestoreNamespaceEffectEvidence},
    startup_recovery::UsrRollbackPreviousRestoreEffectSeal,
};

/// Result of consuming one `Archived` effect lease.
///
/// Only `Applied` retains capability. The other variants are fieldless so a
/// caller cannot retry an uncertain or known-unapplied one-shot attempt.
#[must_use = "a consumed previous-restore apply lease must be handled"]
pub(in crate::client) enum UsrRollbackPreviousRestoreApplyReconciliation<'reservation> {
    Applied(UsrRollbackPreviousRestoreDurableEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

/// Opaque post-effect authority for the exact journal successor.
#[must_use = "reconciled previous-restore evidence still requires exact journal persistence"]
pub(in crate::client) struct UsrRollbackPreviousRestoreDurableEffectAuthority<'reservation> {
    effect: ReconciledPreviousRestoreEffect<'reservation>,
    outcome: RollbackActionOutcome,
}

/// Exact authority-derived `PreviousRestoredToStaging` publication and its new
/// inode binding.
pub(in crate::client) struct UsrRollbackPreviousRestorePublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackPreviousRestorePublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

struct ReconciledPreviousRestoreEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackPreviousRestoreNamespaceEffectEvidence,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackPreviousRestoreApplyEffectLease<'reservation> {
    /// Consume an exact `Archived` lease into one namespace-derived result.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackPreviousRestoreEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreApplyReconciliation<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        // Exact record identity is intentionally the first observation.
        require_journal_record_binding(
            &self.lease.installation,
            journal,
            &self.lease.journal_record_binding,
            &self.lease.record,
        )?;
        self.lease.reconcile_apply_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackPreviousRestoreFinishEffectLease<'reservation> {
    /// Consume an exact `Restored` lease without moving anything.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackPreviousRestoreEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreDurableEffectAuthority<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        require_journal_record_binding(
            &self.lease.installation,
            journal,
            &self.lease.journal_record_binding,
            &self.lease.record,
        )?;
        self.lease.reconcile_finish_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackPreviousRestoreEffectLease<'reservation> {
    fn reconcile_apply_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreApplyReconciliation<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        require_evidence(&self, journal)?;
        self.namespace.require_stable(&self.installation, &self.record)?;

        let attempt = self.perform_restore();
        // The namespace decides, not the syscall report. A failure that still
        // moved the tree is `Applied`; one that did not is `NotApplied`; a
        // namespace that reads as neither is `Ambiguous`.
        let restored = self.namespace.require_restored(&self.installation, &self.record);
        let trailing = require_evidence(&self, journal);

        match (attempt, restored) {
            (Ok(()), Ok(())) => {
                trailing?;
                Ok(UsrRollbackPreviousRestoreApplyReconciliation::Applied(
                    self.into_durable(RollbackActionOutcome::Applied),
                ))
            }
            (Err(_), Ok(())) => {
                // The move landed even though the call reported a failure, so
                // the effect happened exactly once and must be recorded.
                trailing?;
                Ok(UsrRollbackPreviousRestoreApplyReconciliation::Applied(
                    self.into_durable(RollbackActionOutcome::Applied),
                ))
            }
            (Err(_), Err(_)) => Ok(UsrRollbackPreviousRestoreApplyReconciliation::NotApplied),
            (Ok(()), Err(_)) => Ok(UsrRollbackPreviousRestoreApplyReconciliation::Ambiguous),
        }
    }

    fn reconcile_finish_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreDurableEffectAuthority<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        require_evidence(&self, journal)?;
        // Already exact at admission and nothing is performed here, which is
        // precisely what `AlreadySatisfied` records.
        //
        // The parent-sync suffix a completed move normally runs is not
        // reachable from a fresh process: the retained attempt died with the
        // identity that made the move, and adoption needs the archived slot
        // this layout no longer has. It is covered anyway — the very next
        // rollback action is the reverse exchange, whose durability boundary
        // syncs the staging parent and the installation root before it
        // persists.
        self.namespace.require_restored(&self.installation, &self.record)?;
        require_evidence(&self, journal)?;
        Ok(self.into_durable(RollbackActionOutcome::AlreadySatisfied))
    }

    /// Build the recovery identity, adopt the archive attempt from the parking
    /// name the record carries, and make exactly one compensating move.
    fn perform_restore(&self) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
        let restore_error = |source: crate::transition_identity::Error| {
            UsrRollbackPreviousRestoreAuthorityError::from(UsrRollbackPreviousRestoreAuthorityErrorKind::Restore(
                Box::new(source),
            ))
        };
        let candidate = state::Id::from(
            self.record
                .candidate
                .id
                .expect("an exact previous-restore plan names its candidate"),
        );
        let previous = state::Id::from(
            self.record
                .previous
                .id
                .expect("an exact previous-restore plan names its predecessor"),
        );
        let recorded = self
            .record
            .previous_archive_slot
            .as_ref()
            .expect("an exact previous-restore plan carries its parking name");

        let seal = crate::transition_identity::PreviousRestoreRecoverySeal::for_recovery();
        let identity = crate::transition_identity::StatefulTreeIdentity::prepare_previous_restore_recovery(
            &self.installation,
            &self.state_db,
            candidate,
            previous,
            &seal,
        )
        .map_err(restore_error)?;
        identity
            .adopt_previous_archive_attempt(&self.installation, previous, recorded)
            .map_err(restore_error)?;
        identity
            .restore_previous_with_journal(&self.installation, previous, &seal)
            .map_err(|failure| restore_error(failure.into_source()))
    }

    fn into_durable(
        self,
        outcome: RollbackActionOutcome,
    ) -> UsrRollbackPreviousRestoreDurableEffectAuthority<'reservation> {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        UsrRollbackPreviousRestoreDurableEffectAuthority {
            effect: ReconciledPreviousRestoreEffect {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_record_binding,
                _active_state_reservation,
            },
            outcome,
        }
    }
}

impl UsrRollbackPreviousRestoreDurableEffectAuthority<'_> {
    /// Revalidate the complete durable authority without consuming it.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
        let effect = &self.effect;
        require_journal_record_binding(
            &effect.installation,
            journal,
            &effect.journal_record_binding,
            &effect.record,
        )?;
        effect.installation.revalidate_mutable_namespace()?;
        let database = inspect_current_database(&effect.record, &effect.state_db)?;
        require_exact_database(&effect.database, database)?;
        if !previous_restore_plan_is_exact(&effect.record) {
            return Err(UsrRollbackPreviousRestoreAuthorityErrorKind::RestoreEvidenceMismatch.into());
        }
        // Whatever the outcome, the predecessor is in staging by the time this
        // authority exists, so that is the layout to keep proving.
        effect
            .namespace
            .require_restored(&effect.installation, &effect.record)?;
        require_journal_record_binding(
            &effect.installation,
            journal,
            &effect.journal_record_binding,
            &effect.record,
        )?;
        effect.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.effect.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.effect.record
    }

    /// Revalidate, then consume the durable authority through the exact
    /// `PreviousRestoreIntent` to `PreviousRestoredToStaging` boundary. The
    /// caller cannot supply or override the privately fixed outcome.
    pub(in crate::client) fn advance_previous_restored_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestorePublishedRecord, UsrRollbackPreviousRestoreRecordAdvanceError> {
        self.revalidate(journal)?;
        let successor = self
            .effect
            .record
            .rollback_successor(Some(self.outcome))
            .map_err(UsrRollbackPreviousRestoreRecordAdvanceError::Successor)?;
        if successor.phase != Phase::PreviousRestoredToStaging {
            return Err(UsrRollbackPreviousRestoreRecordAdvanceError::UnexpectedSuccessor { phase: successor.phase });
        }
        let cast = self.effect.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self.effect.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackPreviousRestorePublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackPreviousRestoreRecordAdvanceError::Storage { source, successor }),
        }
    }
}

fn require_evidence(
    lease: &UsrRollbackPreviousRestoreEffectLease<'_>,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
    require_journal_record_binding(
        &lease.installation,
        journal,
        &lease.journal_record_binding,
        &lease.record,
    )?;
    lease.installation.revalidate_mutable_namespace()?;
    let database = inspect_current_database(&lease.record, &lease.state_db)?;
    require_exact_database(&lease.database, database)?;
    if !previous_restore_plan_is_exact(&lease.record) {
        return Err(UsrRollbackPreviousRestoreAuthorityErrorKind::RestoreEvidenceMismatch.into());
    }
    lease.installation.revalidate_mutable_namespace()?;
    require_journal_record_binding(
        &lease.installation,
        journal,
        &lease.journal_record_binding,
        &lease.record,
    )?;
    lease.installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackPreviousRestoreRecordAdvanceError {
    #[error("revalidate exact durable previous-restore authority before the bound journal advance")]
    Authority(#[from] UsrRollbackPreviousRestoreAuthorityError),
    #[error("revalidate retained installation before the bound previous-restore journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned previous-restore successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned previous-restore successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound previous-restore journal record")]
    Storage {
        #[source]
        source: StorageError,
        successor: TransitionRecord,
    },
}
