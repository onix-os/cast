//! Effect-free durable entry into ActiveReblit boot synchronization.
//!
//! The production entry accepts the existing lifetime-bound publication plan,
//! its prepared desired inventory, and per-output provenance bindings. It does
//! not accept a detached canonical receipt. The receipt is derived internally
//! from the exact retained pre-boot journal record and the committed database
//! head, staged atomically, then derived again from the same bound inputs
//! immediately before the receipt-bearing `BootSyncStarted` journal advance.
//!
//! This component never opens a boot destination, writes or removes an output,
//! invokes legacy boot synchronization, or turns receipt claim data into effect
//! authority. SQLite and the journal filesystem cannot share one transaction,
//! so cross-store uncertainty drops and reopens the journal for exact
//! predecessor-or-successor classification. Any failure to classify is
//! reported explicitly as a fail-stop reconciliation error.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{
        BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
        CanonicalBootPublicationReceipt,
    },
    db::state::{
        BootPublicationReceiptStageOutcome, BootPublicationReceiptStateError, Database,
    },
    installation,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    Client,
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_publication_receipt::{
        ActiveReblitBootPublicationReceiptError,
        BorrowedActiveReblitBootPublicationProvenanceClaim,
    },
    active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
};

/// Exact durable journal record observed after a fail-stop boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitBootSyncRecord {
    Predecessor,
    BootSyncStarted,
}

/// Successful cross-store staging with the exact returned successor inode.
///
/// The journal store must outlive its record binding, so both remain owned by
/// this non-cloneable typestate until a later coordinator consumes them.
#[derive(Debug)]
pub(in crate::client) struct StagedActiveReblitBootSync {
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    database_outcome: BootPublicationReceiptStageOutcome,
    receipt: CanonicalBootPublicationReceipt,
    journal: TransitionJournalStore,
    database: Database,
    // Deliberately last so its global lock outlives the journal and database.
    installation: Installation,
}

/// Fresh read-only correlation of one staged receipt with its exact client.
///
/// Private fields prevent sibling components from manufacturing this view.
/// It carries no destination descriptor and grants no publication, promotion,
/// replacement, removal, or deletion authority.
pub(in crate::client) struct FreshStagedActiveReblitBootSync<'staged, 'client> {
    staged: &'staged StagedActiveReblitBootSync,
    _client: &'client Client,
}

impl StagedActiveReblitBootSync {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        &self.record
    }

    pub(in crate::client) const fn database_outcome(&self) -> BootPublicationReceiptStageOutcome {
        self.database_outcome
    }

    pub(in crate::client) const fn receipt(&self) -> &CanonicalBootPublicationReceipt {
        &self.receipt
    }

    pub(in crate::client) const fn receipt_fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.receipt.fingerprint()
    }

    /// Revalidate this exact staged result against the client which owns its
    /// retained installation and state-database capabilities.
    pub(in crate::client) fn revalidate_against<'staged, 'client>(
        &'staged self,
        client: &'client Client,
    ) -> Result<
        FreshStagedActiveReblitBootSync<'staged, 'client>,
        ActiveReblitBootSyncFreshValidationError,
    > {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(ActiveReblitBootSyncFreshValidationError::ClientCapabilityMismatch);
        }
        validate_staged_successor(
            &self.installation,
            &self.database,
            &self.journal,
            &self.record,
            &self.record_binding,
            &self.receipt,
            receipt_pair(&self.receipt),
        )?;
        Ok(FreshStagedActiveReblitBootSync {
            staged: self,
            _client: client,
        })
    }

    #[cfg(test)]
    pub(in crate::client) fn into_parts(
        self,
    ) -> (
        TransitionJournalStore,
        TransitionRecord,
        TransitionJournalRecordBinding,
    ) {
        (self.journal, self.record, self.record_binding)
    }
}

impl FreshStagedActiveReblitBootSync<'_, '_> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.staged.record()
    }

    pub(in crate::client) const fn receipt(&self) -> &CanonicalBootPublicationReceipt {
        self.staged.receipt()
    }

    pub(in crate::client) const fn receipt_fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.staged.receipt_fingerprint()
    }
}

