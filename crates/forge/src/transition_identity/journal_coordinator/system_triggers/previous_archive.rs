//! Durable predecessor-archive advance for `archive_previous` transitions.
//!
//! NewState (and any future operation that displaces a real predecessor) must
//! move the exact staged previous tree into its authenticated per-state slot
//! between `SystemTriggersComplete` and boot publication. This boundary drives
//! the two intervening journal phases with the same bound-record discipline the
//! system-trigger runner uses: the `PreviousArchiveIntent` record is persisted
//! *before* the physical move, and `PreviousArchived` *after* it, so a crash at
//! any point leaves a durable record startup reconciliation can resume or
//! reverse. ActiveReblit repairs its state in place and never reaches here.

use thiserror::Error;

use super::{
    CandidateMetadataProof, Phase, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError,
    SystemTriggersCompleteCoordinator, TransitionRecord, UsrExchangeReadiness, advance_bound_system_trigger_record, db,
    require_same_store_record_binding, require_system_trigger_same_store_evidence, state,
};
use crate::state::TransitionId;
use crate::transition_journal::TransitionJournalRecordBinding;

type BoxedAdvanceError = Box<dyn std::error::Error + Send + Sync + 'static>;

const ARCHIVE_PREVIOUS: &str = "archive the previous state tree";
const SKIP_SYSTEM_TRIGGERS: &str = "archive the previous state tree without system triggers";
const HAND_OFF_NEW_STATE_BOOT: &str = "hand off new state boot synchronization";

/// Unforgeable proof that a journal-coordinated caller owns the exact durable
/// `PreviousArchiveIntent` record and may physically archive its predecessor
/// while its transition journal is retained. Every legacy archive entry point
/// continues to require journal absence.
pub(crate) struct PreviousArchiveEffectSeal {
    _private: (),
}

/// Unforgeable proof that a NewState transition reached the exact durable
/// `PreviousArchived` record and may hand its retained stores into boot
/// publication. The client boot-staging constructor accepts only this seal for
/// the NewState route, mirroring the ActiveReblit handoff seal.
pub(crate) struct PreviousArchivedBootSyncHandoffSeal {
    _private: (),
}

/// Sole in-process owner after the predecessor tree is durably archived, ready
/// to hand off into boot publication.
#[derive(Debug)]
pub(crate) struct PreviousArchivedCoordinator {
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    authority: crate::client::PublishedJournalRootAbiAuthority,
    readiness: UsrExchangeReadiness,
    record_binding: TransitionJournalRecordBinding,
}

