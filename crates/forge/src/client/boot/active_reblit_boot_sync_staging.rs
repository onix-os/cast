//! Effect-free durable entry into ActiveReblit boot synchronization.
//!
//! The production entry accepts the existing lifetime-bound publication plan
//! and its prepared desired inventory. It does not accept provenance claims or
//! a detached canonical receipt. Instead it authenticates the complete current
//! installed chain from one strict database snapshot, prepares the installed-
//! versus-desired delta, classifies it through a bound read-only namespace
//! preflight, and derives receipt claims internally. The receipt is staged
//! atomically, then derived again from the same bound inputs immediately before
//! the receipt-bearing `BootSyncStarted` journal advance.
//!
//! This component opens boot destinations only for bound read-only assessment;
//! it never writes, publishes, replaces, or removes an output, invokes legacy
//! boot synchronization, or turns receipt claim data into effect authority.
//! SQLite and the journal filesystem cannot share one transaction,
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
        BootPublicationReceiptStageOutcome, BootPublicationReceiptStateError,
        CurrentExactPromotedBootPublicationReceiptChainError, Database,
    },
    installation,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    Client, CoordinatorActiveStateReservation,
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_publication_receipt::ActiveReblitBootPublicationReceiptError,
    active_reblit_boot_publication_preflight::ActiveReblitBootPublicationPreflightError,
    active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    active_reblit_installed_boot_publication_delta::{
        ActiveReblitBootPublicationDeltaError,
        AuthenticatedActiveReblitInstalledBootPublication,
        ClassifiedActiveReblitBootPublicationDelta,
        PreparedActiveReblitBootPublicationDelta,
    },
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
pub(in crate::client) struct StagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    database_outcome: BootPublicationReceiptStageOutcome,
    receipt: CanonicalBootPublicationReceipt,
    prepared_delta: PreparedActiveReblitBootPublicationDelta,
    classified_delta: ClassifiedActiveReblitBootPublicationDelta,
    plan: &'plan Plan,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    journal: TransitionJournalStore,
    database: Database,
    // Both retained locks outlive the journal and database; the continuously
    // held writer reservation is deliberately last.
    installation: Installation,
    active_state_reservation: CoordinatorActiveStateReservation,
}

/// Fresh read-only correlation of one staged receipt with its exact client.
///
/// Private fields prevent sibling components from manufacturing this view.
/// It carries no destination descriptor and grants no publication, promotion,
/// replacement, removal, or deletion authority.
pub(in crate::client) struct FreshStagedActiveReblitBootSync<
    'staged,
    'client,
    'plan,
    'inventory,
    Plan,
> {
    staged: &'staged StagedActiveReblitBootSync<'plan, 'inventory, Plan>,
    _client: &'client Client,
}

impl<Plan> std::fmt::Debug for StagedActiveReblitBootSync<'_, '_, Plan> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedActiveReblitBootSync")
            .field("record", &self.record)
            .field("database_outcome", &self.database_outcome)
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan> StagedActiveReblitBootSync<'plan, 'inventory, Plan> {
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
        FreshStagedActiveReblitBootSync<
            'staged,
            'client,
            'plan,
            'inventory,
            Plan,
        >,
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

impl<'plan, 'inventory, Plan>
    FreshStagedActiveReblitBootSync<'_, '_, 'plan, 'inventory, Plan>
{
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.staged.record()
    }

    pub(in crate::client) const fn receipt(&self) -> &CanonicalBootPublicationReceipt {
        self.staged.receipt()
    }

    pub(in crate::client) const fn receipt_fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.staged.receipt_fingerprint()
    }

    pub(in crate::client) const fn plan(&self) -> &'plan Plan {
        self.staged.plan
    }

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.staged.inventory
    }

    /// Borrow the exact authenticated installed-versus-desired union retained
    /// at staging. This inert value grants no filesystem or cleanup authority.
    pub(in crate::client) const fn prepared_delta(
        &self,
    ) -> &PreparedActiveReblitBootPublicationDelta {
        &self.staged.prepared_delta
    }

    /// Borrow the initial sealed live classification retained at staging.
    /// Later effect code must recapture and compare it before any mutation.
    pub(in crate::client) const fn classified_delta(
        &self,
    ) -> &ClassifiedActiveReblitBootPublicationDelta {
        &self.staged.classified_delta
    }
}

