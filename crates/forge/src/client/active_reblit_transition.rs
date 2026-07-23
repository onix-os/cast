//! Production ActiveReblit execution through the durable journal coordinator.
//!
//! This route is deliberately separate from fresh-state and archived-state
//! activation. Boot applicability is fixed before journal creation; every
//! later effect consumes the existing coordinator and boot typestates.

use std::{error::Error as StdError, time::{Duration, Instant}};

use thiserror::Error as ThisError;

use crate::{
    State, SystemModel,
    transition_identity::{
        PreparedActiveReblitBootStateRoots, SystemTriggersCompleteCoordinator,
        execute_active_reblit_forward,
    },
};

use super::{
    Client, Error,
    active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
    active_reblit_boot_inputs::{
        ActiveReblitStoneBootInputsOutcome, PreparedActiveReblitStoneBootInputs,
    },
    active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
    active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
    active_reblit_mounted_boot_topology::PreparedActiveReblitMountedBootTopology,
    active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
    fixed_staging,
    postblit::{self, TriggerScope},
    JournalUsrExchangeAuthorityPreflight,
};

const BOOT_INPUT_TIMEOUT: Duration = Duration::from_secs(120);
const BOOT_PUBLICATION_TIMEOUT: Duration = Duration::from_secs(120);

type BoxedLiveActiveReblitError = Box<dyn StdError + Send + Sync + 'static>;

#[derive(Debug, ThisError)]
#[error("live ActiveReblit failed during {stage}")]
struct LiveActiveReblitError {
    stage: &'static str,
    #[source]
    source: BoxedLiveActiveReblitError,
}

impl LiveActiveReblitError {
    fn at(stage: &'static str, source: impl StdError + Send + Sync + 'static) -> Self {
        Self {
            stage,
            source: Box::new(source),
        }
    }
}

#[derive(Debug, ThisError)]
#[error("the monotonic ActiveReblit operation deadline overflowed")]
struct DeadlineOverflow;

impl Client {
    /// Repair the selected active state without entering the legacy stateful
    /// transition path used by fresh-state creation.
    pub(super) fn apply_active_reblit_candidate(
        &self,
        candidate: fixed_staging::StatefulCandidate,
        state: &State,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        self.apply_active_reblit_candidate_inner(candidate, state, system_snapshot)
            .map_err(|source| Error::LiveActiveReblit {
                source: Box::new(source),
            })
    }

    fn apply_active_reblit_candidate_inner(
        &self,
        candidate: fixed_staging::StatefulCandidate,
        state: &State,
        system_snapshot: SystemModel,
    ) -> Result<(), LiveActiveReblitError> {
        let fixed_staging::StatefulCandidate {
            tree,
            staging: _staging,
            candidate_usr,
            local_etc,
            active_state,
        } = candidate;

        let input_deadline = deadline_after(BOOT_INPUT_TIMEOUT, "boot input deadline")?;
        let boot_inputs = PreparedActiveReblitStoneBootInputs::prepare_until(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            state,
            input_deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("pre-journal boot applicability", source))?;
        let (run_boot_sync, stone) = match boot_inputs {
            ActiveReblitStoneBootInputsOutcome::NotApplicable(_) => (false, None),
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => (true, Some(stone)),
        };

        let authority = JournalUsrExchangeAuthorityPreflight::inspect(
            &self.installation,
            active_state,
            Some(state.clone()),
        )
        .map_err(|source| LiveActiveReblitError::at("pre-journal client authority", source))?;
        let candidate_path = self.installation.staging_path("usr");
        let (identity, authority) = authority
            .prepare_retained_active_reblit_identity(
                &self.state_db,
                &candidate_usr,
                &candidate_path,
                state.id,
            )
            .map_err(|source| LiveActiveReblitError::at("retained tree identity", source))?;

        let coordinator = execute_active_reblit_forward(
            identity,
            authority,
            state.id,
            run_boot_sync,
            |os_info| super::candidate_metadata::derive_outputs(os_info, &system_snapshot),
            |view| {
                let (candidate_usr, candidate_usr_path) = view.retained_candidate_usr();
                let (installation, isolation_root) = view.retained_isolation_root();
                Client::apply_triggers(
                    TriggerScope::RetainedTransaction {
                        kind: postblit::RetainedTransactionKind::Stateful,
                        installation,
                        isolation_root,
                        local_etc: &local_etc,
                        candidate_usr,
                        candidate_usr_path,
                    },
                    &tree,
                )
            },
            |view| {
                let (installation, retained_usr, isolation_root) = view.retained_view();
                let live_usr_path = installation.root.join("usr");
                Client::apply_triggers(
                    TriggerScope::System {
                        installation,
                        isolation_root,
                        local_etc: &local_etc,
                        retained_usr,
                        live_usr_path: &live_usr_path,
                    },
                    &tree,
                )
            },
        )
        .map_err(|source| LiveActiveReblitError::at("journal-coordinated forward prefix", source))?;

        match stone {
            Some(stone) => self.complete_active_reblit_boot(
                coordinator,
                &candidate_usr,
                state,
                stone,
            ),
            None => {
                let _finalized = coordinator
                    .complete_active_reblit_without_boot()
                    .map_err(|source| LiveActiveReblitError::at("no-boot terminal completion", source))?;
                Ok(())
            }
        }
    }

