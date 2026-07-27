//! Narrow live-facing composition of the durable NewState forward prefix.
//!
//! Mirrors `active_reblit_forward` but drives `Operation::NewState`: it inserts
//! the fresh-allocation phases (`begin_fresh_allocation` → correlated DB row →
//! `finish_fresh_allocation`) before candidate preparation and takes the
//! `NewStateIsolation` branch (which never reserves an existing tree). Every
//! intermediate coordinator typestate stays private; callers receive only
//! callback-scoped borrows for the two external trigger effects and the exact
//! `SystemTriggersComplete` authority at the end of the prefix.

use std::{error::Error as StdError, fs::File, path::Path};

use thiserror::Error;

use crate::{
    Installation,
    client::{JournalUsrExchangeAuthority, RetainedRootAbi},
    db,
    state::{self, Selection, TransitionId},
};

use super::super::{CandidateMetadataError, CandidateMetadataOutputs, StatefulTreeIdentity};
use super::{
    NewStatePrevious, PreparedStatefulTransitionCoordinator, StatefulTransitionRequest,
    SystemTriggersCompleteCoordinator, system_triggers::StatefulSystemTriggerAuthority,
    transaction_triggers::StatefulTransactionTriggerAuthority,
};

type BoxedForwardError = Box<dyn StdError + Send + Sync + 'static>;

/// The only failure surface exposed by the composed NewState prefix.
///
/// Private phase failures remain boxed so their authority-bearing types do not
/// become part of the crate-visible facade.
#[derive(Debug, Error)]
#[error("NewState forward transition failed during {stage}")]
pub(crate) struct NewStateForwardError {
    stage: &'static str,
    #[source]
    source: BoxedForwardError,
}

impl NewStateForwardError {
    fn at(stage: &'static str, source: impl StdError + Send + Sync + 'static) -> Self {
        Self {
            stage,
            source: Box::new(source),
        }
    }

    pub(crate) const fn stage(&self) -> &'static str {
        self.stage
    }
}

#[derive(Debug, Error)]
#[error("NewState candidate preparation returned a different operation typestate")]
struct UnexpectedPreparedOperation;

/// Callback-scoped, borrow-only transaction-trigger inputs.
#[derive(Debug)]
pub(crate) struct NewStateTransactionTriggerView<'authority> {
    transition_id: &'authority TransitionId,
    state: state::Id,
    candidate_usr: &'authority File,
    candidate_usr_path: &'authority Path,
    installation: &'authority Installation,
    isolation_root: &'authority RetainedRootAbi,
}

impl<'authority> NewStateTransactionTriggerView<'authority> {
    fn from_authority(authority: StatefulTransactionTriggerAuthority<'authority>) -> Self {
        let transition_id = authority.transition_id();
        let state = authority.candidate_state();
        let (candidate_usr, candidate_usr_path) = authority.retained_candidate_usr();
        let (installation, isolation_root) = authority.retained_isolation_root();
        Self {
            transition_id,
            state,
            candidate_usr,
            candidate_usr_path,
            installation,
            isolation_root,
        }
    }

    pub(crate) const fn transition_id(&self) -> &'authority TransitionId {
        self.transition_id
    }

    pub(crate) const fn state(&self) -> state::Id {
        self.state
    }

    pub(crate) const fn retained_candidate_usr(&self) -> (&'authority File, &'authority Path) {
        (self.candidate_usr, self.candidate_usr_path)
    }

    pub(crate) const fn retained_isolation_root(&self) -> (&'authority Installation, &'authority RetainedRootAbi) {
        (self.installation, self.isolation_root)
    }
}

/// Callback-scoped, borrow-only system-trigger inputs.
#[derive(Debug)]
pub(crate) struct NewStateSystemTriggerView<'authority> {
    transition_id: &'authority TransitionId,
    state: state::Id,
    installation: &'authority Installation,
    candidate_usr: &'authority File,
    isolation_root: &'authority RetainedRootAbi,
}

