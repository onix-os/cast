//! One-shot cleanup reconciliation and shared fixed durability suffix.

use crate::transition_journal::TransitionJournalStore;

use super::{
    ActiveReblitCommitCleanupApplyEffectAuthority, ActiveReblitCommitCleanupAuthorityError,
    ActiveReblitCommitCleanupCommonEvidence, ActiveReblitCommitCleanupDatabaseEvidence,
    ActiveReblitCommitCleanupFinishEffectAuthority, inspect_current_database,
    record_plan_is_exact, require_exact_active_state, require_exact_database,
    require_exact_record_binding,
};
use crate::client::startup_reconciliation::activation_namespace::{
    ActiveReblitCommitCleanupDurabilityError, ActiveReblitCommitCleanupEffectError as NamespaceEffectError,
    ActiveReblitCommitCleanupExchangeReconciliation,
    DurableActiveReblitCommitCleanupNamespace, PendingActiveReblitCommitCleanupDurability,
};

/// Semantic result of consuming the exact Apply capability. Neither failure
/// variant carries retry or durability authority.
#[must_use = "a consumed ActiveReblit cleanup exchange must be handled"]
pub(in crate::client) enum ActiveReblitCommitCleanupApplyReconciliation<'reservation> {
    Applied(ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

/// Exact completed layout from either freshly Applied or independently
/// admitted Finish evidence. Both origins enter the same suffix here.
#[must_use = "completed ActiveReblit cleanup evidence still requires durability"]
pub(in crate::client) struct ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    namespace: PendingActiveReblitCommitCleanupDurability,
}

/// Sealed authority returned only after the fixed suffix and trailing exact
/// evidence checks complete. It exposes no persistence operation here.
#[must_use = "durable ActiveReblit cleanup authority must remain sealed"]
pub(in crate::client) struct ActiveReblitCommitCleanupDurableAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    namespace: DurableActiveReblitCommitCleanupNamespace,
}

impl<'reservation> ActiveReblitCommitCleanupApplyEffectAuthority<'reservation> {
    /// Consume exact Apply evidence through one and only one exchange attempt.
    pub(in crate::client) fn reconcile(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupApplyReconciliation<'reservation>,
        ActiveReblitCommitCleanupEffectError,
    > {
        let Self {
            _evidence: evidence,
            _namespace: namespace,
        } = self;

        let database_before = begin_common_revalidation(&evidence, journal)?;
        let prepared = namespace.prepare_exchange(&evidence.installation, &evidence.record);
        let trailing = finish_common_revalidation(&evidence, journal, database_before);
        let prepared = prepared?;
        trailing?;

        let database_before = begin_common_revalidation(&evidence, journal)?;
        let attempted = prepared.attempt_exchange_once(&evidence.installation, &evidence.record);
        let reconciliation = match attempted {
            Ok(attempted) => attempted.reconcile(&evidence.installation, &evidence.record),
            Err(source) => {
                let _ = finish_common_revalidation(&evidence, journal, database_before);
                return Err(source.into());
            }
        };
        finish_common_revalidation(&evidence, journal, database_before)?;

        Ok(match reconciliation {
            ActiveReblitCommitCleanupExchangeReconciliation::Applied(namespace) => {
                ActiveReblitCommitCleanupApplyReconciliation::Applied(
                    ActiveReblitCommitCleanupPendingDurabilityAuthority {
                        evidence,
                        namespace: namespace.into_durability(),
                    },
                )
            }
            ActiveReblitCommitCleanupExchangeReconciliation::NotApplied => {
                ActiveReblitCommitCleanupApplyReconciliation::NotApplied
            }
            ActiveReblitCommitCleanupExchangeReconciliation::Ambiguous => {
                ActiveReblitCommitCleanupApplyReconciliation::Ambiguous
            }
        })
    }
}

impl<'reservation> ActiveReblitCommitCleanupFinishEffectAuthority<'reservation> {
    /// Enter the shared Finish suffix without issuing an exchange attempt or
    /// any duplicate preliminary synchronization sequence.
    pub(in crate::client) fn into_durability(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation>,
        ActiveReblitCommitCleanupEffectError,
    > {
        let Self {
            _evidence: evidence,
            _namespace: namespace,
        } = self;
        let database_before = begin_common_revalidation(&evidence, journal)?;
        let namespace = namespace.into_durability(&evidence.installation, &evidence.record);
        let trailing = finish_common_revalidation(&evidence, journal, database_before);
        let namespace = namespace?;
        trailing?;
        Ok(ActiveReblitCommitCleanupPendingDurabilityAuthority { evidence, namespace })
    }
}

impl<'reservation> ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation> {
    /// Consume both Applied and Finish origins through the identical fixed
    /// durability sequence.
    pub(in crate::client) fn complete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupDurableAuthority<'reservation>,
        ActiveReblitCommitCleanupEffectError,
    > {
        let Self { evidence, namespace } = self;
        let database_before = begin_common_revalidation(&evidence, journal)?;
        let durable_namespace = namespace.complete(&evidence.installation, &evidence.record);
        let trailing = finish_common_revalidation(&evidence, journal, database_before);
        let namespace = durable_namespace?;
        trailing?;
        let database_before = begin_common_revalidation(&evidence, journal)?;
        namespace.revalidate(&evidence.installation, &evidence.record)?;
        finish_common_revalidation(&evidence, journal, database_before)?;
        Ok(ActiveReblitCommitCleanupDurableAuthority { evidence, namespace })
    }
}

impl ActiveReblitCommitCleanupDurableAuthority<'_> {
    /// Freshly revalidate durable evidence without repeating any barrier.
    #[allow(dead_code)]
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitCommitCleanupEffectError> {
        let database_before = begin_common_revalidation(&self.evidence, journal)?;
        self.namespace
            .revalidate(&self.evidence.installation, &self.evidence.record)?;
        finish_common_revalidation(&self.evidence, journal, database_before)?;
        Ok(())
    }
}

