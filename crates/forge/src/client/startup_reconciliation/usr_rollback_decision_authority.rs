//! Sealed evidence authority for persisting one journal-only rollback decision.

use crate::{
    Installation, db,
    transition_journal::{
        InitialRollbackAction, Operation, Phase, RollbackObservations, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackDecisionSeal,
    startup_recovery::UsrExchangeParentDurabilityCompletionSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrExchangeLayout, UsrRollbackDecisionNamespaceError,
    UsrRollbackDecisionNamespaceInspection, UsrRollbackDecisionNamespaceProof, database_ownership_evidence_compatible,
    inspect_database, metadata_provenance_evidence_compatible,
};

/// Result of asking whether the exact startup evidence admits the narrow
/// journal-only rollback-decision slice.
#[allow(dead_code)] // deferral detail is consumed by focused startup contracts
pub(in crate::client) enum UsrRollbackDecisionAdmission<'reservation> {
    NotApplicable,
    Deferred(UsrRollbackDecisionDeferral),
    ParentDurabilityRequired(UsrExchangeParentDurabilityAuthority<'reservation>),
    Ready(UsrRollbackDecisionAuthority<'reservation>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum UsrRollbackDecisionDeferral {
    IncompatibleEvidence,
}

/// Exact, retained database/namespace evidence plus the cooperating-writer
/// reservation under which the evidence was captured.
///
/// The reservation is intentionally not interpreted as an active-selection
/// witness. Its only role here is to keep cooperating namespace writers
/// excluded until the executor either persists the decision or fails stop.
pub(in crate::client) struct UsrRollbackDecisionAuthority<'reservation> {
    evidence: UsrRollbackDecisionEvidence<'reservation>,
    observations: RollbackObservations,
}

/// Exact Intent+POST evidence which may become rollback-decision authority
/// only after both exchange-parent durability barriers complete.
pub(in crate::client) struct UsrExchangeParentDurabilityAuthority<'reservation> {
    evidence: UsrRollbackDecisionEvidence<'reservation>,
}

