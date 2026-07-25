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
    State, SystemModel,
    state::{self, Selection},
    transition_identity::{
        NewStatePrevious, PreparedActiveReblitBootStateRoots, PreviousArchivedCoordinator, execute_new_state_forward,
    },
};

use super::{
    Client, JournalUsrExchangeAuthorityPreflight,
    active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
    active_reblit_boot_inputs::{ActiveReblitStoneBootInputsOutcome, PreparedActiveReblitStoneBootInputs},
    active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
    active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
    active_reblit_mounted_boot_topology::PreparedActiveReblitMountedBootTopology,
    active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
    candidate_metadata, fixed_staging,
    postblit::{self, TriggerScope},
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

#[derive(Debug, ThisError)]
#[error("the new state carries no bootable payload for the coordinated boot route")]
struct NewStateBootNotApplicable;

impl Client {
    /// Coordinated durable NewState apply for the archive-previous case: create
    /// a fresh state over the active one, archiving the predecessor as a boot
    /// rollback anchor. Composes the durable prefix (Slice 1), predecessor
    /// archive (Slice 2), and boot publication (Slice 3). Not yet the default —
    /// `new_state` keeps the legacy path until the crash matrix passes.
    #[allow(dead_code)]
    pub(in crate::client) fn apply_new_state_candidate(
        &self,
        candidate: fixed_staging::StatefulCandidate,
        previous: Option<state::Id>,
        selections: &[Selection],
        summary: &str,
        system_snapshot: SystemModel,
    ) -> Result<State, LiveNewStateBootError> {
        let fixed_staging::StatefulCandidate {
            tree,
            staging: _staging,
            candidate_usr,
            local_etc,
            active_state,
        } = candidate;

        // Boot applicability must be known before the journal record is written,
        // because `run_boot_sync` is fixed at creation and validation later
        // requires receipts exactly when it is set. NewState's row does not exist
        // yet, so the candidate's selections stand in as the prospective head
        // (`plans/future_impl.md` §1.1a).
        let applicability_deadline = deadline_after(BOOT_PUBLICATION_TIMEOUT, "boot applicability deadline")?;
        let run_boot_sync = self.new_state_boot_applicable(selections, previous, applicability_deadline)?;
        // A predecessor is archived as the rollback anchor; a first install has
        // none, so the archive phases never occur.
        let journal_previous = match previous {
            Some(previous) => NewStatePrevious::Active(previous),
            None => NewStatePrevious::SynthesizedEmpty,
        };

        let preflight = JournalUsrExchangeAuthorityPreflight::inspect(&self.installation, active_state, None)
            .map_err(|source| LiveNewStateBootError::at("pre-journal client authority", source))?;
        let candidate_path = self.installation.staging_path("usr");
        let (identity, authority) = preflight
            .prepare_unallocated_candidate(&self.state_db, &candidate_path)
            .map_err(|source| LiveNewStateBootError::at("unallocated candidate identity", source))?;

        let (coordinator, allocated) = execute_new_state_forward(
            identity,
            authority,
            &self.state_db,
            journal_previous,
            selections,
            summary,
            run_boot_sync,
            |os_info| candidate_metadata::derive_outputs(os_info, &system_snapshot),
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
        .map_err(|source| LiveNewStateBootError::at("journal-coordinated forward prefix", source))?;

        let archived = coordinator
            .archive_previous_tree()
            .map_err(|source| LiveNewStateBootError::at("predecessor archive", source))?;

        // The candidate state exists only after the forward prefix allocated its
        // row, so boot applicability is captured here rather than pre-journal.
        let boot_candidate = self
            .state_db
            .get(allocated)
            .map_err(|source| LiveNewStateBootError::at("candidate state load", source))?;

        if !run_boot_sync {
            // The pre-journal probe already established that this candidate
            // publishes no bootable plan, and the record says so. Commit
            // straight from `PreviousArchived` rather than entering boot.
            let _handoff = archived
                .commit_new_state_without_boot()
                .map_err(|source| LiveNewStateBootError::at("no-boot commit decision", source))?;
            return Ok(boot_candidate);
        }

        let input_deadline = deadline_after(BOOT_PUBLICATION_TIMEOUT, "boot input deadline")?;
        let stone = match PreparedActiveReblitStoneBootInputs::prepare_until(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            &boot_candidate,
            input_deadline,
        )
        .map_err(|source| LiveNewStateBootError::at("boot applicability", source))?
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(_) => {
                // The pre-journal probe said bootable, so the post-allocation
                // plan must agree; disagreement means the namespace or database
                // moved under us and the journal now asserts a boot that cannot
                // happen.
                return Err(LiveNewStateBootError::at(
                    "boot applicability",
                    NewStateBootNotApplicable,
                ));
            }
        };

        self.complete_new_state_boot(archived, &candidate_usr, &boot_candidate, stone)?;
        Ok(boot_candidate)
    }
}

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
        let prepared =
            PreparedActiveReblitBootRenderInputs::prepare_until(&stone, &roots, &self.installation, deadline)
                .map_err(|source| LiveNewStateBootError::at("boot render inputs", source))?;
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(&self.installation, deadline)
            .map_err(|source| LiveNewStateBootError::at("local boot policy", source))?;
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(&self.installation, deadline)
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
        let topology = PreparedActiveReblitMountedBootTopology::prepare_until(&self.installation, deadline)
            .map_err(|source| LiveNewStateBootError::at("mounted boot topology", source))?;
        let topology = topology
            .revalidate_until(&self.installation, deadline)
            .map_err(|source| LiveNewStateBootError::at("mounted boot topology revalidation", source))?;
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs)
            .map_err(|source| LiveNewStateBootError::at("BLS rendering", source))?;
        let plan = rendered
            .into_publication_plan(&topology)
            .map_err(|source| LiveNewStateBootError::at("boot publication planning", source))?;
        let inventory = plan
            .prepare_desired_publication_inventory()
            .map_err(|source| LiveNewStateBootError::at("desired boot publication inventory", source))?;
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
            Err(promoted) => promoted
                .cleanup_promoted_outputs(self)
                .map_err(|source| LiveNewStateBootError::at("promoted boot output cleanup", source))?,
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