impl Client {
    /// Derive and stage one complete bound receipt, then durably enter
    /// `BootSyncStarted` without performing a boot-publication effect.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) fn stage_active_reblit_boot_sync<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >(
        &self,
        plan: &BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        inventory: &PreparedActiveReblitDesiredPublicationInventory,
        provenance_claims: &[BorrowedActiveReblitBootPublicationProvenanceClaim<'_>],
        journal: TransitionJournalStore,
        predecessor: TransitionRecord,
        predecessor_binding: TransitionJournalRecordBinding,
    ) -> Result<StagedActiveReblitBootSync, ActiveReblitBootSyncStagingError> {
        stage_with_retained_stores(
            &self.installation,
            &self.state_db,
            plan,
            inventory,
            provenance_claims,
            journal,
            predecessor,
            predecessor_binding,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_with_retained_stores<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    installation: &Installation,
    database: &Database,
    plan: &BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
    provenance_claims: &[BorrowedActiveReblitBootPublicationProvenanceClaim<'_>],
    journal: TransitionJournalStore,
    predecessor: TransitionRecord,
    predecessor_binding: TransitionJournalRecordBinding,
) -> Result<StagedActiveReblitBootSync, ActiveReblitBootSyncStagingError> {
    if !plan.is_bound_to_installation(installation) {
        return Err(ActiveReblitBootSyncStagingError::PlanInstallationMismatch);
    }
    require_mutable_namespace(installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncStagingError::Installation)?;
    if !journal.has_record_binding(cast, &predecessor_binding, &predecessor)? {
        return Err(ActiveReblitBootSyncStagingError::PredecessorBindingChanged);
    }
    require_active_reblit_predecessor(&predecessor)?;

    // The committed predecessor is database-owned. It is never accepted from
    // the caller or copied from a detached receipt.
    let admitted_state = database
        .boot_publication_receipt_state()
        .map_err(ActiveReblitBootSyncStagingError::DatabaseAdmission)?;
    let committed_predecessor = admitted_state.head().committed();
    let receipt = plan
        .prepare_complete_boot_publication_receipt(
            inventory,
            &predecessor,
            committed_predecessor,
            provenance_claims,
        )
        .map_err(ActiveReblitBootSyncStagingError::ReceiptMapping)?;
    let pair = receipt_pair(&receipt);
    let successor = exact_successor(&predecessor, pair)?;

    let database_outcome = database
        .stage_boot_publication_receipt(&receipt)
        .map_err(ActiveReblitBootSyncStagingError::DatabaseStage)?;
    require_exact_receipt_state(database, &receipt, pair)
        .map_err(ActiveReblitBootSyncStagingError::DatabaseRevalidation)?;

    // Staging can take an arbitrary amount of time. Strictly reload the
    // database-owned head, then reauthenticate the namespace and exact
    // predecessor binding before the terminal pure remap.
    let rederivation_state = database
        .boot_publication_receipt_state()
        .map_err(ActiveReblitBootSyncReceiptStateError::from)
        .map_err(ActiveReblitBootSyncStagingError::DatabaseRevalidation)?;
    let exact_pending = rederivation_state.pending().is_some_and(|pending| {
        pending.fingerprint() == receipt.fingerprint()
            && pending.body() == receipt.body()
            && pending.canonical_body() == receipt.canonical_body()
    });
    if rederivation_state.receipt_pair_for(receipt.body().transition_id()) != Some(pair)
        || !exact_pending
    {
        return Err(ActiveReblitBootSyncStagingError::DatabaseRevalidation(
            ActiveReblitBootSyncReceiptStateError::Mismatch,
        ));
    }
    let rederived_committed_predecessor = rederivation_state.head().committed();
    require_mutable_namespace(installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncStagingError::Installation)?;
    if !journal.has_record_binding(cast, &predecessor_binding, &predecessor)? {
        return Err(ActiveReblitBootSyncStagingError::PredecessorBindingChangedAfterStage);
    }

    // This test seam is deliberately after every external check. The remap
    // and byte-for-byte equality proof are the final work before the bound
    // journal advance, which reauthenticates its source binding internally.
    after_receipt_stage_before_final_rederivation();
    let rederived = plan
        .prepare_complete_boot_publication_receipt(
            inventory,
            &predecessor,
            rederived_committed_predecessor,
            provenance_claims,
        )
        .map_err(ActiveReblitBootSyncStagingError::ReceiptRederivation)?;
    if rederived.fingerprint() != receipt.fingerprint()
        || rederived.body() != receipt.body()
        || rederived.canonical_body() != receipt.canonical_body()
    {
        return Err(ActiveReblitBootSyncStagingError::ReceiptRederivationMismatch);
    }

    match journal.advance_record_binding(cast, predecessor_binding, &successor) {
        Ok(successor_binding) => {
            after_successful_advance_before_validation();
            match validate_staged_successor(
                installation,
                database,
                &journal,
                &successor,
                &successor_binding,
                &rederived,
                pair,
            ) {
                Ok(()) => Ok(StagedActiveReblitBootSync {
                    installation: installation.clone(),
                    database: database.clone(),
                    journal,
                    record: successor,
                    record_binding: successor_binding,
                    database_outcome,
                    receipt: rederived,
                }),
                Err(validation) => {
                    drop(successor_binding);
                    match reconcile_journal(
                        installation,
                        database,
                        journal,
                        &predecessor,
                        &successor,
                        &rederived,
                        pair,
                    ) {
                        Ok(durable) => Err(
                            ActiveReblitBootSyncStagingError::PostAdvanceValidation {
                                durable,
                                validation,
                            },
                        ),
                        Err(reconciliation) => Err(
                            ActiveReblitBootSyncStagingError::PostAdvanceValidationAndReconciliation {
                                validation,
                                reconciliation,
                            },
                        ),
                    }
                }
            }
        }
        Err(source) => match reconcile_journal(
            installation,
            database,
            journal,
            &predecessor,
            &successor,
            &rederived,
            pair,
        ) {
            Ok(durable) => Err(ActiveReblitBootSyncStagingError::JournalAdvance {
                durable,
                source,
            }),
            Err(reconciliation) => {
                Err(ActiveReblitBootSyncStagingError::JournalAdvanceAndReconciliation {
                    advance: source,
                    reconciliation,
                })
            }
        },
    }
}

fn receipt_pair(receipt: &CanonicalBootPublicationReceipt) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    }
}

fn require_active_reblit_predecessor(
    predecessor: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncStagingError> {
    if predecessor.operation != Operation::ActiveReblit {
        return Err(ActiveReblitBootSyncStagingError::WrongOperation {
            actual: predecessor.operation,
        });
    }
    predecessor
        .boot_sync_started_successor(BootPublicationReceiptPair {
            committed: None,
            pending: BootPublicationReceiptFingerprint::from_bytes([0_u8; 32]),
        })
        .map(|_| ())
        .map_err(ActiveReblitBootSyncStagingError::Successor)
}

fn exact_successor(
    predecessor: &TransitionRecord,
    pair: BootPublicationReceiptPair,
) -> Result<TransitionRecord, ActiveReblitBootSyncStagingError> {
    let successor = predecessor
        .boot_sync_started_successor(pair)
        .map_err(ActiveReblitBootSyncStagingError::Successor)?;
    let successor_receipts = successor
        .boot_publication_receipt_correlation()
        .map_err(ActiveReblitBootSyncStagingError::Successor)?;
    if successor.phase != Phase::BootSyncStarted
        || successor.operation != Operation::ActiveReblit
        || successor.transition_id != predecessor.transition_id
        || successor_receipts != Some(pair)
    {
        return Err(ActiveReblitBootSyncStagingError::UnexpectedSuccessor);
    }
    Ok(successor)
}

fn validate_staged_successor(
    installation: &Installation,
    database: &Database,
    journal: &TransitionJournalStore,
    successor: &TransitionRecord,
    successor_binding: &TransitionJournalRecordBinding,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncPostAdvanceValidationError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::Installation)?;
    if !journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::Journal)?
    {
        return Err(ActiveReblitBootSyncPostAdvanceValidationError::SuccessorBindingChanged);
    }
    require_exact_record_receipt_pair(successor, receipt, pair)?;
    require_exact_receipt_state(database, receipt, pair)
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::ReceiptState)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::Installation)?;
    if !journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::Journal)?
    {
        return Err(ActiveReblitBootSyncPostAdvanceValidationError::SuccessorBindingChanged);
    }
    Ok(())
}

