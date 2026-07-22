//! Consuming persistence of exact promoted ActiveReblit boot completion.
//!
//! The caller must supply the unforgeable completion seal minted by terminal
//! publication orchestration. This layer owns only the journal-state change:
//! it consumes the retained `BootSyncStarted` staging authority, derives the
//! receipt-bound typed successor, advances under the inherited plan deadline,
//! and returns a fresh same-store binding captured after a mandatory reopen.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{
        BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
        CanonicalBootPublicationReceipt,
    },
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitBootSyncCompletionSeal,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    },
    db::state::{
        BootPublicationReceiptPromotionError, BootPublicationReceiptStageOutcome,
        Database,
    },
    installation,
    transition_journal::{
        CodecError, Operation, Phase, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ActiveReblitBootSyncPromotedValidationError, StagedActiveReblitBootSync,
};

/// Exact durable journal state proved after a completion failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitBootSyncCompletionRecord {
    BootSyncStarted,
    BootSyncComplete,
}

/// Durable `BootSyncComplete` staging state with a binding owned by its open
/// journal store.
///
/// The exact `BootSyncStarted` predecessor is retained privately so an outer
/// terminal-evidence failure can consume this value into source-or-successor
/// reconciliation. No field or authority is cloneable.
#[must_use = "completed boot-sync staging must be coordinated or deliberately discarded"]
pub(in crate::client) struct CompletedStagedActiveReblitBootSync<
    'plan,
    'inventory,
    Plan,
> {
    predecessor: TransitionRecord,
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    receipt: CanonicalBootPublicationReceipt,
    plan: &'plan Plan,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    staging_outcome: BootPublicationReceiptStageOutcome,
    journal: TransitionJournalStore,
    database: Database,
    // Deliberately last so its global lock outlives the journal and database.
    installation: Installation,
}

/// Fresh read-only correlation of one completed staging result with its exact
/// client capability set.
pub(in crate::client) struct FreshCompletedStagedActiveReblitBootSync<
    'completed,
    'client,
    'plan,
    'inventory,
    Plan,
> {
    completed: &'completed CompletedStagedActiveReblitBootSync<
        'plan,
        'inventory,
        Plan,
    >,
    _client: &'client Client,
}

impl<Plan> std::fmt::Debug
    for CompletedStagedActiveReblitBootSync<'_, '_, Plan>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompletedStagedActiveReblitBootSync")
            .field("record", &self.record)
            .field("receipt", &self.receipt)
            .field("staging_outcome", &self.staging_outcome)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan>
    CompletedStagedActiveReblitBootSync<'plan, 'inventory, Plan>
{
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        &self.record
    }

    pub(in crate::client) const fn receipt(
        &self,
    ) -> &CanonicalBootPublicationReceipt {
        &self.receipt
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.receipt.fingerprint()
    }

    const fn plan(&self) -> &'plan Plan {
        self.plan
    }

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.inventory
    }

    pub(in crate::client) const fn staging_outcome(
        &self,
    ) -> BootPublicationReceiptStageOutcome {
        self.staging_outcome
    }

    pub(in crate::client) fn revalidate_against<'completed, 'client>(
        &'completed self,
        client: &'client Client,
    ) -> Result<
        FreshCompletedStagedActiveReblitBootSync<
            'completed,
            'client,
            'plan,
            'inventory,
            Plan,
        >,
        ActiveReblitBootSyncCompleteValidationError,
    > {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(
                ActiveReblitBootSyncCompleteValidationError::ClientCapabilityMismatch,
            );
        }
        validate_completed_successor(
            &self.installation,
            &self.database,
            &self.journal,
            &self.record,
            &self.record_binding,
            &self.receipt,
            receipt_pair(&self.receipt),
        )?;
        Ok(FreshCompletedStagedActiveReblitBootSync {
            completed: self,
            _client: client,
        })
    }

    /// Consume a completed token after an outer terminal/topology validation
    /// failure and return only its durable Started-or-Complete classification.
    pub(in crate::client) fn reconcile_after_completed_validation_failure(
        self,
    ) -> Result<
        DurableActiveReblitBootSyncCompletionRecord,
        ActiveReblitBootSyncCompletionReconciliationError,
    > {
        let Self {
            predecessor,
            record,
            record_binding,
            receipt,
            journal,
            database,
            installation,
            ..
        } = self;
        drop(record_binding);
        drop(journal);
        reconcile_completion_journal(
            &installation,
            &database,
            &predecessor,
            &record,
            &receipt,
        )
    }
}