    fn complete_active_reblit_boot(
        &self,
        coordinator: SystemTriggersCompleteCoordinator,
        selected_head_usr: &std::fs::File,
        state: &State,
        stone: PreparedActiveReblitStoneBootInputs,
    ) -> Result<(), LiveActiveReblitError> {
        let deadline = deadline_after(BOOT_PUBLICATION_TIMEOUT, "boot publication deadline")?;
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &self.installation,
            selected_head_usr,
            state.id,
            stone.state_ids(),
            deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("live boot state roots", source))?;
        let prepared = PreparedActiveReblitBootRenderInputs::prepare_until(
            &stone,
            &roots,
            &self.installation,
            deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("boot render inputs", source))?;
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
            &self.installation,
            deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("local boot policy", source))?;
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
            &self.installation,
            deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("root filesystem intent", source))?;
        let inputs = prepared
            .revalidate_until(
                &self.state_db,
                &self.layout_db,
                &self.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .map_err(|source| LiveActiveReblitError::at("boot input revalidation", source))?;
        let topology = PreparedActiveReblitMountedBootTopology::prepare_until(
            &self.installation,
            deadline,
        )
        .map_err(|source| LiveActiveReblitError::at("mounted boot topology", source))?;
        let topology = topology
            .revalidate_until(&self.installation, deadline)
            .map_err(|source| LiveActiveReblitError::at("mounted boot topology revalidation", source))?;
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs)
            .map_err(|source| LiveActiveReblitError::at("BLS rendering", source))?;
        let plan = rendered
            .into_publication_plan(&topology)
            .map_err(|source| LiveActiveReblitError::at("boot publication planning", source))?;
        let inventory = plan
            .prepare_desired_publication_inventory()
            .map_err(|source| LiveActiveReblitError::at("desired boot publication inventory", source))?;
        let staged = self
            .stage_active_reblit_boot_sync_from_coordinator(&plan, &inventory, coordinator)
            .map_err(|source| LiveActiveReblitError::at("BootSyncStarted staging", source))?;
        let terminal = staged
            .attempt_immutable_boot_publication(self)
            .map_err(|source| LiveActiveReblitError::at("immutable boot publication", source))?;
        let promoted = terminal
            .promote_terminal_receipt(self)
            .map_err(|source| LiveActiveReblitError::at("boot receipt promotion", source))?;
        let cleaned = match promoted.try_into_cleaned() {
            Ok(cleaned) => cleaned,
            Err(promoted) => promoted
                .cleanup_promoted_outputs(self)
                .map_err(|source| LiveActiveReblitError::at("promoted boot output cleanup", source))?,
        };
        let completed = cleaned
            .persist_boot_sync_complete(self)
            .map_err(|source| LiveActiveReblitError::at("BootSyncComplete persistence", source))?;
        let committed = completed
            .persist_commit_decided(self)
            .map_err(|source| LiveActiveReblitError::at("CommitDecided persistence", source))?;
        let cleaned = committed
            .persist_commit_cleanup_complete(self)
            .map_err(|source| LiveActiveReblitError::at("commit cleanup", source))?;
        let complete = cleaned
            .persist_complete(self)
            .map_err(|source| LiveActiveReblitError::at("Complete persistence", source))?;
        let _finalized = complete
            .finalize(self)
            .map_err(|source| LiveActiveReblitError::at("terminal finalization", source))?;
        Ok(())
    }
}

fn deadline_after(
    duration: Duration,
    stage: &'static str,
) -> Result<Instant, LiveActiveReblitError> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| LiveActiveReblitError::at(stage, DeadlineOverflow))
}
