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
        NewStatePrevious, PreparedActiveReblitBootStateRoots, PreviousArchivedCoordinator,
        SystemTriggersCompleteCoordinator, execute_activate_archived_forward, execute_new_state_forward,
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

const BOOT_PUBLICATION_TIMEOUT: Duration = crate::client::boot_timeout_policy::boot_budget(Duration::from_secs(120));

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

        // The candidate state exists only after the forward prefix allocated its
        // row, so it is loaded here rather than pre-journal.
        let boot_candidate = self
            .state_db
            .get(allocated)
            .map_err(|source| LiveNewStateBootError::at("candidate state load", source))?;

        // A first install has no predecessor, so the archive phases never occur
        // and both tails start one step earlier, at `SystemTriggersComplete`.
        let Some(_) = previous else {
            if !run_boot_sync {
                let handoff = coordinator
                    .commit_new_state_unarchived_without_boot()
                    .map_err(|source| LiveNewStateBootError::at("unarchived no-boot commit", source))?;
                self.finish_new_state_transition(handoff.journal, handoff.record, &handoff.active_state_reservation)?;
                return Ok(boot_candidate);
            }
            let stone = self.require_new_state_boot_inputs(&boot_candidate)?;
            self.complete_new_state_boot(
                NewStateBootSource::Unarchived(coordinator),
                &candidate_usr,
                &boot_candidate,
                stone,
            )?;
            return Ok(boot_candidate);
        };

        let archived = coordinator
            .archive_previous_tree()
            .map_err(|source| LiveNewStateBootError::at("predecessor archive", source))?;

        if !run_boot_sync {
            // The pre-journal probe already established that this candidate
            // publishes no bootable plan, and the record says so. Commit
            // straight from `PreviousArchived` rather than entering boot.
            let handoff = archived
                .commit_new_state_without_boot()
                .map_err(|source| LiveNewStateBootError::at("no-boot commit decision", source))?;
            self.finish_new_state_transition(handoff.journal, handoff.record, &handoff.active_state_reservation)?;
            return Ok(boot_candidate);
        }

        let stone = self.require_new_state_boot_inputs(&boot_candidate)?;
        self.complete_new_state_boot(
            NewStateBootSource::Archived(archived),
            &candidate_usr,
            &boot_candidate,
            stone,
        )?;
        Ok(boot_candidate)
    }

    /// Coordinated durable `ActivateArchived` apply: make an already-archived
    /// state live again, archiving the predecessor as the rollback anchor.
    ///
    /// The mirror of `apply_new_state_candidate`, and the piece §1.2 always
    /// listed as remaining. Without it the coordinated route existed but had no
    /// caller — `cast state activate` reached `commit_stateful_staging`, whose
    /// exchange asserts through `ExchangeJournalGuard::LegacyNoJournal` that no
    /// journal is present, so the archived-staging pair and every crash-matrix
    /// claim about this operation were unreachable
    /// (`plans/close_out.md`, correction 2026-07-29).
    ///
    /// Two things differ from NewState:
    ///
    /// - **No allocation.** The candidate row exists, so the identity is
    ///   prepared against the *archived* tree rather than an unallocated
    ///   staging one, and there is no allocated id to read back.
    /// - **The staging move belongs to the coordinator.** The legacy route moved
    ///   the archived tree into staging before committing, outside the journal.
    ///   That move now sits between `begin_archived_staging` and
    ///   `complete_archived_staging`, so a crash mid-move leaves a record
    ///   recovery can act on rather than an orphan.
    #[allow(dead_code)] // wired at the `state activate` call site next
    pub(in crate::client) fn apply_activate_archived_candidate(
        &self,
        candidate: &State,
        previous: &State,
        archived_usr: &std::path::Path,
        active_state: super::active_state_authority::ActiveStateAuthority,
        local_etc: &super::transaction_root::RetainedLocalEtc,
        tree: &vfs::Tree<super::PendingFile>,
        system_snapshot: SystemModel,
        run_system_triggers: bool,
        run_boot_sync: bool,
    ) -> Result<(), LiveNewStateBootError> {
        let preflight = JournalUsrExchangeAuthorityPreflight::inspect(&self.installation, active_state, None)
            .map_err(|source| LiveNewStateBootError::at("pre-journal client authority", source))?;

        // Prepared against the archived tree: the candidate has not moved into
        // staging yet, and moving it is the coordinator's job.
        let (identity, authority) = preflight
            .prepare_candidate(&self.state_db, archived_usr, candidate.id)
            .map_err(|source| LiveNewStateBootError::at("archived candidate identity", source))?;

        let coordinator = execute_activate_archived_forward(
            identity,
            authority,
            &self.installation,
            candidate.id,
            previous.id,
            run_system_triggers,
            run_boot_sync,
            |os_info| candidate_metadata::derive_outputs(os_info, &system_snapshot),
            // ActivateArchived runs system triggers only — its candidate was
            // already built when the state was created, so there is nothing for
            // transaction triggers to do.
            |view| {
                let (installation, retained_usr, isolation_root) = view.retained_view();
                let live_usr_path = installation.root.join("usr");
                Client::apply_triggers(
                    TriggerScope::System {
                        installation,
                        isolation_root,
                        local_etc,
                        retained_usr,
                        live_usr_path: &live_usr_path,
                    },
                    tree,
                )
            },
        )
        .map_err(|source| LiveNewStateBootError::at("journal-coordinated forward prefix", source))?;
        let archived = coordinator;

        if !run_boot_sync {
            let handoff = archived
                .commit_new_state_without_boot()
                .map_err(|source| LiveNewStateBootError::at("no-boot commit decision", source))?;
            self.finish_new_state_transition(handoff.journal, handoff.record, &handoff.active_state_reservation)?;
            return Ok(());
        }

        let stone = self.require_new_state_boot_inputs(candidate)?;
        // The retained descriptor, not a reopened pathname: the candidate moved
        // into staging under the coordinator, and re-resolving that name would
        // reintroduce the substitution window the retained-descriptor discipline
        // exists to close (`previous_tree_move.rs`).
        //
        // `try_clone` duplicates the descriptor — same inode, no path lookup —
        // so the handle outlives the borrow without weakening that property.
        let (retained_candidate_usr, _) = archived.retained_candidate_usr();
        let candidate_usr = retained_candidate_usr
            .try_clone()
            .map_err(|source| LiveNewStateBootError::at("retain staged candidate descriptor", source))?;
        self.complete_new_state_boot(NewStateBootSource::Archived(archived), &candidate_usr, candidate, stone)?;
        Ok(())
    }

    /// Walk a committed transition to its terminal deletion.
    ///
    /// Stopping at `CommitDecided` would leave a live journal record, which the
    /// next startup reports as `RecoveryPending` — the transition would look
    /// interrupted even though it succeeded.
    fn finish_new_state_transition(
        &self,
        journal: crate::transition_journal::TransitionJournalStore,
        record: crate::transition_journal::TransitionRecord,
        reservation: &crate::client::active_state_snapshot::ActiveStateReservation,
    ) -> Result<(), LiveNewStateBootError> {
        let journal = crate::client::startup_recovery::finish_activation_after_commit(
            journal,
            &self.state_db,
            &self.installation,
            record,
            reservation,
        )
        .map_err(|source| LiveNewStateBootError::at("terminal completion", source))?;
        drop(journal);
        Ok(())
    }

    /// Prepare the sealed boot inputs for a candidate the pre-journal probe
    /// already judged bootable.
    ///
    /// Disagreement here means the namespace or database moved after the record
    /// was written, so the journal now asserts a boot that cannot happen. That
    /// must fail loudly rather than silently skip boot.
    fn require_new_state_boot_inputs(
        &self,
        boot_candidate: &State,
    ) -> Result<PreparedActiveReblitStoneBootInputs, LiveNewStateBootError> {
        let input_deadline = deadline_after(BOOT_PUBLICATION_TIMEOUT, "boot input deadline")?;
        match PreparedActiveReblitStoneBootInputs::prepare_until(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            boot_candidate,
            input_deadline,
        )
        .map_err(|source| LiveNewStateBootError::at("boot applicability", source))?
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => Ok(stone),
            ActiveReblitStoneBootInputsOutcome::NotApplicable(_) => Err(LiveNewStateBootError::at(
                "boot applicability",
                NewStateBootNotApplicable,
            )),
        }
    }
}