impl<'plan, 'inventory, Plan>
    FreshCompletedStagedActiveReblitBootSync<'_, '_, 'plan, 'inventory, Plan>
{
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.completed.record()
    }

    pub(in crate::client) const fn receipt(
        &self,
    ) -> &CanonicalBootPublicationReceipt {
        self.completed.receipt()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.completed.receipt_fingerprint()
    }

    pub(in crate::client) const fn plan(&self) -> &'plan Plan {
        self.completed.plan()
    }

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.completed.inventory()
    }

    pub(in crate::client) const fn staging_outcome(
        &self,
    ) -> BootPublicationReceiptStageOutcome {
        self.completed.staging_outcome()
    }
}

impl<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >
where
    'input: 'plan,
{
    /// Consume exact promoted `BootSyncStarted` staging into one deadline-bound
    /// `BootSyncComplete` record. Only terminal orchestration can mint `seal`.
    pub(in crate::client) fn persist_boot_sync_complete(
        self,
        client: &Client,
        _seal: ActiveReblitBootSyncCompletionSeal,
    ) -> Result<
        CompletedStagedActiveReblitBootSync<
            'plan,
            'inventory,
            BoundActiveReblitBlsPublicationPlan<
                'input,
                'topology_view,
                'topology_authority,
                'attempt,
                'stone,
                'roots,
            >,
        >,
        ActiveReblitBootSyncCompletePersistenceError,
    > {
        let deadline = require_promoted_staging_admission(&self, client)?;
        let pair = receipt_pair(&self.receipt);
        let successor = exact_boot_sync_complete_successor(
            &self.record,
            &self.receipt,
            pair,
        )?;

        let repeated_deadline = require_promoted_staging_admission(&self, client)?;
        if repeated_deadline != deadline {
            return Err(
                ActiveReblitBootSyncCompletePersistenceError::PlanDeadlineMismatch {
                    expected: deadline,
                    actual: repeated_deadline,
                },
            );
        }

        let StagedActiveReblitBootSync {
            record: predecessor,
            record_binding: predecessor_binding,
            database_outcome: staging_outcome,
            receipt,
            plan,
            inventory,
            journal,
            database,
            installation,
            prepared_delta: _,
            classified_delta: _,
        } = self;
        let cast = installation
            .retained_mutable_cast_directory()
            .map_err(ActiveReblitBootSyncCompletePersistenceError::Installation)?;

        match journal.advance_record_binding_until(
            cast,
            predecessor_binding,
            &successor,
            deadline,
        ) {
            Ok(successor_binding) => match finish_successful_completion_advance(
                &installation,
                &database,
                &predecessor,
                &successor,
                &receipt,
                journal,
                successor_binding,
            ) {
                Ok((journal, record_binding)) => {
                    Ok(CompletedStagedActiveReblitBootSync {
                        predecessor,
                        record: successor,
                        record_binding,
                        receipt,
                        plan,
                        inventory,
                        staging_outcome,
                        journal,
                        database,
                        installation,
                    })
                }
                Err(validation) => match reconcile_completion_journal(
                    &installation,
                    &database,
                    &predecessor,
                    &successor,
                    &receipt,
                ) {
                    Ok(durable) => Err(
                        ActiveReblitBootSyncCompletePersistenceError::PostAdvanceValidation {
                            durable,
                            validation,
                        },
                    ),
                    Err(reconciliation) => Err(
                        ActiveReblitBootSyncCompletePersistenceError::PostAdvanceValidationAndReconciliation {
                            validation,
                            reconciliation,
                        },
                    ),
                },
            },
            Err(source) => {
                drop(journal);
                match reconcile_completion_journal(
                    &installation,
                    &database,
                    &predecessor,
                    &successor,
                    &receipt,
                ) {
                    Ok(durable) => Err(
                        ActiveReblitBootSyncCompletePersistenceError::JournalAdvance {
                            durable,
                            source,
                        },
                    ),
                    Err(reconciliation) => Err(
                        ActiveReblitBootSyncCompletePersistenceError::JournalAdvanceAndReconciliation {
                            advance: source,
                            reconciliation,
                        },
                    ),
                }
            }
        }
    }
}