/// Evidence shared by direct rollback-decision admission and the narrower
/// parent-durability normalization typestate.
struct UsrRollbackDecisionEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackDecisionNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackDecisionAuthority<'reservation> {
    /// Capture authority only when presented with the unforgeable startup-gate
    /// seal. Safe code outside that writer-first gate cannot admit persistence.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackDecisionSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackDecisionAdmission<'reservation>, UsrRollbackDecisionAuthorityError> {
        Self::capture_from_context(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            initial_in_flight,
        )
    }

    fn capture_from_context(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackDecisionAdmission<'reservation>, UsrRollbackDecisionAuthorityError> {
        if !rollback_decision_source_is_supported(record) {
            return Ok(UsrRollbackDecisionAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection = match UsrRollbackDecisionNamespaceInspection::begin(installation, journal, record) {
            Ok(inspection) => inspection,
            Err(_) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
        };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) {
            return Ok(UsrRollbackDecisionAdmission::Deferred(
                UsrRollbackDecisionDeferral::IncompatibleEvidence,
            ));
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackDecisionAdmission::Deferred(
                UsrRollbackDecisionDeferral::IncompatibleEvidence,
            ));
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
        };

        let mut usr_exchange_not_required = false;
        let usr_exchange = match (record.phase, namespace.layout()) {
            (Phase::UsrExchangeIntent, UsrExchangeLayout::Pre) => Some(InitialRollbackAction::AlreadySatisfied),
            (Phase::UsrExchangeIntent, UsrExchangeLayout::Post) => None,
            (Phase::UsrExchanged, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),
            (Phase::UsrExchanged, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            (Phase::RootLinksComplete, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),
            (Phase::RootLinksComplete, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            (Phase::SystemTriggersStarted | Phase::SystemTriggersComplete, UsrExchangeLayout::Post) => {
                Some(InitialRollbackAction::Pending)
            }
            (Phase::SystemTriggersStarted | Phase::SystemTriggersComplete, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            // Every operation reaching this phase crossed the exchange to get
            // here, so the layout rule is the same for all of them. Restricting
            // it to ActiveReblit left a NewState boot sync stalled forever.
            (Phase::BootSyncStarted, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),
            (Phase::BootSyncStarted, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            // NewState archived its predecessor before booting: the candidate is
            // live and the predecessor sits in its archived slot, so the /usr
            // exchange still needs reversal (Post) after the predecessor is first
            // restored to staging (see `previous_archive` below).
            // The intent phase spans the archive, so both its layouts are post-exchange:
            // the candidate is live either way and the predecessor is in staging or
            // already in its slot. Omitting it left a crash there stalled forever.
            (Phase::PreviousArchiveIntent | Phase::PreviousArchived, UsrExchangeLayout::Post) => {
                Some(InitialRollbackAction::Pending)
            }
            (Phase::PreviousArchiveIntent | Phase::PreviousArchived, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            // Pre-exchange sources. `/usr` was never touched, so there is no
            // exchange to reverse and the plan carries no usr action; the
            // rollback chain only has to discard the candidate. A `Post` layout
            // at these phases contradicts the record — the exchange happened but
            // the journal never recorded reaching it — so defer rather than
            // guess (`plans/future_impl.md` §1.4).
            // The archived-staging pair belongs here too. ActivateArchived
            // traverses it *before* candidate preparation
            // (`plans/future_impl.md` §1.2b), so `/usr` is equally untouched and
            // the reasoning above applies unchanged. Only this operation reaches
            // these phases, which is why nothing caught their absence until the
            // ActivateArchived crash matrix ran (2026-08-10): a cut at
            // `ArchivedCandidateStaged` fell through to the catch-all and
            // aborted on every startup, leaving the system unrecoverable.
            (
                Phase::Preparing
                | Phase::FreshStateAllocating
                | Phase::FreshStateAllocated
                | Phase::ArchivedCandidateStagingIntent
                | Phase::ArchivedCandidateStaged
                | Phase::CandidatePrepareStarted
                | Phase::CandidatePrepared
                | Phase::TransactionTriggersStarted
                | Phase::TransactionTriggersComplete,
                UsrExchangeLayout::Pre,
            ) => {
                usr_exchange_not_required = true;
                None
            }
            (
                Phase::Preparing
                | Phase::FreshStateAllocating
                | Phase::FreshStateAllocated
                | Phase::ArchivedCandidateStagingIntent
                | Phase::ArchivedCandidateStaged
                | Phase::CandidatePrepareStarted
                | Phase::CandidatePrepared
                | Phase::TransactionTriggersStarted
                | Phase::TransactionTriggersComplete,
                UsrExchangeLayout::Post,
            ) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            // Deferral, not `unreachable!`. This runs on the recovery path, so an
            // unmodelled source that aborts here is not a loud bug report — it
            // panics on every startup and the system can never recover, which is
            // strictly worse than refusing. A phase this admission does not model
            // must decline and leave the record for a human.
            _ => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
        };
        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        let evidence = UsrRollbackDecisionEvidence {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        // `None` from the match is overloaded: for `UsrExchangeIntent + Pre` it
        // means the exchange parent still needs normalising, while for a
        // pre-exchange source it means there is simply no exchange to reverse.
        // The flag separates them.
        Ok(match (usr_exchange, usr_exchange_not_required) {
            (Some(usr_exchange), _) => UsrRollbackDecisionAdmission::Ready(Self {
                observations: rollback_observations(
                    record.operation,
                    Some(usr_exchange),
                    previous_archive_observation(record),
                ),
                evidence,
            }),
            (None, true) => UsrRollbackDecisionAdmission::Ready(Self {
                observations: rollback_observations(record.operation, None, previous_archive_observation(record)),
                evidence,
            }),
            (None, false) => {
                UsrRollbackDecisionAdmission::ParentDurabilityRequired(UsrExchangeParentDurabilityAuthority {
                    evidence,
                })
            }
        })
    }

    /// Revalidate the owned source record, retained namespace inventories, and
    /// an exact database/namespace/database sandwich immediately around use.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackDecisionAuthorityError> {
        self.evidence.revalidate(journal)
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.evidence.record
    }

    pub(in crate::client) fn observations(&self) -> RollbackObservations {
        self.observations
    }

    /// Revalidate, then consume this complete authority through the exact
    /// predecessor-to-successor journal boundary. The database, namespace,
    /// reservation, and record evidence remain owned until the one-shot store
    /// call consumes the predecessor binding.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        next: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackDecisionRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.evidence.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.evidence.journal_record_binding, next)
            .map_err(UsrRollbackDecisionRecordAdvanceError::Storage)
    }
}

fn rollback_decision_source_is_supported(record: &TransitionRecord) -> bool {
    // Every pre-exchange phase, derived from the ordinal rather than listed.
    //
    // Nothing in `/usr` has been touched before `UsrExchangeIntent`, so the
    // derived plan carries `usr_exchange: NotRequired` and the chain only has
    // to discard the candidate. The list this replaces started at
    // `CandidatePrepared` and silently stranded everything earlier: a crash
    // while preparing a candidate, allocating the fresh state, or staging an
    // archived candidate left a record whose disposition is `BeginRollback`
    // that no dispatcher would accept, so every startup repeated the same
    // `PendingSystemTransition` forever (`plans/future_impl.md` §1.4, and again
    // 2026-07-30).
    //
    // Chain membership is required so an operation cannot claim a phase it
    // never passes through — activation never runs transaction triggers.
    let pre_exchange = record.phase.forward().is_some_and(|source| {
        source.ordinal() < crate::transition_journal::ForwardPhase::UsrExchangeIntent.ordinal()
            && crate::transition_journal::expected_forward_generation(record, source).is_some()
    });
    pre_exchange
        || matches!(
            record.phase,
            Phase::UsrExchangeIntent | Phase::UsrExchanged | Phase::RootLinksComplete
        )
        // Post-exchange sources, with the generation *derived* rather than
        // listed per operation. The table this replaces carried only NewState
        // and ActiveReblit rows, so ActivateArchived had no post-exchange
        // rollback admission at all beyond `RootLinksComplete`: a plain
        // install → remove → activate stalls at `PreviousArchived` on a real
        // guest, with no crash injection involved (VM run 2026-07-31).
        //
        // ADMISSION ONLY — READ BEFORE RELYING ON THIS. The reverse-exchange
        // and previous-restore effects for the ActivateArchived post-exchange
        // chain are NOT implemented. These rollbacks are expected to advance
        // and then stall further in. This was a deliberate, requested step to
        // make the next gap visible; it is not a completed recovery path. Do
        // not delete this note until the effects exist and a crash-matrix cell
        // proves they run.
        || (matches!(
            record.phase,
            Phase::SystemTriggersStarted
                | Phase::SystemTriggersComplete
                | Phase::PreviousArchiveIntent
                | Phase::PreviousArchived
                | Phase::BootSyncStarted
        ) && record
            .phase
            .forward()
            .and_then(|source| crate::transition_journal::expected_forward_generation(record, source))
            == Some(record.generation))
}

#[cfg(test)]
pub(in crate::client) fn usr_rollback_decision_source_is_supported_for_test(record: &TransitionRecord) -> bool {
    rollback_decision_source_is_supported(record)
}

impl UsrRollbackDecisionEvidence<'_> {
    /// Revalidate the owned source record, retained namespace inventories, and
    /// an exact database/namespace/database sandwich immediately around use.
    fn revalidate(&self, journal: &TransitionJournalStore) -> Result<(), UsrRollbackDecisionAuthorityError> {
        // Exact public record identity is deliberately the first check. Equal
        // bytes at a replacement inode cannot authorize persistence.
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackDecisionAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackDecisionAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackDecisionAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

impl<'reservation> UsrExchangeParentDurabilityAuthority<'reservation> {
    /// Revalidate exact Intent+POST normalization authority. The shared
    /// evidence routine performs the per-open journal binding check first.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackDecisionAuthorityError> {
        self.evidence.revalidate(journal)?;
        self.require_intent_post()
    }

    /// Apply only the retained staging-parent durability barrier.
    pub(in crate::client) fn sync_retained_staging_parent(
        &self,
        before_sync: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<(u64, u64), UsrRollbackDecisionAuthorityError> {
        self.evidence
            .namespace
            .sync_retained_staging_parent(before_sync)
            .map_err(UsrRollbackDecisionAuthorityError::from)
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    /// Consume completed normalization authority into the existing sealed
    /// rollback-decision capability. Only the normalizer can construct the
    /// completion seal.
    pub(in crate::client) fn complete(
        self,
        _seal: UsrExchangeParentDurabilityCompletionSeal,
    ) -> Result<UsrRollbackDecisionAuthority<'reservation>, UsrRollbackDecisionAuthorityError> {
        self.require_intent_post()?;
        let operation = self.evidence.record.operation;
        Ok(UsrRollbackDecisionAuthority {
            evidence: self.evidence,
            observations: rollback_observations(operation, Some(InitialRollbackAction::Pending), None),
        })
    }

    fn require_intent_post(&self) -> Result<(), UsrRollbackDecisionAuthorityError> {
        if self.evidence.record.phase == Phase::UsrExchangeIntent
            && self.evidence.namespace.layout() == UsrExchangeLayout::Post
        {
            Ok(())
        } else {
            Err(UsrRollbackDecisionAuthorityErrorKind::ParentDurabilitySourceMismatch.into())
        }
    }
}

fn rollback_observations(
    operation: Operation,
    usr_exchange: Option<InitialRollbackAction>,
    previous_archive: Option<InitialRollbackAction>,
) -> RollbackObservations {
    RollbackObservations {
        allocated_candidate_id: None,
        previous_archive,
        // `None` here means the exchange never happened, so the derived plan
        // carries `usr_exchange: NotRequired`.
        usr_exchange,
        candidate: InitialRollbackAction::Pending,
        fresh_db: (operation == Operation::NewState).then_some(InitialRollbackAction::Pending),
    }
}

/// Derived from the journal's own possibility rule rather than a phase list.
/// The plan must agree with it exactly: a source the journal calls possible but
/// the plan calls NotRequired is refused. Hand-listing the phases here drifted
/// out of step three times, each time stalling a crash at the missing phase.
fn previous_archive_observation(record: &TransitionRecord) -> Option<InitialRollbackAction> {
    let source = record.phase.forward()?;
    record
        .previous_restore_rollback_is_possible(source)
        .then_some(InitialRollbackAction::Pending)
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackDecisionAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackDecisionAuthorityErrorKind::DatabaseIncompatible {
            evidence: Box::new(evidence),
        }
        .into())
    }
}

fn database_is_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    database_ownership_evidence_compatible(record, evidence)
        && metadata_provenance_evidence_compatible(record, evidence)
}

fn require_exact_database(
    expected: &DatabaseEvidence,
    actual: DatabaseEvidence,
) -> Result<(), UsrRollbackDecisionAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackDecisionAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackDecisionAuthorityError(#[from] UsrRollbackDecisionAuthorityErrorKind);

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackDecisionRecordAdvanceError {
    #[error("revalidate exact rollback-decision authority before the bound journal advance")]
    Authority(#[from] UsrRollbackDecisionAuthorityError),
    #[error("revalidate retained installation before the bound rollback-decision journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound rollback-decision journal record")]
    Storage(#[source] StorageError),
}

impl From<InspectionError> for UsrRollbackDecisionAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackDecisionNamespaceError> for UsrRollbackDecisionAuthorityError {
    fn from(source: UsrRollbackDecisionNamespaceError) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackDecisionAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackDecisionAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackDecisionAuthorityErrorKind {
    #[error("startup rollback-decision authority lost its exact canonical journal record binding")]
    JournalRecordBindingMismatch,
    #[error("startup parent-durability authority is not bound to exact UsrExchangeIntent + POST evidence")]
    ParentDurabilitySourceMismatch,
    #[error("inspect exact rollback-decision database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent rollback-decision namespace proof")]
    Namespace(#[source] UsrRollbackDecisionNamespaceError),
    #[error("revalidate the retained mutable installation namespace around rollback-decision authority")]
    Installation(#[source] crate::installation::Error),
    #[error("capture or revalidate the exact rollback-decision journal record binding")]
    Journal(#[source] StorageError),
    #[error("rollback-decision database evidence is incompatible with the persisted source: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-decision database evidence changed from {expected:?} to {actual:?}")]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
    },
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_INITIAL_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[allow(dead_code)] // armed by focused rollback-decision race contracts
pub(in crate::client) fn arm_between_usr_rollback_decision_database_captures(hook: impl FnOnce() + 'static) {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_initial_database_captures() {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_initial_database_captures() {}