fn begin_common_revalidation(
    evidence: &ActiveReblitCommitCleanupCommonEvidence<'_>,
    journal: &TransitionJournalStore,
) -> Result<ActiveReblitCommitCleanupDatabaseEvidence, ActiveReblitCommitCleanupAuthorityError> {
    require_exact_record_binding(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    evidence.installation.revalidate_mutable_namespace()?;
    let database = require_exact_database(
        &evidence.database,
        inspect_current_database(&evidence.record, evidence.receipt_pair, &evidence.state_db)?,
    )?;
    require_exact_active_state(&evidence.record, &evidence.installation, &evidence.active_state)?;
    if !record_plan_is_exact(&evidence.record, evidence.receipt_pair) {
        return Err(super::ActiveReblitCommitCleanupAuthorityErrorKind::RouteEvidenceChanged.into());
    }
    Ok(database)
}

fn finish_common_revalidation(
    evidence: &ActiveReblitCommitCleanupCommonEvidence<'_>,
    journal: &TransitionJournalStore,
    database_before: ActiveReblitCommitCleanupDatabaseEvidence,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    let database_after = require_exact_database(
        &evidence.database,
        inspect_current_database(&evidence.record, evidence.receipt_pair, &evidence.state_db)?,
    )?;
    require_exact_active_state(&evidence.record, &evidence.installation, &evidence.active_state)?;
    if database_before != database_after || !record_plan_is_exact(&evidence.record, evidence.receipt_pair) {
        return Err(super::ActiveReblitCommitCleanupAuthorityErrorKind::RouteEvidenceChanged.into());
    }
    require_exact_record_binding(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    evidence.installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitCommitCleanupEffectError(
    ActiveReblitCommitCleanupEffectErrorKind,
);

impl From<ActiveReblitCommitCleanupAuthorityError> for ActiveReblitCommitCleanupEffectError {
    fn from(source: ActiveReblitCommitCleanupAuthorityError) -> Self {
        Self(ActiveReblitCommitCleanupEffectErrorKind::Authority(source))
    }
}

impl From<NamespaceEffectError> for ActiveReblitCommitCleanupEffectError {
    fn from(source: NamespaceEffectError) -> Self {
        Self(ActiveReblitCommitCleanupEffectErrorKind::Namespace(source))
    }
}

impl From<ActiveReblitCommitCleanupDurabilityError> for ActiveReblitCommitCleanupEffectError {
    fn from(source: ActiveReblitCommitCleanupDurabilityError) -> Self {
        Self(ActiveReblitCommitCleanupEffectErrorKind::Durability(source))
    }
}

#[derive(Debug, thiserror::Error)]
enum ActiveReblitCommitCleanupEffectErrorKind {
    #[error("revalidate exact ActiveReblit CommitDecided authority around cleanup")]
    Authority(#[source] ActiveReblitCommitCleanupAuthorityError),
    #[error("prepare or reconcile the exact one-shot ActiveReblit cleanup exchange")]
    Namespace(#[source] NamespaceEffectError),
    #[error("complete the fixed ActiveReblit cleanup durability suffix")]
    Durability(#[source] ActiveReblitCommitCleanupDurabilityError),
}