impl<'authority> NewStateSystemTriggerView<'authority> {
    fn from_authority(authority: StatefulSystemTriggerAuthority<'authority>) -> Self {
        let transition_id = authority.transition_id();
        let state = authority.candidate_state();
        let (installation, candidate_usr, isolation_root) = authority.retained_view();
        Self {
            transition_id,
            state,
            installation,
            candidate_usr,
            isolation_root,
        }
    }

    pub(crate) const fn transition_id(&self) -> &'authority TransitionId {
        self.transition_id
    }

    pub(crate) const fn state(&self) -> state::Id {
        self.state
    }

    pub(crate) const fn retained_view(
        &self,
    ) -> (&'authority Installation, &'authority File, &'authority RetainedRootAbi) {
        (self.installation, self.candidate_usr, self.isolation_root)
    }
}

/// Consume the complete NewState forward prefix through durable
/// `SystemTriggersComplete`, returning the coordinator plus the freshly
/// allocated state id.
///
/// The fresh state row is inserted mid-prefix, stamped with the journal's
/// transition id, so a crash before `finish_fresh_allocation` leaves an
/// unowned row that startup reconciliation can invalidate. System triggers are
/// mandatory. Boot applicability remains a caller decision made before the
/// first journal record is created.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_new_state_forward<TxError, SystemError, DeriveMetadata, TransactionTrigger, SystemTrigger>(
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    database: &db::state::Database,
    previous: NewStatePrevious,
    selections: &[Selection],
    summary: &str,
    run_boot_sync: bool,
    derive_metadata: DeriveMetadata,
    transaction_trigger: TransactionTrigger,
    system_trigger: SystemTrigger,
) -> Result<(SystemTriggersCompleteCoordinator, state::Id), NewStateForwardError>
where
    TxError: StdError + Send + Sync + 'static,
    SystemError: StdError + Send + Sync + 'static,
    DeriveMetadata: FnOnce(Option<&[u8]>) -> Result<CandidateMetadataOutputs, CandidateMetadataError>,
    TransactionTrigger: for<'authority> FnOnce(NewStateTransactionTriggerView<'authority>) -> Result<(), TxError>,
    SystemTrigger: for<'authority> FnOnce(NewStateSystemTriggerView<'authority>) -> Result<(), SystemError>,
{
    let coordinator = identity
        .begin_transition(StatefulTransitionRequest::NewState {
            previous,
            run_system_triggers: true,
            run_boot_sync,
        })
        .map_err(|source| NewStateForwardError::at("transition creation", source))?;
    let coordinator = coordinator
        .begin_fresh_allocation()
        .map_err(|source| NewStateForwardError::at("fresh allocation intent", source))?;
    let allocated = {
        let transition = coordinator
            .transition_id_for_allocation()
            .map_err(|source| NewStateForwardError::at("fresh allocation correlation", source))?;
        database
            .add_with_transition(transition, selections, Some(summary), None)
            .map_err(|source| NewStateForwardError::at("fresh state row allocation", source))?
            .id
    };
    let coordinator = coordinator
        .finish_fresh_allocation(database, allocated)
        .map_err(|source| NewStateForwardError::at("fresh allocation completion", source))?;
    let coordinator = coordinator
        .begin_candidate_prepare()
        .map_err(|source| NewStateForwardError::at("candidate preparation intent", source))?;
    let prepared = coordinator
        .finish_candidate_prepare(derive_metadata)
        .map_err(|source| NewStateForwardError::at("candidate metadata publication", source))?;
    let PreparedStatefulTransitionCoordinator::NewStateIsolation(prepared) = prepared else {
        return Err(NewStateForwardError::at(
            "candidate metadata publication",
            UnexpectedPreparedOperation,
        ));
    };

    let prepared = prepared
        .prepare_for_transaction_triggers(authority.installation())
        .map_err(|source| NewStateForwardError::at("transaction isolation publication", source))?;
    let complete = prepared
        .run_transaction_triggers(|inner| transaction_trigger(NewStateTransactionTriggerView::from_authority(inner)))
        .map_err(|source| NewStateForwardError::at("transaction triggers", source))?;
    let intent = complete
        .begin_usr_exchange_intent()
        .map_err(|source| NewStateForwardError::at("/usr exchange intent", source))?;
    let exchanged = intent
        .execute_usr_exchange(authority)
        .map_err(|source| NewStateForwardError::at("/usr exchange", source))?;
    let root_links = exchanged
        .publish_root_abi()
        .map_err(|source| NewStateForwardError::at("root ABI publication", source))?;
    let complete = root_links
        .run_system_triggers(|inner| system_trigger(NewStateSystemTriggerView::from_authority(inner)))
        .map_err(|source| NewStateForwardError::at("system triggers", source))?;
    Ok((complete, allocated))
}