#[derive(Debug, Error)]
pub(crate) enum PreviousArchiveFailure {
    #[error("transition {transition_id} is not an archive-previous SystemTriggersComplete authority")]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id} failed predecessor-archive preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} produced an unexpected {stage} archive successor")]
    SuccessorContract {
        transition_id: TransitionId,
        stage: &'static str,
    },
    #[error("transition {transition_id} failed to persist the {stage} archive record")]
    Advance {
        transition_id: TransitionId,
        stage: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("transition {transition_id} failed to physically archive its predecessor tree")]
    PhysicalArchive {
        transition_id: TransitionId,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

#[derive(Debug, Error)]
pub(crate) enum PreviousArchiveBootHandoffFailure {
    #[error("transition {transition_id} is not a NewState PreviousArchived boot authority")]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id} failed NewState boot-handoff preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} could not load its candidate state for boot")]
    CandidateState {
        transition_id: TransitionId,
        #[source]
        source: BoxedAdvanceError,
    },
}

impl super::RootLinksCompleteCoordinator {
    /// Archive the predecessor without running system triggers.
    ///
    /// `cast state activate --skip-triggers` is a real flag, and the coordinated
    /// route had no way to honour it: `run_system_triggers` was the only exit
    /// from `RootLinksComplete`, so the swapped call site simply discarded the
    /// flag (`plans/close_out.md`).
    ///
    /// The journal chain already modelled this — `next_forward_phase` sends
    /// `RootLinksComplete -> PreviousArchiveIntent` when
    /// `!options.run_system_triggers` — so no phase is skipped or invented here.
    /// Only the trigger effect and its two phases are absent, which is exactly
    /// what the caller asked for, and the record says so.
    pub(crate) fn skip_system_triggers(self) -> Result<PreviousArchivedCoordinator, PreviousArchiveFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();

        // Same admission as the ordinary tail, minus the source phase: a
        // predecessor must exist and be scheduled for archiving.
        if coordinator.record.options.run_system_triggers
            || !coordinator.record.options.archive_previous
            || coordinator.record.previous.id.is_none()
        {
            return Err(PreviousArchiveFailure::SourceContract { transition_id });
        }
        let preflight = |source| PreviousArchiveFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::RootLinksComplete, SKIP_SYSTEM_TRIGGERS)
            .map_err(preflight)?;
        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(preflight)?;

        // Intent is durable before the physical move, exactly as in the
        // triggered tail.
        let intent =
            archive_successor(&coordinator.record, Phase::PreviousArchiveIntent, "intent").map_err(|stage| {
                PreviousArchiveFailure::SuccessorContract {
                    transition_id: transition_id.clone(),
                    stage,
                }
            })?;
        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            intent,
        )
        .map_err(|source| PreviousArchiveFailure::Advance {
            transition_id: transition_id.clone(),
            stage: "intent",
            source: Box::new(source),
        })?;

        finish_previous_archive(
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
            transition_id,
        )
    }
}

impl SystemTriggersCompleteCoordinator {
    /// Durably archive the displaced predecessor tree, advancing the journal
    /// through `PreviousArchiveIntent → PreviousArchived`.
    pub(crate) fn archive_previous_tree(self) -> Result<PreviousArchivedCoordinator, PreviousArchiveFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();

        if !exact_archive_previous_source(&coordinator.record) {
            return Err(PreviousArchiveFailure::SourceContract { transition_id });
        }
        let preflight = |source| PreviousArchiveFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::SystemTriggersComplete, ARCHIVE_PREVIOUS)
            .map_err(preflight)?;
        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(preflight)?;

        // Intent is durable before the physical move: a crash after this record
        // leaves startup reconciliation to complete or reverse the archive.
        let intent =
            archive_successor(&coordinator.record, Phase::PreviousArchiveIntent, "intent").map_err(|stage| {
                PreviousArchiveFailure::SuccessorContract {
                    transition_id: transition_id.clone(),
                    stage,
                }
            })?;
        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            intent,
        )
        .map_err(|source| PreviousArchiveFailure::Advance {
            transition_id: transition_id.clone(),
            stage: "intent",
            source: Box::new(source),
        })?;

        finish_previous_archive(
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
            transition_id,
        )
    }
}

/// Physically archive the predecessor and persist `PreviousArchived`.
///
/// Shared by both ways of reaching a durable `PreviousArchiveIntent`: the
/// ordinary system-trigger tail, and `skip_system_triggers` for a caller that
/// asked not to run them (`cast state activate --skip-triggers`). Only the route
/// to the intent record differs; everything after it is identical, so it lives
/// here rather than being written twice.
fn finish_previous_archive(
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    authority: crate::client::PublishedJournalRootAbiAuthority,
    readiness: UsrExchangeReadiness,
    record_binding: TransitionJournalRecordBinding,
    transition_id: TransitionId,
) -> Result<PreviousArchivedCoordinator, PreviousArchiveFailure> {
    {
        // Physical move of the exact staged predecessor into its state slot.
        let previous_id = coordinator.record.previous.id.map(state::Id::from).ok_or_else(|| {
            PreviousArchiveFailure::SourceContract {
                transition_id: transition_id.clone(),
            }
        })?;
        let seal = PreviousArchiveEffectSeal { _private: () };
        coordinator
            .identity
            .archive_previous_with_journal(authority.installation(), previous_id, &seal)
            .map_err(|source| PreviousArchiveFailure::PhysicalArchive {
                transition_id: transition_id.clone(),
                source: Box::new(source),
            })?;

        // Completion is durable only after the move succeeded.
        let archived =
            archive_successor(&coordinator.record, Phase::PreviousArchived, "archived").map_err(|stage| {
                PreviousArchiveFailure::SuccessorContract {
                    transition_id: transition_id.clone(),
                    stage,
                }
            })?;
        // The physical move deliberately relocated the previous tree out of
        // staging, so the tree/root-ABI same-store sandwich no longer holds by
        // design. Completion re-validates only the journal record binding (cast
        // and canonical inode), which the move did not touch.
        let (coordinator, record_binding) =
            advance_archive_completion_record(coordinator, &authority, record_binding, archived).map_err(|source| {
                PreviousArchiveFailure::Advance {
                    transition_id,
                    stage: "archived",
                    source,
                }
            })?;

        Ok(PreviousArchivedCoordinator {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        })
    }
}