fn require_exact_record_receipt_pair(
    record: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    expected: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncPostAdvanceValidationError> {
    let actual = record
        .boot_publication_receipt_correlation()
        .map_err(ActiveReblitBootSyncPostAdvanceValidationError::RecordReceipt)?;
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::BootSyncStarted
        || &record.transition_id != receipt.body().transition_id()
        || actual != Some(expected)
    {
        return Err(ActiveReblitBootSyncPostAdvanceValidationError::RecordReceiptMismatch);
    }
    Ok(())
}

fn require_exact_receipt_state(
    database: &Database,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), ActiveReblitBootSyncReceiptStateError> {
    let state = database.boot_publication_receipt_state()?;
    let exact_body = state.pending().is_some_and(|pending| {
        pending.fingerprint() == receipt.fingerprint()
            && pending.body() == receipt.body()
            && pending.canonical_body() == receipt.canonical_body()
    });
    if state.receipt_pair_for(receipt.body().transition_id()) != Some(pair) || !exact_body {
        return Err(ActiveReblitBootSyncReceiptStateError::Mismatch);
    }
    Ok(())
}

fn reconcile_journal(
    installation: &Installation,
    database: &Database,
    journal: TransitionJournalStore,
    predecessor: &TransitionRecord,
    successor: &TransitionRecord,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<DurableActiveReblitBootSyncRecord, ActiveReblitBootSyncReconciliationError> {
    // Opening another store while the first holds the journal lock would
    // deadlock. Drop it first, then let canonical reopen clean an interrupted
    // exchange residue before strict source-or-successor classification.
    drop(journal);
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    let reopened = TransitionJournalStore::open_in_retained_cast(cast, &installation.root)
        .map_err(ActiveReblitBootSyncReconciliationError::Reopen)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    let actual = reopened
        .load_revalidated_retained_cast(cast)
        .map_err(ActiveReblitBootSyncReconciliationError::Load)?;
    let (durable, expected) = match actual {
        Some(ref actual) if actual == predecessor => {
            (DurableActiveReblitBootSyncRecord::Predecessor, predecessor)
        }
        Some(ref actual) if actual == successor => {
            (DurableActiveReblitBootSyncRecord::BootSyncStarted, successor)
        }
        actual => {
            return Err(ActiveReblitBootSyncReconciliationError::UnexpectedRecord {
                actual: actual.map(Box::new),
            });
        }
    };

    let binding = reopened
        .record_binding(cast, expected)
        .map_err(ActiveReblitBootSyncReconciliationError::Bind)?;
    if !reopened
        .has_record_binding(cast, &binding, expected)
        .map_err(ActiveReblitBootSyncReconciliationError::Bind)?
    {
        return Err(ActiveReblitBootSyncReconciliationError::BindingChanged);
    }
    require_exact_receipt_state(database, receipt, pair)
        .map_err(ActiveReblitBootSyncReconciliationError::ReceiptState)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    if !reopened
        .has_record_binding(cast, &binding, expected)
        .map_err(ActiveReblitBootSyncReconciliationError::Bind)?
    {
        return Err(ActiveReblitBootSyncReconciliationError::BindingChanged);
    }
    Ok(durable)
}

fn require_mutable_namespace(
    installation: &Installation,
) -> Result<(), ActiveReblitBootSyncStagingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncStagingError::Installation)
}