fn require_promoted_staging_admission<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    staged: &StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    client: &Client,
) -> Result<std::time::Instant, ActiveReblitBootSyncCompletePersistenceError>
where
    'input: 'plan,
{
    let retained_plan = staged.plan;
    let fresh = staged
        .revalidate_promoted_against(client)
        .map_err(ActiveReblitBootSyncCompletePersistenceError::PromotedAdmission)?;
    if !std::ptr::eq(fresh.plan(), retained_plan) {
        return Err(ActiveReblitBootSyncCompletePersistenceError::PlanMismatch);
    }
    Ok(fresh.plan().input_deadline())
}

fn exact_boot_sync_complete_successor(
    predecessor: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<TransitionRecord, ActiveReblitBootSyncCompletePersistenceError> {
    require_exact_started_record(predecessor, receipt, pair)
        .map_err(ActiveReblitBootSyncCompletePersistenceError::PredecessorValidation)?;
    let successor = predecessor
        .boot_sync_complete_successor(pair)
        .map_err(ActiveReblitBootSyncCompletePersistenceError::Successor)?;
    let expected_generation = predecessor
        .generation
        .checked_add(1)
        .ok_or(ActiveReblitBootSyncCompletePersistenceError::UnexpectedSuccessor)?;
    if successor.operation != Operation::ActiveReblit
        || successor.phase != Phase::BootSyncComplete
        || successor.transition_id != predecessor.transition_id
        || successor.generation != expected_generation
        || successor
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitBootSyncCompletePersistenceError::Successor)?
            != Some(pair)
    {
        return Err(ActiveReblitBootSyncCompletePersistenceError::UnexpectedSuccessor);
    }
    Ok(successor)
}

fn finish_successful_completion_advance(
    installation: &Installation,
    database: &Database,
    predecessor: &TransitionRecord,
    successor: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    journal: TransitionJournalStore,
    successor_binding: TransitionJournalRecordBinding,
) -> Result<
    (TransitionJournalStore, TransitionJournalRecordBinding),
    ActiveReblitBootSyncCompleteValidationError,
> {
    let pair = receipt_pair(receipt);
    validate_completed_successor(
        installation,
        database,
        &journal,
        successor,
        &successor_binding,
        receipt,
        pair,
    )?;

    drop(journal);
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    let reopened = TransitionJournalStore::open_in_retained_cast(
        cast,
        &installation.root,
    )
    .map_err(ActiveReblitBootSyncCompleteValidationError::Reopen)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    if !reopened
        .has_reopened_record_binding(cast, &successor_binding, successor)
        .map_err(ActiveReblitBootSyncCompleteValidationError::Journal)?
    {
        return Err(
            ActiveReblitBootSyncCompleteValidationError::ReopenedSuccessorBindingChanged,
        );
    }
    require_exact_started_record(predecessor, receipt, pair)?;
    let recaptured = reopened
        .record_binding(cast, successor)
        .map_err(ActiveReblitBootSyncCompleteValidationError::RecaptureBinding)?;
    drop(successor_binding);
    validate_completed_successor(
        installation,
        database,
        &reopened,
        successor,
        &recaptured,
        receipt,
        pair,
    )?;
    Ok((reopened, recaptured))
}