impl PreviousArchivedCoordinator {
    /// Retire the wrapper the archived-candidate staging exchange displaced.
    ///
    /// The exchange leaves the old staging wrapper parked under the candidate's
    /// canonical state name. Only the legacy route ever retired it, so the
    /// coordinated route left it behind and the *next* activation failed with
    /// `PreviousArchiveSlotExists` — activate 2, then activate 1, and the second
    /// one refuses. Caught by
    /// `repeated_archived_activations_reuse_wrapper_slots_beyond_the_scan_bound`
    /// (`plans/close_out.md`).
    ///
    /// Runs after the predecessor archive so the previous tree is already out of
    /// staging, matching where the legacy route did it.
    pub(crate) fn retire_displaced_archived_slot(
        &self,
        installation: &crate::Installation,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        let seal = super::super::ArchivedCandidateStagingEffectSeal { _private: () };
        self.coordinator
            .identity
            .retire_displaced_archived_candidate_slot_with_journal(installation, candidate, &seal)
            .map_err(|source| StatefulTransitionCoordinatorError::ArchivedCandidateStaging(Box::new(source)))
    }

    /// The retained candidate `/usr` descriptor, for a boot tail that must not
    /// reopen the staging pathname (see the coordinator's accessor).
    pub(crate) fn retained_candidate_usr(&self) -> (&std::fs::File, &std::path::Path) {
        self.coordinator.retained_candidate_usr()
    }

    /// Hand the retained stores into boot publication for the freshly booted
    /// NewState candidate. The predecessor has already been durably archived,
    /// so `boot::synchronize` can enumerate it as an immediate rollback entry.
    pub(crate) fn into_new_state_boot_sync_handoff(
        self,
    ) -> Result<crate::client::CoordinatorActiveReblitBootSyncHandoff, PreviousArchiveBootHandoffFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();

        if !exact_new_state_boot_source(&coordinator.record) {
            return Err(PreviousArchiveBootHandoffFailure::SourceContract { transition_id });
        }
        let preflight = |source| PreviousArchiveBootHandoffFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::PreviousArchived, HAND_OFF_NEW_STATE_BOOT)
            .map_err(preflight)?;
        // The predecessor tree left staging during the archive, so only the
        // journal record binding (cast + canonical inode) is re-validated here.
        require_same_store_record_binding(&coordinator, &authority, &record_binding).map_err(preflight)?;

        let candidate_id = coordinator.record.candidate.id.map(state::Id::from).ok_or_else(|| {
            PreviousArchiveBootHandoffFailure::SourceContract {
                transition_id: transition_id.clone(),
            }
        })?;
        let installation = authority.installation().clone();
        let active_state_reservation = authority.into_active_state_reservation();
        drop(metadata);
        let _ = provenance;
        drop(readiness);
        let StatefulTransitionCoordinator { identity, record } = coordinator;
        let crate::transition_identity::StatefulTreeIdentity {
            journal,
            state_database,
            ..
        } = identity;
        let boot_candidate =
            state_database
                .get(candidate_id)
                .map_err(|source| PreviousArchiveBootHandoffFailure::CandidateState {
                    transition_id,
                    source: Box::new(source),
                })?;
        Ok(
            crate::client::CoordinatorActiveReblitBootSyncHandoff::from_previous_archived(
                PreviousArchivedBootSyncHandoffSeal { _private: () },
                record,
                record_binding,
                journal,
                state_database,
                installation,
                boot_candidate,
                active_state_reservation,
            ),
        )
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.coordinator.record
    }

    #[cfg(test)]
    pub(crate) fn tree_identity(&self) -> &crate::transition_identity::StatefulTreeIdentity {
        &self.coordinator.identity
    }

    #[cfg(test)]
    pub(crate) fn installation(&self) -> &crate::Installation {
        self.authority.installation()
    }
}