impl Client {
    /// Derive and stage one complete bound receipt, then durably enter
    /// `BootSyncStarted` without performing a boot-publication effect.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) fn stage_active_reblit_boot_sync<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >(
        &self,
        plan: &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
        journal: TransitionJournalStore,
        predecessor: TransitionRecord,
        predecessor_binding: TransitionJournalRecordBinding,
    ) -> Result<
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
        >,
        ActiveReblitBootSyncStagingError,
    > {
        stage_with_retained_stores(
            &self.installation,
            &self.state_db,
            plan,
            inventory,
            journal,
            predecessor,
            predecessor_binding,
        )
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn stage_with_retained_stores<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    installation: &Installation,
    database: &Database,
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    journal: TransitionJournalStore,
    predecessor: TransitionRecord,
    predecessor_binding: TransitionJournalRecordBinding,
) -> Result<
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
    >,
    ActiveReblitBootSyncStagingError,
> {
    let active_state_reservation = CoordinatorActiveStateReservation::acquire()
        .map_err(ActiveReblitBootSyncStagingError::FixtureWriterReservation)?;
    stage_with_retained_stores_and_reservation(
        active_state_reservation,
        installation,
        installation.clone(),
        database.clone(),
        plan,
        inventory,
        journal,
        predecessor,
        predecessor_binding,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_with_retained_stores_and_reservation<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    active_state_reservation: CoordinatorActiveStateReservation,
    plan_installation: &Installation,
    retained_installation: Installation,
    retained_database: Database,
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    journal: TransitionJournalStore,
    predecessor: TransitionRecord,
    predecessor_binding: TransitionJournalRecordBinding,
) -> Result<
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
    >,
    ActiveReblitBootSyncStagingError,
> {
    if !plan.is_bound_to_installation(plan_installation) {
        return Err(ActiveReblitBootSyncStagingError::PlanInstallationMismatch);
    }
    let installation = &retained_installation;
    let database = &retained_database;
    require_mutable_namespace(installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncStagingError::Installation)?;
    if !journal.has_record_binding(cast, &predecessor_binding, &predecessor)? {
        return Err(ActiveReblitBootSyncStagingError::PredecessorBindingChanged);
    }
    require_active_reblit_predecessor(&predecessor)?;

    // Installed ownership and predecessor identity come only from one strict
    // database-authenticated current-chain snapshot. Empty means both the head
    // and immutable receipt store are empty; pending or dangling state fails.
    let current_chain = database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .map_err(ActiveReblitBootSyncStagingError::CurrentInstalledChain)?;
    let installed =
        AuthenticatedActiveReblitInstalledBootPublication::from_current_exact_promoted_chain(
            &current_chain,
        );
    let committed_predecessor = installed
        .receipt()
        .map(CanonicalBootPublicationReceipt::fingerprint);
    let prepared_delta = plan
        .prepare_installed_boot_publication_delta(inventory, installed)
        .map_err(ActiveReblitBootSyncStagingError::DeltaPreparation)?;
    let preflight = plan
        .prepare_boot_publication_preflight()
        .map_err(ActiveReblitBootSyncStagingError::InitialPreflight)?;
    let classified_delta = preflight
        .classify_installed_boot_publication_delta(&prepared_delta)
        .map_err(ActiveReblitBootSyncStagingError::DeltaClassification)?;
    let provenance_claims = classified_delta
        .derive_receipt_provenance_claims(inventory)
        .map_err(ActiveReblitBootSyncStagingError::ReceiptClaimDerivation)?;
    let receipt = plan
        .prepare_complete_boot_publication_receipt(
            inventory,
            &predecessor,
            committed_predecessor,
            &provenance_claims,
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
            &provenance_claims,
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
                    installation: retained_installation,
                    database: retained_database,
                    journal,
                    record: successor,
                    record_binding: successor_binding,
                    database_outcome,
                    receipt: rederived,
                    prepared_delta,
                    classified_delta,
                    plan,
                    inventory,
                    active_state_reservation,
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
    // deadlock. Drop it first, then use a nonblocking canonical reopen: the
    // writer reservation remains held, so waiting for a journal contender
    // which may itself be waiting for that writer would invert lock order.
    drop(journal);
    after_old_journal_drop_before_reopen();
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncReconciliationError::Installation)?;
    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root)
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
    static AFTER_OLD_JOURNAL_DROP_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_successful_advance_before_validation(callback: impl FnOnce() + 'static) {
    AFTER_SUCCESSFUL_ADVANCE_BEFORE_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn arm_after_old_journal_drop_before_reopen(callback: impl FnOnce() + 'static) {
    AFTER_OLD_JOURNAL_DROP_BEFORE_REOPEN.with(|slot| {
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

#[cfg(test)]
fn after_old_journal_drop_before_reopen() {
    AFTER_OLD_JOURNAL_DROP_BEFORE_REOPEN.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(not(test))]
fn after_old_journal_drop_before_reopen() {}

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
    #[cfg(test)]
    #[error("reserve the cooperating writer for a direct staging fixture")]
    FixtureWriterReservation(#[source] crate::client::Error),
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
    #[error("authenticate the complete current installed boot-publication receipt chain")]
    CurrentInstalledChain(#[source] CurrentExactPromotedBootPublicationReceiptChainError),
    #[error("prepare the authenticated installed-versus-desired boot-publication delta")]
    DeltaPreparation(#[source] ActiveReblitBootPublicationDeltaError),
    #[error("perform the bound read-only boot-publication preflight before receipt staging")]
    InitialPreflight(#[source] ActiveReblitBootPublicationPreflightError),
    #[error("classify the installed delta from the sealed live preflight")]
    DeltaClassification(#[source] ActiveReblitBootPublicationDeltaError),
    #[error("derive receipt provenance claims from the classified installed delta")]
    ReceiptClaimDerivation(#[source] ActiveReblitBootPublicationDeltaError),
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

#[path = "active_reblit_boot_sync_staging/coordinator_handoff.rs"]
mod coordinator_handoff;
pub(crate) use coordinator_handoff::CoordinatorActiveReblitBootSyncHandoff;
#[cfg(test)]
pub(in crate::client) use coordinator_handoff::stage_active_reblit_boot_sync_from_handoff_for_test;
#[allow(unused_imports)] // returned by the intentionally unwired coordinator entry
pub(in crate::client) use coordinator_handoff::ActiveReblitCoordinatorBootSyncStagingError;

#[path = "active_reblit_boot_sync_staging/immutable_publication_attempt.rs"]
mod immutable_publication_attempt;

#[path = "active_reblit_boot_sync_staging/promoted_receipt_validation.rs"]
mod promoted_receipt_validation;
pub(in crate::client) use promoted_receipt_validation::ActiveReblitBootSyncPromotedValidationError;

#[path = "active_reblit_boot_sync_staging/boot_sync_complete_persistence.rs"]
mod boot_sync_complete_persistence;
#[allow(unused_imports)] // fresh completed view is consumed by the later commit-coordination slice
pub(in crate::client) use boot_sync_complete_persistence::{
    ActiveReblitBootSyncCompletePersistenceError,
    ActiveReblitBootSyncCompleteValidationError,
    ActiveReblitBootSyncCompletionReconciliationError,
    CompleteStagedActiveReblitFinalizationError,
    CommitCleanupCompleteStagedActiveReblitCompleteError,
    CommitCleanupCompleteStagedActiveReblitBootSync,
    CommitCleanupCompleteStagedActiveReblitBootSyncValidationError,
    CommittedStagedActiveReblitBootSync,
    CommittedStagedActiveReblitBootSyncValidationError,
    CommittedStagedActiveReblitCommitCleanupError,
    CompleteStagedActiveReblitBootSync,
    CompleteStagedActiveReblitBootSyncValidationError,
    CompletedStagedActiveReblitCommitDecisionError,
    CompletedStagedActiveReblitBootSync,
    DurableActiveReblitBootSyncCompletionRecord,
    FreshCompletedStagedActiveReblitBootSync,
    FinalizedStagedActiveReblitBootSync,
    FinalizedStagedActiveReblitBootSyncValidationError,
};
#[cfg(test)]
pub(in crate::client) use boot_sync_complete_persistence::arm_before_completion_journal_reopen;

#[cfg(test)]
#[path = "active_reblit_boot_sync_staging_tests.rs"]
mod tests;