/// Where a NewState transition enters boot publication.
///
/// Replacing an active state enters from `PreviousArchived`, once the
/// predecessor is the durable rollback anchor. A first install has no
/// predecessor, so it enters one phase earlier, from `SystemTriggersComplete`.
pub(in crate::client) enum NewStateBootSource {
    Archived(PreviousArchivedCoordinator),
    Unarchived(SystemTriggersCompleteCoordinator),
}

impl Client {
    /// Publish boot entries for the freshly created NewState candidate and drive
    /// the durable journal to `Complete`. Not yet wired: `new_state` routes
    /// through this once the coordinated forward path replaces the legacy
    /// commit (Phase 1 Slice 5).
    #[allow(dead_code)]
    pub(in crate::client) fn complete_new_state_boot(
        &self,
        coordinator: NewStateBootSource,
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
        // The coordinator is consumed only here, after every boot input is
        // prepared, so a preparation failure leaves the journal untouched.
        let handoff = match coordinator {
            NewStateBootSource::Archived(coordinator) => coordinator
                .into_new_state_boot_sync_handoff()
                .map_err(|source| LiveNewStateBootError::at("new state boot handoff", source))?,
            NewStateBootSource::Unarchived(coordinator) => coordinator
                .into_new_state_unarchived_boot_sync_handoff()
                .map_err(|source| LiveNewStateBootError::at("unarchived new state boot handoff", source))?,
        };
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
            .query_bounded(&candidate_packages, PROSPECTIVE_LAYOUT_BOUNDS, || {
                Instant::now() <= deadline
            })
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
                    projection
                        .as_ref()
                        .expect("a tail exists only with a projection")
                        .layouts(),
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