fn exact_new_state_boot_source(record: &TransitionRecord) -> bool {
    record.operation == crate::transition_journal::Operation::NewState
        && record.phase == Phase::PreviousArchived
        && record.options.archive_previous
        && record.options.run_boot_sync
        && record.candidate.id.is_some()
        && record.previous.id.is_some()
        && record.candidate.id != record.previous.id
}

fn exact_archive_previous_source(record: &TransitionRecord) -> bool {
    record.phase == Phase::SystemTriggersComplete && record.options.archive_previous && record.previous.id.is_some()
}

fn archive_successor(
    record: &TransitionRecord,
    expected_phase: Phase,
    stage: &'static str,
) -> Result<TransitionRecord, &'static str> {
    let successor = record.forward_successor(None).map_err(|_| stage)?;
    if successor.phase != expected_phase {
        return Err(stage);
    }
    Ok(successor)
}

/// Durably advance the record binding without re-running the tree/root-ABI
/// same-store sandwich. Used only for the post-move `PreviousArchived` advance,
/// where the predecessor tree has legitimately left staging; the journal cast
/// and canonical record inode — the only things this validates — are untouched.
fn advance_archive_completion_record(
    mut coordinator: StatefulTransitionCoordinator,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    predecessor_binding: TransitionJournalRecordBinding,
    successor: TransitionRecord,
) -> Result<(StatefulTransitionCoordinator, TransitionJournalRecordBinding), BoxedAdvanceError> {
    let cast = authority
        .installation()
        .retained_mutable_cast_directory()
        .map_err(crate::transition_identity::Error::from)?;
    let successor_binding =
        coordinator
            .identity
            .journal
            .advance_record_binding(&cast, predecessor_binding, &successor)?;
    coordinator.record = successor;
    require_same_store_record_binding(&coordinator, authority, &successor_binding)?;
    Ok((coordinator, successor_binding))
}

/// Retained capabilities handed to the terminal tail when a NewState
/// transition commits without publishing boot entries.
pub(crate) struct NewStateNoBootCommitDecisionHandoff {
    pub(crate) journal: crate::transition_journal::TransitionJournalStore,
    pub(crate) state_database: db::state::Database,
    pub(crate) installation: crate::Installation,
    pub(crate) record: TransitionRecord,
    pub(crate) active_state_reservation: crate::client::CoordinatorActiveStateReservation,
}

impl PreviousArchivedCoordinator {
    /// Advance a non-bootable NewState transition from `PreviousArchived`
    /// straight to `CommitDecided`, skipping boot publication.
    ///
    /// The phase model already permits this: `PreviousArchived` advances to
    /// `CommitDecided` whenever `run_boot_sync` is unset. A candidate that
    /// publishes no kernel is the ordinary reason — see
    /// `plans/future_impl.md` §1.1a, which fixes `run_boot_sync` before the
    /// record is written so this path is reachable at all.
    ///
    /// Only the record binding is revalidated: the predecessor tree legitimately
    /// left staging during the archive, so the tree/root-ABI sandwich no longer
    /// describes this namespace.
    // Forward scaffolding: consumed once Slice 5 wires the coordinated route live.
    #[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
    pub(crate) fn commit_new_state_without_boot(
        self,
    ) -> Result<NewStateNoBootCommitDecisionHandoff, PreviousArchiveNoBootFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();

