//! Narrow live-facing composition of the durable ActiveReblit forward prefix.
//!
//! Every intermediate coordinator typestate stays private. Callers receive
//! only callback-scoped borrows for the two external trigger effects and the
//! exact `SystemTriggersComplete` authority at the end of the prefix.

use std::{error::Error as StdError, fs::File, path::Path};

use thiserror::Error;

use crate::{
    Installation,
    client::{JournalUsrExchangeAuthority, RetainedRootAbi},
    state::{self, TransitionId},
};

use super::{
    PreparedStatefulTransitionCoordinator, StatefulTransitionRequest,
    SystemTriggersCompleteCoordinator,
    system_triggers::StatefulSystemTriggerAuthority,
    transaction_isolation::require_pre_journal_active_reblit_installation,
    transaction_triggers::StatefulTransactionTriggerAuthority,
};
use super::super::{CandidateMetadataError, CandidateMetadataOutputs, StatefulTreeIdentity};

type BoxedForwardError = Box<dyn StdError + Send + Sync + 'static>;

/// The only failure surface exposed by the composed ActiveReblit prefix.
///
/// Private phase failures remain boxed so their authority-bearing types do not
/// become part of the crate-visible facade.
#[derive(Debug, Error)]
#[error("ActiveReblit forward transition failed during {stage}")]
pub(crate) struct ActiveReblitForwardError {
    stage: &'static str,
    #[source]
    source: BoxedForwardError,
}

impl ActiveReblitForwardError {
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
#[error("ActiveReblit candidate preparation returned a different operation typestate")]
struct UnexpectedPreparedOperation;

/// Callback-scoped, borrow-only transaction-trigger inputs.
#[derive(Debug)]
pub(crate) struct ActiveReblitTransactionTriggerView<'authority> {
    transition_id: &'authority TransitionId,
    state: state::Id,
    candidate_usr: &'authority File,
    candidate_usr_path: &'authority Path,
    installation: &'authority Installation,
    isolation_root: &'authority RetainedRootAbi,
}

impl<'authority> ActiveReblitTransactionTriggerView<'authority> {
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

    pub(crate) const fn retained_isolation_root(
        &self,
    ) -> (&'authority Installation, &'authority RetainedRootAbi) {
        (self.installation, self.isolation_root)
    }
}

/// Callback-scoped, borrow-only system-trigger inputs.
#[derive(Debug)]
pub(crate) struct ActiveReblitSystemTriggerView<'authority> {
    transition_id: &'authority TransitionId,
    state: state::Id,
    installation: &'authority Installation,
    candidate_usr: &'authority File,
    isolation_root: &'authority RetainedRootAbi,
}

impl<'authority> ActiveReblitSystemTriggerView<'authority> {
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
    ) -> (
        &'authority Installation,
        &'authority File,
        &'authority RetainedRootAbi,
    ) {
        (self.installation, self.candidate_usr, self.isolation_root)
    }
}

/// Consume the complete ActiveReblit forward prefix through durable
/// `SystemTriggersComplete`.
///
/// System triggers are mandatory. Boot applicability remains a caller decision
/// made before the first journal record is created.
pub(crate) fn execute_active_reblit_forward<
    TxError,
    SystemError,
    DeriveMetadata,
    TransactionTrigger,
    SystemTrigger,
>(
    identity: StatefulTreeIdentity,
    authority: JournalUsrExchangeAuthority,
    state: state::Id,
    run_boot_sync: bool,
    derive_metadata: DeriveMetadata,
    transaction_trigger: TransactionTrigger,
    system_trigger: SystemTrigger,
) -> Result<SystemTriggersCompleteCoordinator, ActiveReblitForwardError>
where
    TxError: StdError + Send + Sync + 'static,
    SystemError: StdError + Send + Sync + 'static,
    DeriveMetadata: FnOnce(Option<&[u8]>) -> Result<CandidateMetadataOutputs, CandidateMetadataError>,
    TransactionTrigger:
        for<'authority> FnOnce(ActiveReblitTransactionTriggerView<'authority>) -> Result<(), TxError>,
    SystemTrigger: for<'authority> FnOnce(ActiveReblitSystemTriggerView<'authority>) -> Result<(), SystemError>,
{
    require_pre_journal_active_reblit_installation(&identity, authority.installation(), state)
        .map_err(|source| ActiveReblitForwardError::at("paired client authority", source))?;
    let coordinator = identity
        .begin_transition(StatefulTransitionRequest::ActiveReblit {
            state,
            run_system_triggers: true,
            run_boot_sync,
        })
        .map_err(|source| ActiveReblitForwardError::at("transition creation", source))?;
    let coordinator = coordinator
        .begin_candidate_prepare()
        .map_err(|source| ActiveReblitForwardError::at("candidate preparation intent", source))?;
    let prepared = coordinator
        .finish_candidate_prepare(derive_metadata)
        .map_err(|source| ActiveReblitForwardError::at("candidate metadata publication", source))?;
    let PreparedStatefulTransitionCoordinator::ActiveReblitReservation(prepared) = prepared else {
        return Err(ActiveReblitForwardError::at(
            "candidate metadata publication",
            UnexpectedPreparedOperation,
        ));
    };

    let prepared = prepared
        .reserve_for_transaction_triggers(authority.installation())
        .map_err(|source| ActiveReblitForwardError::at("ActiveReblit reservation", source))?;
    let prepared = prepared
        .prepare_for_transaction_triggers(authority.installation())
        .map_err(|source| ActiveReblitForwardError::at("transaction isolation publication", source))?;
    let complete = prepared
        .run_transaction_triggers(|inner| {
            transaction_trigger(ActiveReblitTransactionTriggerView::from_authority(inner))
        })
        .map_err(|source| ActiveReblitForwardError::at("transaction triggers", source))?;
    let intent = complete
        .begin_usr_exchange_intent()
        .map_err(|source| ActiveReblitForwardError::at("/usr exchange intent", source))?;
    let exchanged = intent
        .execute_usr_exchange(authority)
        .map_err(|source| ActiveReblitForwardError::at("/usr exchange", source))?;
    let root_links = exchanged
        .publish_root_abi()
        .map_err(|source| ActiveReblitForwardError::at("root ABI publication", source))?;
    root_links
        .run_system_triggers(|inner| {
            system_trigger(ActiveReblitSystemTriggerView::from_authority(inner))
        })
        .map_err(|source| ActiveReblitForwardError::at("system triggers", source))
}
