//! Production NewState boot publication through the durable journal coordinator.
//!
//! The predecessor has already been archived (`PreviousArchived`), so this
//! drives only the operation-neutral boot-publication suffix for the freshly
//! created candidate: the same bounded input capture, BLS rendering, staging,
//! immutable publication, and terminal finalization the ActiveReblit route
//! uses, differing only in the handoff source (a distinct candidate whose real
//! predecessor is a separate archived rollback anchor).

use std::{
    error::Error as StdError,
    time::{Duration, Instant},
};

use thiserror::Error as ThisError;

use crate::{
    State,
    transition_identity::{PreparedActiveReblitBootStateRoots, PreviousArchivedCoordinator},
};

use super::{
    Client,
    active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
    active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
    active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
    active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
    active_reblit_mounted_boot_topology::PreparedActiveReblitMountedBootTopology,
    active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
};

const BOOT_PUBLICATION_TIMEOUT: Duration = Duration::from_secs(120);

type BoxedLiveNewStateError = Box<dyn StdError + Send + Sync + 'static>;

#[derive(Debug, ThisError)]
#[error("live NewState boot failed during {stage}")]
pub(in crate::client) struct LiveNewStateBootError {
    stage: &'static str,
    #[source]
    source: BoxedLiveNewStateError,
}

impl LiveNewStateBootError {
    fn at(stage: &'static str, source: impl StdError + Send + Sync + 'static) -> Self {
        Self {
            stage,
            source: Box::new(source),
        }
    }
}

#[derive(Debug, ThisError)]
#[error("the monotonic NewState boot deadline overflowed")]
struct DeadlineOverflow;

impl Client {
    /// Publish boot entries for the freshly created NewState candidate and drive
    /// the durable journal to `Complete`. Not yet wired: `new_state` routes
    /// through this once the coordinated forward path replaces the legacy
    /// commit (Phase 1 Slice 5).
    #[allow(dead_code)]
    pub(in crate::client) fn complete_new_state_boot(
        &self,
        coordinator: PreviousArchivedCoordinator,
        boot_candidate_usr: &std::fs::File,
        boot_candidate: &State,
        stone: PreparedActiveReblitStoneBootInputs,
    ) -> Result<(), LiveNewStateBootError> {
        let deadline = deadline_after(BOOT_PUBLICATION_TIMEOUT, "boot publication deadline")?;
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &self.installation,
            boot_candidate_usr,
            boot_candidate.id,
            stone.state_ids(),
            deadline,
        )
        .map_err(|source| LiveNewStateBootError::at("live boot state roots", source))?;
        let prepared = PreparedActiveReblitBootRenderInputs::prepare_until(
            &stone,
            &roots,
            &self.installation,
            deadline,
        )
        .map_err(|source| LiveNewStateBootError::at("boot render inputs", source))?;
        let local_policy =
            PreparedActiveReblitLocalBootPolicy::prepare_until(&self.installation, deadline)
                .map_err(|source| LiveNewStateBootError::at("local boot policy", source))?;
        let root_intent =
            PreparedActiveReblitRootFilesystemIntent::prepare_until(&self.installation, deadline)
                .map_err(|source| LiveNewStateBootError::at("root filesystem intent", source))?;
        let inputs = prepared
            .revalidate_until(
                &self.state_db,
                &self.layout_db,
                &self.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .map_err(|source| LiveNewStateBootError::at("boot input revalidation", source))?;
        let topology =
            PreparedActiveReblitMountedBootTopology::prepare_until(&self.installation, deadline)
                .map_err(|source| LiveNewStateBootError::at("mounted boot topology", source))?;
        let topology = topology
            .revalidate_until(&self.installation, deadline)
            .map_err(|source| {
                LiveNewStateBootError::at("mounted boot topology revalidation", source)
            })?;
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs)
            .map_err(|source| LiveNewStateBootError::at("BLS rendering", source))?;
        let plan = rendered
            .into_publication_plan(&topology)
            .map_err(|source| LiveNewStateBootError::at("boot publication planning", source))?;
        let inventory = plan.prepare_desired_publication_inventory().map_err(|source| {
            LiveNewStateBootError::at("desired boot publication inventory", source)
        })?;
        let handoff = coordinator
            .into_new_state_boot_sync_handoff()
            .map_err(|source| LiveNewStateBootError::at("new state boot handoff", source))?;
        let staged = self
            .stage_new_state_boot_sync_from_handoff(&plan, &inventory, handoff)
            .map_err(|source| LiveNewStateBootError::at("BootSyncStarted staging", source))?;
        let terminal = staged
            .attempt_immutable_boot_publication(self)
            .map_err(|source| LiveNewStateBootError::at("immutable boot publication", source))?;
        let promoted = terminal
            .promote_terminal_receipt(self)
            .map_err(|source| LiveNewStateBootError::at("boot receipt promotion", source))?;
        let cleaned = match promoted.try_into_cleaned() {
            Ok(cleaned) => cleaned,
            Err(promoted) => promoted.cleanup_promoted_outputs(self).map_err(|source| {
                LiveNewStateBootError::at("promoted boot output cleanup", source)
            })?,
        };
        let completed = cleaned
            .persist_boot_sync_complete(self)
            .map_err(|source| LiveNewStateBootError::at("BootSyncComplete persistence", source))?;
        let committed = completed
            .persist_commit_decided(self)
            .map_err(|source| LiveNewStateBootError::at("CommitDecided persistence", source))?;
        let cleaned = committed
            .persist_commit_cleanup_complete(self)
            .map_err(|source| LiveNewStateBootError::at("commit cleanup", source))?;
        let complete = cleaned
            .persist_complete(self)
            .map_err(|source| LiveNewStateBootError::at("Complete persistence", source))?;
        let _finalized = complete
            .finalize(self)
            .map_err(|source| LiveNewStateBootError::at("terminal finalization", source))?;
        Ok(())
    }
}

fn deadline_after(duration: Duration, stage: &'static str) -> Result<Instant, LiveNewStateBootError> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| LiveNewStateBootError::at(stage, DeadlineOverflow))
}