fn validate_completed_successor(
    installation: &Installation,
    database: &Database,
    journal: &TransitionJournalStore,
    successor: &TransitionRecord,
    successor_binding: &TransitionJournalRecordBinding,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncCompleteValidationError> {
    if !journal.has_record_store_binding(successor_binding) {
        return Err(
            ActiveReblitBootSyncCompleteValidationError::JournalCapabilityMismatch,
        );
    }
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    if !journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(ActiveReblitBootSyncCompleteValidationError::Journal)?
    {
        return Err(ActiveReblitBootSyncCompleteValidationError::BindingChanged);
    }
    require_exact_completed_record(successor, receipt, pair)?;
    database
        .require_promoted_boot_publication_receipt(receipt)
        .map_err(ActiveReblitBootSyncCompleteValidationError::ReceiptState)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompleteValidationError::Installation)?;
    if !journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(ActiveReblitBootSyncCompleteValidationError::Journal)?
    {
        return Err(ActiveReblitBootSyncCompleteValidationError::BindingChanged);
    }
    Ok(())
}

fn reconcile_completion_journal(
    installation: &Installation,
    database: &Database,
    predecessor: &TransitionRecord,
    successor: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<
    DurableActiveReblitBootSyncCompletionRecord,
    ActiveReblitBootSyncCompletionReconciliationError,
> {
    let pair = receipt_pair(receipt);
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Installation)?;
    let reopened = TransitionJournalStore::open_in_retained_cast(
        cast,
        &installation.root,
    )
    .map_err(ActiveReblitBootSyncCompletionReconciliationError::Reopen)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Installation)?;
    let actual = reopened
        .load_revalidated_retained_cast(cast)
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Load)?;
    let (durable, expected) = match actual {
        Some(ref actual) if actual == predecessor => {
            require_exact_started_record(predecessor, receipt, pair)?;
            (
                DurableActiveReblitBootSyncCompletionRecord::BootSyncStarted,
                predecessor,
            )
        }
        Some(ref actual) if actual == successor => {
            require_exact_completed_record(successor, receipt, pair)?;
            (
                DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
                successor,
            )
        }
        actual => {
            return Err(
                ActiveReblitBootSyncCompletionReconciliationError::UnexpectedRecord {
                    actual: actual.map(Box::new),
                },
            );
        }
    };
    let binding = reopened
        .record_binding(cast, expected)
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Bind)?;
    if !reopened
        .has_record_binding(cast, &binding, expected)
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Bind)?
    {
        return Err(
            ActiveReblitBootSyncCompletionReconciliationError::BindingChanged,
        );
    }
    database
        .require_promoted_boot_publication_receipt(receipt)
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::ReceiptState)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Installation)?;
    if !reopened
        .has_record_binding(cast, &binding, expected)
        .map_err(ActiveReblitBootSyncCompletionReconciliationError::Bind)?
    {
        return Err(
            ActiveReblitBootSyncCompletionReconciliationError::BindingChanged,
        );
    }
    Ok(durable)
}

