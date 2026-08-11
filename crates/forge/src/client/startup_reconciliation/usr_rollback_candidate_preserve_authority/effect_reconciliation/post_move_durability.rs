//! Authority bridge into exact NewState candidate post-move durability.
//!
//! Applied movement and already-preserved Finish evidence retain distinct
//! origins, but both consume the same namespace suffix. The origin is fixed
//! internally and remains private to the resulting opaque authority.

use crate::{
    Installation, db,
    transition_journal::{
        RollbackActionOutcome, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

mod persistence;

pub(in crate::client) use persistence::UsrRollbackCandidatePreserveRecordAdvanceError;
#[cfg(test)]
pub(in crate::client) use persistence::arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence;

use super::super::UsrRollbackCandidatePreserveAuthorityErrorKind;
use super::{
    ReconciledNewStateCandidatePreserveEffect, UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority,
    require_post_effect_evidence, require_pre_effect_evidence,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        DatabaseEvidence, UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority,
        UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority,
        UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveFinishAuthority,
        UsrRollbackCandidatePreserveTopology,
        activation_namespace::{
            UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace,
            UsrRollbackNewStateCandidatePreserveDurableNamespace,
        },
        usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
    },
    startup_recovery::{UsrRollbackCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal},
};

/// Consuming Finish selection for the post-move durability checkpoint.
///
/// Unsupported operation families carry no namespace or authority capability.
#[must_use = "candidate-preservation Finish durability selection must be handled"]
pub(in crate::client) enum UsrRollbackCandidatePreserveFinishDurabilitySelection<'reservation> {
    NewState(UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
    Archived(UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
    ActiveReblit(UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
    /// Nothing was ever staged: no effect to revalidate, only the advance.
    /// Boxed: this enum travels the deep recovery chain by value, and widening
    /// it overflows the endpoint walk's default test stack.
    ArchivedNeverStaged(Box<UsrRollbackArchivedNeverStagedAuthority<'reservation>>),
}

/// Authority for a rollback decided before its archived candidate was staged.
///
/// It deliberately holds no namespace evidence: there is no child move to
/// revalidate and no canonical slot to bind, because nothing was moved. The
/// only capability it grants is the journal advance.
#[must_use = "never-staged archived authority must persist its successor"]
pub(in crate::client) struct UsrRollbackArchivedNeverStagedAuthority<'reservation> {
    pub(in crate::client) installation: Installation,
    pub(in crate::client) record: TransitionRecord,
    pub(in crate::client) journal_record_binding: TransitionJournalRecordBinding,
    pub(in crate::client) _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact NewState POST authority admitted without moving the candidate in this
/// startup entry.
#[must_use = "already-preserved NewState authority still requires post-move durability"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    _effect: ReconciledNewStateCandidatePreserveEffect<
        'reservation,
        UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace,
    >,
}

/// Opaque authority shared after either origin completed the full suffix.
#[must_use = "durable NewState candidate-preservation authority must remain sealed"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'reservation> {
    _effect: DurableNewStateCandidatePreserveEffect<'reservation>,
    origin: RollbackActionOutcome,
}

struct DurableNewStateCandidatePreserveEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackNewStateCandidatePreserveDurableNamespace,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    /// Consume Finish admission into one exact operation-specific durability
    /// authority or a fieldless unsupported result.
    pub(in crate::client) fn into_post_move_durability_selection(
        self,
        _seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackCandidatePreserveFinishDurabilitySelection<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // The binding is deliberately the first retained-evidence observation.
        self.evidence.require_journal_record_binding(journal)?;
        let topology = self.evidence.namespace.topology();
        if !topology.is_preserved() {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.evidence.revalidate_after_binding(journal, topology)?;

        match topology {
            UsrRollbackCandidatePreserveTopology::NewStatePreserved => {
                let evidence = self.evidence;
                let namespace = evidence
                    .namespace
                    .into_new_state_preserved_durability_evidence(&evidence.record)?;
                Ok(UsrRollbackCandidatePreserveFinishDurabilitySelection::NewState(
                    UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority {
                        _effect: ReconciledNewStateCandidatePreserveEffect {
                            installation: evidence.installation,
                            state_db: evidence.state_db,
                            record: evidence.record,
                            database: evidence.database,
                            namespace,
                            journal_record_binding: evidence.journal_record_binding,
                            _active_state_reservation: evidence._active_state_reservation,
                        },
                    },
                ))
            }
            UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index } => {
                Ok(UsrRollbackCandidatePreserveFinishDurabilitySelection::ActiveReblit(
                    self.into_active_reblit_finish_after_revalidation(journal, wrapper_index)?,
                ))
            }
            UsrRollbackCandidatePreserveTopology::ArchivedPreserved => {
                Ok(UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived(
                    self.into_archived_finish_after_revalidation(journal)?,
                ))
            }
            // Nothing was ever staged, so there is no child move to revalidate and
            // no slot to bind: the candidate already sits where preservation
            // would leave it. Only the journal advance remains.
            UsrRollbackCandidatePreserveTopology::ArchivedNeverStaged => self.never_staged_selection(journal),
            UsrRollbackCandidatePreserveTopology::NewStateStaged
            | UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue
            | UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine
            | UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot
            | UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { .. }
            | UsrRollbackCandidatePreserveTopology::ActiveReblitStagedWithoutReservation => {
                Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
            }
        }
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation> {
    /// Complete the shared suffix after this invocation applied the move.
    pub(in crate::client) fn complete_post_move_durability(
        self,
        _seal: &UsrRollbackCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(
            &self._effect.installation,
            &self._effect.journal_record_binding,
            &self._effect.record,
            journal,
        )?;
        let effect = complete_applied_after_binding(self._effect, journal)?;
        Ok(UsrRollbackNewStateCandidatePreserveDurableEffectAuthority {
            _effect: effect,
            origin: RollbackActionOutcome::Applied,
        })
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    /// Complete the identical suffix from exact already-preserved evidence.
    pub(in crate::client) fn complete_post_move_durability(
        self,
        _seal: &UsrRollbackCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(
            &self._effect.installation,
            &self._effect.journal_record_binding,
            &self._effect.record,
            journal,
        )?;
        let effect = complete_already_satisfied_after_binding(self._effect, journal)?;
        Ok(UsrRollbackNewStateCandidatePreserveDurableEffectAuthority {
            _effect: effect,
            origin: RollbackActionOutcome::AlreadySatisfied,
        })
    }
}

fn complete_applied_after_binding<'reservation>(
    effect: ReconciledNewStateCandidatePreserveEffect<
        'reservation,
        crate::client::startup_reconciliation::activation_namespace::UsrRollbackNewStateCandidatePreserveAppliedNamespace,
    >,
    journal: &TransitionJournalStore,
) -> Result<DurableNewStateCandidatePreserveEffect<'reservation>, UsrRollbackCandidatePreserveAuthorityError> {
    let ReconciledNewStateCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    } = effect;

    require_pre_effect_evidence(
        &installation,
        &state_db,
        &record,
        &database,
        &journal_record_binding,
        journal,
    )?;
    let namespace_result = namespace.complete_post_move_durability(&installation, &record);
    let trailing_evidence =
        require_effect_binding(&installation, &journal_record_binding, &record, journal).and_then(|()| {
            require_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            )
        });
    let namespace = namespace_result?;
    trailing_evidence?;

    Ok(DurableNewStateCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    })
}