/// Bounds for the pre-allocation layout query. Generous: this reads only the
/// candidate's own packages, not a whole retained chain.
const PROSPECTIVE_LAYOUT_BOUNDS: crate::db::layout::QueryBounds = crate::db::layout::QueryBounds {
    max_rows: 262_144,
    max_string_bytes: 64 * 1024 * 1024,
};

impl Client {
    /// Decide `run_boot_sync` for a NewState transition **before** its state row
    /// exists, so the journal record is correct at creation.
    ///
    /// The candidate's selections stand in as the prospective head; the retained
    /// tail comes from the current active state's projection, truncated exactly
    /// as allocating a head would truncate it. Reuses the real plan's rules, so
    /// this answer cannot drift from the post-allocation plan
    /// (`plans/future_impl.md` §1.1a).
    #[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
    pub(in crate::client) fn new_state_boot_applicable(
        &self,
        selections: &[Selection],
        previous: Option<state::Id>,
        deadline: Instant,
    ) -> Result<bool, LiveNewStateBootError> {
        let candidate_packages = selections
            .iter()
            .map(|selection| selection.package.clone())
            .collect::<Vec<_>>();
        let candidate_layouts = match self
            .layout_db
            .query_bounded(&candidate_packages, PROSPECTIVE_LAYOUT_BOUNDS, || Instant::now() <= deadline)
            .map_err(|source| LiveNewStateBootError::at("prospective candidate layouts", source))?
        {
            crate::db::layout::BoundedQueryOutcome::Complete(layouts) => layouts,
            bounded => {
                return Err(LiveNewStateBootError::at(
                    "prospective candidate layouts",
                    ProspectiveLayoutsBounded(format!("{bounded:?}")),
                ));
            }
        };
        let selected = candidate_packages
            .iter()
            .map(|package| package.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        // A first install has no retained chain, so the candidate alone decides
        // applicability. Replacing an active state inherits the tail the new head
        // would keep.
        let projection = match previous {
            Some(previous) => Some(
                crate::client::active_reblit_boot_projection::PreparedActiveReblitBootProjection::prepare_until(
                    &self.state_db,
                    &self.layout_db,
                    previous,
                    deadline,
                )
                .map_err(|source| LiveNewStateBootError::at("prospective chain projection", source))?,
            ),
            None => None,
        };
        let tail_states = projection.as_ref().map_or(&[][..], |projection| {
            crate::client::active_reblit_boot_projection::prospective_chain_tail(projection.states())
        });
        let tail_selected = tail_states
            .iter()
            .map(|state| {
                state
                    .selections
                    .iter()
                    .map(|selection| selection.package.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .collect::<Vec<_>>();
        let chain = tail_states
            .iter()
            .zip(tail_selected.iter())
            .map(|(state, selected)| {
                (
                    state.id,
                    selected,
                    projection.as_ref().expect("a tail exists only with a projection").layouts(),
                )
            })
            .collect::<Vec<_>>();

        match crate::client::active_reblit_boot_projection::assess_prospective_boot_applicability(
            &candidate_layouts,
            &selected,
            &chain,
            // Diagnostic label only: the candidate's row does not exist yet, so
            // errors are attributed to the predecessor when there is one.
            previous.unwrap_or_default(),
            deadline,
        )
        .map_err(|source| LiveNewStateBootError::at("prospective boot applicability", source))?
        {
            crate::client::active_reblit_boot_projection::ProspectiveBootApplicability::Applicable => Ok(true),
            crate::client::active_reblit_boot_projection::ProspectiveBootApplicability::NotApplicable(_) => Ok(false),
        }
    }
}

#[derive(Debug, ThisError)]
#[error("the prospective candidate layout query exceeded its bounds: {0}")]
struct ProspectiveLayoutsBounded(String);