        if !exact_new_state_no_boot_source(&coordinator.record) {
            return Err(PreviousArchiveNoBootFailure::SourceContract { transition_id });
        }
        let preflight = |source| PreviousArchiveNoBootFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::PreviousArchived, COMMIT_NEW_STATE_WITHOUT_BOOT)
            .map_err(preflight)?;
        require_same_store_record_binding(&coordinator, &authority, &record_binding).map_err(preflight)?;

        let successor = coordinator
            .record
            .forward_successor(None)
            .map_err(StatefulTransitionCoordinatorError::from)
            .map_err(preflight)?;
        if successor.phase != Phase::CommitDecided {
            return Err(PreviousArchiveNoBootFailure::SuccessorContract {
                transition_id,
                actual_phase: successor.phase,
            });
        }

        let (coordinator, record_binding) =
            advance_archive_completion_record(coordinator, &authority, record_binding, successor).map_err(
                |source| PreviousArchiveNoBootFailure::Advance {
                    transition_id: transition_id.clone(),
                    source,
                },
            )?;
        require_same_store_record_binding(&coordinator, &authority, &record_binding).map_err(|source| {
            PreviousArchiveNoBootFailure::Preflight {
                transition_id: transition_id.clone(),
                source,
            }
        })?;

        let installation = authority.installation().clone();
        let active_state_reservation = authority.into_active_state_reservation();
        drop(record_binding);
        drop(metadata);
        let _ = provenance;
        drop(readiness);
        let StatefulTransitionCoordinator { identity, record } = coordinator;
        let crate::transition_identity::StatefulTreeIdentity {
            journal,
            state_database,
            ..
        } = identity;
        Ok(NewStateNoBootCommitDecisionHandoff {
            journal,
            state_database,
            installation,
            record,
            active_state_reservation,
        })
    }
}

const COMMIT_NEW_STATE_WITHOUT_BOOT: &str = "commit new state without boot";

/// A NewState transition that archived its predecessor and carries no bootable
/// payload, so it commits directly from `PreviousArchived`.
fn exact_new_state_no_boot_source(record: &TransitionRecord) -> bool {
    // Both operations that make a *candidate* live reach `PreviousArchived` and
    // may commit from there without boot; ActiveReblit does not — it has its own
    // no-boot tail, because its candidate and previous are the same state.
    // Widened deliberately and by naming the operations, not by dropping the
    // check: an over-broad admission here is exactly the failure mode the
    // rollback predicates already produced twice (`plans/close_out.md`).
    matches!(
        record.operation,
        crate::transition_journal::Operation::NewState | crate::transition_journal::Operation::ActivateArchived
    ) && record.phase == Phase::PreviousArchived
        && record.rollback.is_none()
        && record.options.archive_previous
        && !record.options.run_boot_sync
        && record.boot_publication_receipts.is_none()
        && record.candidate.id.is_some()
        && record.previous.id.is_some()
        && record.candidate.id != record.previous.id
}

#[derive(Debug, Error)]
pub(crate) enum PreviousArchiveNoBootFailure {
    #[error("transition {transition_id} is not an exact no-boot NewState commit source")]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id}: preflight for the no-boot NewState commit")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id}: no-boot NewState successor is phase {actual_phase:?}, not commit-decided")]
    SuccessorContract {
        transition_id: TransitionId,
        actual_phase: Phase,
    },
    #[error("transition {transition_id}: advance the no-boot NewState commit decision")]
    Advance {
        transition_id: TransitionId,
        #[source]
        source: BoxedAdvanceError,
    },
}