fn complete_already_satisfied_after_binding<'reservation>(
    effect: ReconciledNewStateCandidatePreserveEffect<
        'reservation,
        UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace,
    >,
    journal: &TransitionJournalStore,
) -> Result<DurableNewStateCandidatePreserveEffect<'reservation>, UsrRollbackCandidatePreserveAuthorityError> {
    let ReconciledNewStateCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    } = effect;

    require_pre_effect_evidence(
        &installation,
        &state_db,
        &record,
        &database,
        &journal_record_binding,
        journal,
    )?;
    let namespace_result = namespace.complete_post_move_durability(&installation, &record);
    let trailing_evidence =
        require_effect_binding(&installation, &journal_record_binding, &record, journal).and_then(|()| {
            require_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            )
        });
    let namespace = namespace_result?;
    trailing_evidence?;

    Ok(DurableNewStateCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    })
}

#[cfg(test)]
impl UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'_> {
    pub(in crate::client) fn origin_for_test(&self) -> RollbackActionOutcome {
        self.origin
    }
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    /// Admit a rollback whose archived candidate was never staged.
    ///
    /// Revalidates the shared non-namespace evidence and the retained proof,
    /// then hands back an authority carrying no effect: the namespace is
    /// already in its final shape, so there is nothing to reconcile.
    fn into_never_staged_finish_after_revalidation(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackArchivedNeverStagedAuthority<'reservation>, UsrRollbackCandidatePreserveAuthorityError> {
        let evidence = self.evidence;
        evidence.namespace.revalidate(
            &evidence.installation,
            journal,
            &evidence.journal_record_binding,
            &evidence.record,
        )?;
        Ok(UsrRollbackArchivedNeverStagedAuthority {
            installation: evidence.installation,
            record: evidence.record,
            journal_record_binding: evidence.journal_record_binding,
            _active_state_reservation: evidence._active_state_reservation,
        })
    }
}

impl UsrRollbackArchivedNeverStagedAuthority<'_> {
    /// Publish the sole successor. No namespace mutation is possible from here.
    pub(in crate::client) fn advance_candidate_preserved_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(TransitionRecord, TransitionJournalRecordBinding), UsrRollbackCandidatePreserveRecordAdvanceError>
    {
        let successor = self
            .record
            .rollback_successor(Some(crate::transition_journal::RollbackActionOutcome::AlreadySatisfied))
            .map_err(UsrRollbackCandidatePreserveRecordAdvanceError::Successor)?;
        if successor.phase != crate::transition_journal::Phase::CandidatePreserved {
            return Err(UsrRollbackCandidatePreserveRecordAdvanceError::UnexpectedSuccessor { phase: successor.phase });
        }
        let cast = self.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self.journal_record_binding, &successor) {
            Ok(binding) => Ok((successor, binding)),
            Err(source) => Err(UsrRollbackCandidatePreserveRecordAdvanceError::Storage { source, successor }),
        }
    }
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    /// Own call frame: the selection match already carries every other arm's
    /// locals, and the endpoint walk sits close to the default test stack.
    fn never_staged_selection(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackCandidatePreserveFinishDurabilitySelection<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        Ok(
            UsrRollbackCandidatePreserveFinishDurabilitySelection::ArchivedNeverStaged(Box::new(
                self.into_never_staged_finish_after_revalidation(journal)?,
            )),
        )
    }
}