fn require_exact_started_record(
    record: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncCompleteValidationError> {
    require_exact_record(record, receipt, pair, Phase::BootSyncStarted)
}

fn require_exact_completed_record(
    record: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncCompleteValidationError> {
    require_exact_record(record, receipt, pair, Phase::BootSyncComplete)
}

fn require_exact_record(
    record: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
    phase: Phase,
) -> Result<(), ActiveReblitBootSyncCompleteValidationError> {
    let actual = record
        .boot_publication_receipt_correlation()
        .map_err(ActiveReblitBootSyncCompleteValidationError::RecordReceipt)?;
    if record.operation != Operation::ActiveReblit
        || record.phase != phase
        || &record.transition_id != receipt.body().transition_id()
        || actual != Some(pair)
    {
        return Err(
            ActiveReblitBootSyncCompleteValidationError::RecordReceiptMismatch,
        );
    }
    Ok(())
}

fn receipt_pair(
    receipt: &CanonicalBootPublicationReceipt,
) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCompleteValidationError {
    #[error("the completed boot-sync result belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("the completed record binding belongs to a different journal store")]
    JournalCapabilityMismatch,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("revalidate the exact completed journal binding")]
    Journal(#[source] StorageError),
    #[error("the completed journal binding changed")]
    BindingChanged,
    #[error("decode the completed journal receipt pair")]
    RecordReceipt(#[source] CodecError),
    #[error("the completed journal record does not carry the exact promoted receipt pair")]
    RecordReceiptMismatch,
    #[error("require the exact canonical receipt as promoted with no pending head")]
    ReceiptState(#[source] BootPublicationReceiptPromotionError),
    #[error("reopen the journal after the successful completion advance")]
    Reopen(#[source] StorageError),
    #[error("the reopened journal no longer names the exact returned successor inode")]
    ReopenedSuccessorBindingChanged,
    #[error("recapture a same-store binding for the reopened completed journal")]
    RecaptureBinding(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCompletionReconciliationError {
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("reopen the canonical journal after a completion boundary")]
    Reopen(#[source] StorageError),
    #[error("load the canonical journal after a completion boundary")]
    Load(#[source] StorageError),
    #[error("journal reopen found neither exact BootSyncStarted nor exact BootSyncComplete")]
    UnexpectedRecord {
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("bind the reconciled canonical completion record")]
    Bind(#[source] StorageError),
    #[error("the reconciled canonical completion binding changed")]
    BindingChanged,
    #[error("validate the exact reconciled receipt-bearing record")]
    RecordValidation(#[from] ActiveReblitBootSyncCompleteValidationError),
    #[error("require the exact receipt remains promoted while reconciling completion")]
    ReceiptState(#[source] BootPublicationReceiptPromotionError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCompletePersistenceError {
    #[error("admit exact promoted BootSyncStarted staging")]
    PromotedAdmission(#[source] ActiveReblitBootSyncPromotedValidationError),
    #[error("promoted staging revalidation returned a different retained plan")]
    PlanMismatch,
    #[error("the retained plan deadline changed between promoted staging checks")]
    PlanDeadlineMismatch {
        expected: std::time::Instant,
        actual: std::time::Instant,
    },
    #[error("validate the exact receipt-bearing BootSyncStarted predecessor")]
    PredecessorValidation(#[source] ActiveReblitBootSyncCompleteValidationError),
    #[error("derive the exact receipt-bound BootSyncComplete successor")]
    Successor(#[source] CodecError),
    #[error("the typed BootSyncComplete successor violated its ActiveReblit contract")]
    UnexpectedSuccessor,
    #[error("access the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("advance the exact journal binding; durable record is {durable:?}")]
    JournalAdvance {
        durable: DurableActiveReblitBootSyncCompletionRecord,
        #[source]
        source: StorageError,
    },
    #[error("advance the exact journal binding and reconcile its durable completion state")]
    JournalAdvanceAndReconciliation {
        advance: StorageError,
        #[source]
        reconciliation: ActiveReblitBootSyncCompletionReconciliationError,
    },
    #[error("post-advance validation failed; durable record is {durable:?}")]
    PostAdvanceValidation {
        durable: DurableActiveReblitBootSyncCompletionRecord,
        #[source]
        validation: ActiveReblitBootSyncCompleteValidationError,
    },
    #[error("post-advance validation and required completion reconciliation both failed")]
    PostAdvanceValidationAndReconciliation {
        validation: ActiveReblitBootSyncCompleteValidationError,
        #[source]
        reconciliation: ActiveReblitBootSyncCompletionReconciliationError,
    },
}