#[cfg(test)]
std::thread_local! {
    static AFTER_RECEIPT_STAGE_BEFORE_FINAL_REDERIVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_receipt_stage_before_final_rederivation(callback: impl FnOnce() + 'static) {
    AFTER_RECEIPT_STAGE_BEFORE_FINAL_REDERIVATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn after_receipt_stage_before_final_rederivation() {
    AFTER_RECEIPT_STAGE_BEFORE_FINAL_REDERIVATION.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(not(test))]
fn after_receipt_stage_before_final_rederivation() {}

#[cfg(test)]
std::thread_local! {
    static AFTER_SUCCESSFUL_ADVANCE_BEFORE_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_successful_advance_before_validation(callback: impl FnOnce() + 'static) {
    AFTER_SUCCESSFUL_ADVANCE_BEFORE_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn after_successful_advance_before_validation() {
    AFTER_SUCCESSFUL_ADVANCE_BEFORE_VALIDATION.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(not(test))]
fn after_successful_advance_before_validation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncReceiptStateError {
    #[error("strictly load the canonical receipt body and pending head")]
    Load(#[from] BootPublicationReceiptStateError),
    #[error("the pending receipt body or head differs from the internally derived receipt")]
    Mismatch,
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncPostAdvanceValidationError {
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("revalidate the exact returned BootSyncStarted journal binding")]
    Journal(#[source] StorageError),
    #[error("the returned BootSyncStarted journal binding changed")]
    SuccessorBindingChanged,
    #[error("decode the retained BootSyncStarted receipt pair")]
    RecordReceipt(#[source] CodecError),
    #[error("the retained BootSyncStarted record does not carry the exact staged receipt pair")]
    RecordReceiptMismatch,
    #[error("revalidate the exact internally derived staged receipt")]
    ReceiptState(#[source] ActiveReblitBootSyncReceiptStateError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncFreshValidationError {
    #[error("the staged boot-sync result belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("revalidate the exact staged boot-sync evidence")]
    Evidence(#[from] ActiveReblitBootSyncPostAdvanceValidationError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncStagingError {
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("revalidate the exact ActiveReblit pre-boot journal binding")]
    JournalBinding(#[from] StorageError),
    #[error("the retained pre-boot journal binding changed before receipt staging")]
    PredecessorBindingChanged,
    #[error("the retained pre-boot journal binding changed after receipt staging")]
    PredecessorBindingChangedAfterStage,
    #[error("the bound publication plan does not retain the exact client installation")]
    PlanInstallationMismatch,
    #[error("boot-sync staging requires ActiveReblit, got {actual:?}")]
    WrongOperation { actual: Operation },
    #[error("derive the exact receipt-bearing BootSyncStarted successor")]
    Successor(#[source] CodecError),
    #[error("the typed boot-sync successor violated its ActiveReblit correlation contract")]
    UnexpectedSuccessor,
    #[error("strictly load receipt state before deriving its committed predecessor")]
    DatabaseAdmission(#[source] BootPublicationReceiptStateError),
    #[error("map the bound plan, desired inventory, and provenance inputs into the staged receipt")]
    ReceiptMapping(#[source] ActiveReblitBootPublicationReceiptError),
    #[error("atomically stage the internally derived canonical receipt body and pending head")]
    DatabaseStage(#[source] BootPublicationReceiptStateError),
    #[error("strictly reload the internally derived canonical receipt body and pending head")]
    DatabaseRevalidation(#[source] ActiveReblitBootSyncReceiptStateError),
    #[error("rederive the receipt from the same bound plan, inventory, provenance, and predecessor")]
    ReceiptRederivation(#[source] ActiveReblitBootPublicationReceiptError),
    #[error("the final internally rederived receipt differs from the staged canonical receipt")]
    ReceiptRederivationMismatch,
    #[error("advance the exact pre-boot journal binding; durable record is {durable:?}")]
    JournalAdvance {
        durable: DurableActiveReblitBootSyncRecord,
        #[source]
        source: StorageError,
    },
    #[error("advance the exact pre-boot journal binding and reconcile its durable state")]
    JournalAdvanceAndReconciliation {
        advance: StorageError,
        #[source]
        reconciliation: ActiveReblitBootSyncReconciliationError,
    },
    #[error(
        "post-advance validation failed; caller must reopen from durable {durable:?} rather than retrying the predecessor"
    )]
    PostAdvanceValidation {
        durable: DurableActiveReblitBootSyncRecord,
        #[source]
        validation: ActiveReblitBootSyncPostAdvanceValidationError,
    },
    #[error("post-advance validation and required journal reopen/reconciliation both failed")]
    PostAdvanceValidationAndReconciliation {
        validation: ActiveReblitBootSyncPostAdvanceValidationError,
        #[source]
        reconciliation: ActiveReblitBootSyncReconciliationError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncReconciliationError {
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("reopen the canonical transition journal after a fail-stop boundary")]
    Reopen(#[source] StorageError),
    #[error("load the exact canonical journal record after a fail-stop boundary")]
    Load(#[source] StorageError),
    #[error("journal reopen found neither the exact predecessor nor exact successor")]
    UnexpectedRecord {
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("bind the reconciled canonical journal record")]
    Bind(#[source] StorageError),
    #[error("the reconciled canonical journal binding changed")]
    BindingChanged,
    #[error("revalidate the exact internally derived receipt while reconciling the journal")]
    ReceiptState(#[source] ActiveReblitBootSyncReceiptStateError),
}

#[cfg(test)]
#[path = "active_reblit_boot_sync_staging_tests.rs"]
mod tests;