/// Drive the durable forward prefix for activating an already-archived state.
///
/// Shorter than the NewState prefix by design: the state row already exists —
/// that is what "archived" means — so there is no fresh allocation to correlate
/// and no `FreshStateAllocating`/`FreshStateAllocated` pair. Everything from
/// candidate preparation onward is the shared path
/// (`plans/future_impl.md` §1.2a).
// Forward scaffolding: consumed once `state_planning.rs` routes activation off
// the legacy `commit_stateful_staging`.
#[allow(dead_code)] // consumed by the coordinated ActivateArchived route (§1.2)
pub(crate) fn execute_activate_archived_forward<SystemError, DeriveMetadata, SystemTrigger>(
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    candidate: state::Id,
    previous: state::Id,
    run_boot_sync: bool,
    derive_metadata: DeriveMetadata,
    system_trigger: SystemTrigger,
) -> Result<SystemTriggersCompleteCoordinator, NewStateForwardError>
where
    SystemError: StdError + Send + Sync + 'static,
    DeriveMetadata: FnOnce(Option<&[u8]>) -> Result<CandidateMetadataOutputs, CandidateMetadataError>,
    SystemTrigger: for<'authority> FnOnce(NewStateSystemTriggerView<'authority>) -> Result<(), SystemError>,
{
    if candidate == previous {
        return Err(NewStateForwardError::at(
            "transition creation",
            UnexpectedPreparedOperation,
        ));
    }
    let coordinator = identity
        .begin_transition(StatefulTransitionRequest::ActivateArchived {
            candidate,
            previous,
            run_system_triggers: true,
            run_boot_sync,
        })
        .map_err(|source| NewStateForwardError::at("transition creation", source))?;
    let coordinator = coordinator
        .begin_archived_staging()
        .map_err(|source| NewStateForwardError::at("archived staging intent", source))?;
    let coordinator = coordinator
        .complete_archived_staging()
        .map_err(|source| NewStateForwardError::at("archived staging completion", source))?;
    let coordinator = coordinator
        .begin_candidate_prepare()
        .map_err(|source| NewStateForwardError::at("candidate preparation intent", source))?;
    let prepared = coordinator
        .finish_candidate_prepare(derive_metadata)
        .map_err(|source| NewStateForwardError::at("candidate metadata publication", source))?;
    let PreparedStatefulTransitionCoordinator::Archived(prepared) = prepared else {
        return Err(NewStateForwardError::at(
            "candidate metadata publication",
            UnexpectedPreparedOperation,
        ));
    };

    // Archived activation never runs transaction triggers: the candidate tree
    // already exists and was built by the transition that first created it, so
    // there is no isolation root to publish and nothing to run against it.
    let intent = prepared
        .begin_usr_exchange_intent()
        .map_err(|source| NewStateForwardError::at("/usr exchange intent", source))?;
    let exchanged = intent
        .execute_usr_exchange(authority)
        .map_err(|source| NewStateForwardError::at("/usr exchange", source))?;
    let root_links = exchanged
        .publish_root_abi()
        .map_err(|source| NewStateForwardError::at("root ABI publication", source))?;
    root_links
        .run_system_triggers(|inner| system_trigger(NewStateSystemTriggerView::from_authority(inner)))
        .map_err(|source| NewStateForwardError::at("system triggers", source))
}
