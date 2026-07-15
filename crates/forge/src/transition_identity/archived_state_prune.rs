//! Retained, detach-first removal of authenticated archived state wrappers.
//!
//! One session owns the transition-journal lock for the entire batch.  Every
//! wrapper is authenticated and moved no-replace into a freshly reserved
//! private quarantine before its exact database snapshot may be removed.
//! Recursive deletion begins only after that checked database commit.
//!
//! This coordinator retains in-memory retry state but writes no durable prune
//! intent. Process death after database removal therefore strands the private
//! quarantine as fail-closed operator evidence; a later process must not adopt
//! or delete it. Durable crash reopening is deliberately not claimed here.

mod deletion;
mod error;
mod fault_injection;

use std::{
    ffi::{CStr, CString},
    io,
    os::{fd::AsRawFd as _, unix::fs::MetadataExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    Installation, State, db,
    linux_fs::{controlled_resolution, openat2_file, renameat2_noreplace_once},
    state,
    transition_journal::TransitionJournalStore,
    tree_marker::TreeMarkerStore,
};

use super::{
    QUARANTINE_RELATIVE, ROOTS_RELATIVE, RetainedDirectory, RetainedIdentity, canonical_state_name,
    state_slot_marker::RetainedStateSlotMarker,
};

use self::{
    deletion::{DeleteBudget, DeletionPlan, remove_exact_private_directory},
    fault_injection::{before_wrapper_move, checkpoint},
};

pub(crate) use error::{ArchivedStatePruneError, ArchivedStatePruneMoveOutcome};
pub(crate) use fault_injection::ArchivedStatePruneFaultPoint;
#[cfg(test)]
pub(crate) use fault_injection::{
    arm_archived_state_prune_fault, arm_before_archived_state_prune_child_unlink,
    arm_before_archived_state_prune_wrapper_move,
};

pub(super) const WRAPPER_NAME: &CStr = c"wrapper";
const USR_NAME: &CStr = c"usr";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
pub(crate) const MAX_ARCHIVED_STATE_PRUNE_BATCH: usize = 64;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ArchivedStatePruneLimits {
    pub(crate) depth: usize,
    pub(crate) entries: usize,
    pub(crate) name_bytes: usize,
    pub(crate) operations: usize,
    pub(crate) retained_nodes: usize,
    pub(crate) time: Duration,
}

impl Default for ArchivedStatePruneLimits {
    fn default() -> Self {
        Self {
            depth: 128,
            entries: 1_000_000,
            name_bytes: 64 * 1024 * 1024,
            operations: 4_000_000,
            retained_nodes: 256,
            time: Duration::from_secs(120),
        }
    }
}

#[derive(Debug)]
struct PreparedArchivedState {
    state: state::Id,
    canonical_name: CString,
    wrapper: RetainedDirectory,
    identity: RetainedIdentity,
    _slot_marker: Option<RetainedStateSlotMarker>,
    slot_name: CString,
    slot: RetainedDirectory,
    quarantine_path: PathBuf,
    deletion: Option<DeletionPlan>,
    reservation_retired: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrunePhase {
    Prepared,
    Detached,
    RowsRemoved,
    DatabaseAmbiguous,
    Restored,
    Deleted,
}

impl PrunePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Detached => "detached",
            Self::RowsRemoved => "rows-removed",
            Self::DatabaseAmbiguous => "database-ambiguous",
            Self::Restored => "restored",
            Self::Deleted => "deleted",
        }
    }
}

/// One journal-locked archived-state prune batch.
#[derive(Debug)]
pub(crate) struct RetainedArchivedStatePrune {
    journal: TransitionJournalStore,
    roots: RetainedDirectory,
    quarantine: RetainedDirectory,
    expected: Vec<State>,
    states: Vec<PreparedArchivedState>,
    limits: ArchivedStatePruneLimits,
    delete_budget: Option<DeleteBudget>,
    retirement_budget: Option<DeleteBudget>,
    phase: PrunePhase,
}

/// Diagnostic result for one exact wrapper detached by the retained session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DetachedArchivedState {
    pub(crate) state: state::Id,
    pub(crate) quarantine: PathBuf,
}

impl RetainedArchivedStatePrune {
    pub(crate) fn prepare(
        installation: &Installation,
        state_db: &db::state::Database,
        states: &[State],
    ) -> Result<Self, ArchivedStatePruneError> {
        Self::prepare_with_limits(installation, state_db, states, ArchivedStatePruneLimits::default())
    }

    fn prepare_with_limits(
        installation: &Installation,
        state_db: &db::state::Database,
        states: &[State],
        limits: ArchivedStatePruneLimits,
    ) -> Result<Self, ArchivedStatePruneError> {
        if states.is_empty() {
            return Err(ArchivedStatePruneError::EmptyBatch);
        }
        if states.len() > MAX_ARCHIVED_STATE_PRUNE_BATCH {
            return Err(ArchivedStatePruneError::BatchTooLarge {
                actual: states.len(),
                limit: MAX_ARCHIVED_STATE_PRUNE_BATCH,
            });
        }
        installation.revalidate_root_directory()?;
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root)?;
        require_clean_baseline(&journal, state_db)?;
        super::audit_archived_state_prune_residue(installation, &journal)?;
        let roots_path = installation.root_path("");
        let quarantine_path = installation.state_quarantine_dir();
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path)
            .map_err(|source| identity_error(state::Id::default(), &installation.root, source))?;
        let quarantine =
            RetainedDirectory::open_beneath(installation.root_directory(), QUARANTINE_RELATIVE, quarantine_path)
                .map_err(|source| identity_error(state::Id::default(), &installation.root, source))?;
        if roots.witness.device != quarantine.witness.device {
            return Err(ArchivedStatePruneError::CrossDevice { state: 0 });
        }

        let mut unique = std::collections::BTreeSet::new();
        for expected in states {
            if !unique.insert(expected.id) {
                return Err(ArchivedStatePruneError::StateDatabase(
                    db::state::ExactArchivedRemovalError::Duplicate {
                        state_id: i32::from(expected.id),
                    },
                ));
            }
        }

        let mut prepared = Vec::with_capacity(states.len());
        let preparation = (|| {
            for expected in states {
                let actual = state_db
                    .get(expected.id)
                    .map_err(db::state::ExactArchivedRemovalError::from)?;
                if actual != *expected {
                    return Err(ArchivedStatePruneError::StateDatabase(
                        db::state::ExactArchivedRemovalError::Changed {
                            state_id: i32::from(expected.id),
                        },
                    ));
                }
                prepared.push(prepare_one(&roots, &quarantine, expected.id, limits)?);
            }
            require_clean_baseline(&journal, state_db)?;
            installation.revalidate_root_directory()?;
            Ok(())
        })();
        if let Err(primary) = preparation {
            let cleanup = retire_prepared_reservations(&quarantine, &prepared, limits);
            return match cleanup {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArchivedStatePruneError::PreparationCleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        Ok(Self {
            journal,
            roots,
            quarantine,
            expected: states.to_vec(),
            states: prepared,
            limits,
            delete_budget: None,
            retirement_budget: None,
            phase: PrunePhase::Prepared,
        })
    }

    #[cfg(test)]
    pub(crate) fn prepare_for_test(
        installation: &Installation,
        state_db: &db::state::Database,
        states: &[State],
        limits: ArchivedStatePruneLimits,
    ) -> Result<Self, ArchivedStatePruneError> {
        Self::prepare_with_limits(installation, state_db, states, limits)
    }

    #[cfg(test)]
    pub(crate) fn delete_budget_usage_for_test(&self) -> Option<(usize, std::time::Instant)> {
        self.delete_budget.as_ref().map(DeleteBudget::usage)
    }

    #[cfg(test)]
    pub(crate) fn retirement_budget_usage_for_test(&self) -> Option<(usize, std::time::Instant)> {
        self.retirement_budget.as_ref().map(DeleteBudget::usage)
    }

    /// Move every exact wrapper into its own fresh private reservation.
    /// Database rows remain untouched on every failure from this phase.
    pub(crate) fn detach_all(
        &mut self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<Vec<DetachedArchivedState>, ArchivedStatePruneError> {
        if !matches!(self.phase, PrunePhase::Prepared | PrunePhase::Detached) {
            return Err(ArchivedStatePruneError::InvalidPhase {
                operation: "detach wrappers",
                phase: self.phase.as_str(),
            });
        }
        require_clean_baseline(&self.journal, state_db)?;
        require_exact_database_snapshots(state_db, &self.expected)?;
        let mut detached = Vec::with_capacity(self.states.len());
        for state in &mut self.states {
            detach_one(installation, &self.roots, &self.quarantine, state)?;
            detached.push(DetachedArchivedState {
                state: state.state,
                quarantine: state.quarantine_path.clone(),
            });
        }
        require_clean_baseline(&self.journal, state_db)?;
        require_exact_database_snapshots(state_db, &self.expected)?;
        installation.revalidate_root_directory()?;
        self.phase = PrunePhase::Detached;
        Ok(detached)
    }

    /// Revalidate and remove the exact detached database snapshots while the
    /// same journal lock and retained namespace capabilities remain alive.
    pub(crate) fn remove_database_rows(
        &mut self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStatePruneError> {
        if self.phase != PrunePhase::Detached {
            return Err(ArchivedStatePruneError::InvalidPhase {
                operation: "remove exact state rows",
                phase: self.phase.as_str(),
            });
        }
        require_clean_baseline(&self.journal, state_db)?;
        require_exact_database_snapshots(state_db, &self.expected)?;
        for state in &self.states {
            require_detached_layout(installation, &self.roots, &self.quarantine, state)?;
            require_strict_wrapper(state)?;
        }
        if let Err(source) = state_db.remove_exact_archived(&self.expected) {
            self.phase = if source.definitely_not_applied() {
                PrunePhase::Detached
            } else {
                PrunePhase::DatabaseAmbiguous
            };
            return Err(source.into());
        }
        self.phase = PrunePhase::RowsRemoved;
        for state in &self.states {
            require_detached_layout(installation, &self.roots, &self.quarantine, state)?;
            require_strict_wrapper(state)?;
        }
        if self.journal.load()?.is_some() {
            return Err(ArchivedStatePruneError::AmbiguousLayout {
                state: 0,
                quarantine: self.quarantine.path.clone(),
            });
        }
        Ok(())
    }

    /// Delete only the exact inodes retained through detachment, using a
    /// bounded descriptor-rooted walk. No public archive path is reopened.
    pub(crate) fn delete_detached(&mut self, installation: &Installation) -> Result<(), ArchivedStatePruneError> {
        if self.phase != PrunePhase::RowsRemoved {
            return Err(ArchivedStatePruneError::InvalidPhase {
                operation: "delete detached wrappers",
                phase: self.phase.as_str(),
            });
        }
        installation.revalidate_root_directory()?;
        let limits = self.limits;
        let budget = self.delete_budget.get_or_insert_with(|| DeleteBudget::new(limits));
        for state in &mut self.states {
            delete_one(&self.quarantine, state, budget)?;
        }
        self.quarantine
            .sync("sync archived-state prune quarantine after deletion")
            .map_err(|source| identity_error(state::Id::default(), &self.quarantine.path, source))?;
        installation.revalidate_root_directory()?;
        self.phase = PrunePhase::Deleted;
        Ok(())
    }

    /// Restore detached wrappers no-replace without retiring their private
    /// reservations.
    ///
    /// Keeping the empty reservation names present is a deliberate startup
    /// sentinel while an external side effect such as boot projection is
    /// being compensated. A process loss before that compensation completes
    /// must remain visible to the next startup audit.
    pub(crate) fn restore_wrappers(&mut self, installation: &Installation) -> Result<(), ArchivedStatePruneError> {
        if !matches!(
            self.phase,
            PrunePhase::Prepared | PrunePhase::Detached | PrunePhase::Restored
        ) {
            return Err(ArchivedStatePruneError::InvalidPhase {
                operation: "restore detached wrappers",
                phase: self.phase.as_str(),
            });
        }

        if self.phase != PrunePhase::Restored {
            for state in self.states.iter_mut().rev() {
                restore_one(installation, &self.roots, &self.quarantine, state)?;
            }
            self.phase = PrunePhase::Restored;
        }

        Ok(())
    }

    /// Retire the empty private reservations only after every required
    /// external compensation has completed successfully.
    pub(crate) fn retire_reservations(&mut self) -> Result<(), ArchivedStatePruneError> {
        if self.phase != PrunePhase::Restored {
            return Err(ArchivedStatePruneError::InvalidPhase {
                operation: "retire restored archived-state reservations",
                phase: self.phase.as_str(),
            });
        }

        let budget = self
            .retirement_budget
            .get_or_insert_with(|| DeleteBudget::new(self.limits));
        let mut cleanup_failure = None;
        for state in self.states.iter_mut().rev() {
            if state.reservation_retired {
                continue;
            }
            match retire_reservation(&self.quarantine, state, budget) {
                Ok(()) => state.reservation_retired = true,
                Err(source) if cleanup_failure.is_none() => cleanup_failure = Some(source),
                Err(_) => {}
            }
        }
        if let Some(source) = cleanup_failure {
            return Err(source);
        }
        Ok(())
    }

    /// Restore wrappers and retire reservations when no external side effect
    /// needs compensation.
    pub(crate) fn restore_all(&mut self, installation: &Installation) -> Result<(), ArchivedStatePruneError> {
        self.restore_wrappers(installation)?;
        self.retire_reservations()
    }
}

fn retire_prepared_reservations(
    quarantine: &RetainedDirectory,
    states: &[PreparedArchivedState],
    limits: ArchivedStatePruneLimits,
) -> Result<(), ArchivedStatePruneError> {
    let mut budget = DeleteBudget::new(limits);
    let mut cleanup_failure = None;
    for state in states.iter().rev() {
        if let Err(source) = retire_reservation(quarantine, state, &mut budget)
            && cleanup_failure.is_none()
        {
            cleanup_failure = Some(source);
        }
    }
    if let Some(source) = cleanup_failure {
        return Err(source);
    }
    Ok(())
}

fn retire_reservation(
    quarantine: &RetainedDirectory,
    state: &PreparedArchivedState,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    remove_exact_private_directory(quarantine, &state.slot_name, &state.slot, budget)
}

fn delete_one(
    quarantine: &RetainedDirectory,
    state: &mut PreparedArchivedState,
    budget: &mut DeleteBudget,
) -> Result<(), ArchivedStatePruneError> {
    if state.deletion.is_none() {
        require_location(&state.slot, WRAPPER_NAME, &state.wrapper, true, &state.quarantine_path)?;
        state.deletion = Some(DeletionPlan::prepare(&state.slot)?);
    }
    state.deletion.as_mut().expect("deletion plan was prepared").run(
        &state.slot,
        &state.wrapper,
        WRAPPER_NAME,
        budget,
    )?;
    retire_reservation(quarantine, state, budget)
}

fn prepare_one(
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: state::Id,
    limits: ArchivedStatePruneLimits,
) -> Result<PreparedArchivedState, ArchivedStatePruneError> {
    let canonical_name = canonical_state_name(state).map_err(|source| identity_error(state, &roots.path, source))?;
    let wrapper_path = roots.path.join(canonical_name.to_string_lossy().as_ref());
    let wrapper = roots
        .open_child(&canonical_name, wrapper_path.clone())
        .map_err(|source| identity_error(state, &wrapper_path, source))?;
    let usr_path = wrapper_path.join("usr");
    let usr = wrapper
        .open_child(USR_NAME, usr_path.clone())
        .map_err(|source| identity_error(state, &usr_path, source))?;
    let store = TreeMarkerStore::open(&usr.file, &usr_path)
        .map_err(super::Error::from)
        .map_err(|source| identity_error(state, &usr_path, source))?;
    let marker = store
        .read_for_transition_recovery()
        .map_err(super::Error::from)
        .map_err(|source| identity_error(state, &usr_path, source))?;
    let identity = RetainedIdentity::with_marker(store, marker, Some(state))
        .map_err(|source| identity_error(state, &usr_path, source))?;
    let slot_marker = if identity.marker.needs_slot_link_authorization() {
        let slot_marker = RetainedStateSlotMarker::open_recovery_candidate(&wrapper, state, &identity.marker)
            .map_err(super::Error::from)
            .map_err(|source| identity_error(state, &wrapper_path, source))?;
        wrapper
            .require_exact_entries(&[USR_NAME.to_bytes(), slot_marker.name_bytes()])
            .map_err(|source| identity_error(state, &wrapper_path, source))?;
        identity
            .marker
            .authorize_recovered_slot_link()
            .map_err(super::Error::from)
            .map_err(|source| identity_error(state, &usr_path, source))?;
        slot_marker
            .require_named(&wrapper)
            .map_err(super::Error::from)
            .map_err(|source| identity_error(state, &wrapper_path, source))?;
        Some(slot_marker)
    } else {
        wrapper
            .require_exact_entries(&[USR_NAME.to_bytes()])
            .map_err(|source| identity_error(state, &wrapper_path, source))?;
        None
    };
    let named_store = TreeMarkerStore::open(&usr.file, &usr_path)
        .map_err(super::Error::from)
        .map_err(|source| identity_error(state, &usr_path, source))?;
    identity
        .verify_store_with_state_id(&named_store)
        .map_err(|source| identity_error(state, &usr_path, source))?;

    let slot_name = archived_state_prune_quarantine_name(state, identity.marker.token().as_str())?;
    let slot_path = quarantine.path.join(slot_name.to_string_lossy().as_ref());
    let slot = match RetainedDirectory::create_private_child(quarantine, &slot_name, slot_path.clone()) {
        Ok(slot) => slot,
        Err(super::Error::QuarantineSlotExists { .. }) => {
            return Err(ArchivedStatePruneError::QuarantineCollision { path: slot_path });
        }
        Err(source) => return Err(identity_error(state, &slot_path, source)),
    };
    let reservation_ready = (|| {
        if slot.witness.mode != PRIVATE_DIRECTORY_MODE {
            return Err(ArchivedStatePruneError::WrapperLayoutChanged {
                state: i32::from(state),
                path: slot.path.clone(),
            });
        }
        slot.sync("sync empty archived-state prune reservation")
            .map_err(|source| identity_error(state, &slot.path, source))?;
        quarantine
            .sync("sync archived-state prune reservation name")
            .map_err(|source| identity_error(state, &slot.path, source))?;
        Ok(())
    })();
    if let Err(primary) = reservation_ready {
        let mut budget = DeleteBudget::new(limits);
        let cleanup = remove_exact_private_directory(quarantine, &slot_name, &slot, &mut budget);
        return match cleanup {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(ArchivedStatePruneError::PreparationCleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        };
    }
    Ok(PreparedArchivedState {
        state,
        canonical_name,
        wrapper,
        identity,
        _slot_marker: slot_marker,
        slot_name,
        quarantine_path: slot_path.join("wrapper"),
        slot,
        deletion: None,
        reservation_retired: false,
    })
}

fn detach_one(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &mut PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    revalidate_parents(installation, roots, quarantine, state)?;
    match observe_move_layout(roots, state)? {
        ArchivedStatePruneMoveOutcome::Applied => {
            return finish_detach_suffix(installation, roots, quarantine, state);
        }
        ArchivedStatePruneMoveOutcome::NotApplied => {}
        ArchivedStatePruneMoveOutcome::Ambiguous => {
            return Err(ArchivedStatePruneError::AmbiguousLayout {
                state: i32::from(state.state),
                quarantine: state.quarantine_path.clone(),
            });
        }
    }
    require_strict_wrapper(state)?;
    require_location(roots, &state.canonical_name, &state.wrapper, true, &state.wrapper.path)?;
    require_name_absent(&state.slot.file, WRAPPER_NAME, &state.quarantine_path)?;
    state
        .identity
        .store
        .sync_retained_tree()
        .map_err(super::Error::from)
        .map_err(|source| identity_error(state.state, &state.wrapper.path, source))?;
    state
        .wrapper
        .sync("sync archived wrapper before prune detachment")
        .map_err(|source| identity_error(state.state, &state.wrapper.path, source))?;
    state
        .slot
        .sync("sync archived-state prune slot before detachment")
        .map_err(|source| identity_error(state.state, &state.slot.path, source))?;
    roots
        .sync("sync archived roots before prune detachment")
        .map_err(|source| identity_error(state.state, &roots.path, source))?;
    quarantine
        .sync("sync archived-state quarantine before detachment")
        .map_err(|source| identity_error(state.state, &quarantine.path, source))?;
    revalidate_parents(installation, roots, quarantine, state)?;

    revalidate_parents(installation, roots, quarantine, state)?;
    require_location(roots, &state.canonical_name, &state.wrapper, true, &state.wrapper.path)?;
    require_name_absent(&state.slot.file, WRAPPER_NAME, &state.quarantine_path)?;
    // This hook is deliberately in the actual final-check/syscall window.
    // The one-shot no-replace rename may move a racing foreign source, after
    // which exact destination reconciliation preserves it as evidence and
    // reports ambiguity instead of deleting or adopting it.
    before_wrapper_move();
    let syscall = renameat2_noreplace_once(&roots.file, &state.canonical_name, &state.slot.file, WRAPPER_NAME);
    let layout = observe_move_layout(roots, state)?;
    match layout {
        ArchivedStatePruneMoveOutcome::Applied => {}
        ArchivedStatePruneMoveOutcome::NotApplied => {
            return Err(ArchivedStatePruneError::Move {
                state: i32::from(state.state),
                outcome: layout,
                quarantine: state.quarantine_path.clone(),
                source: syscall
                    .err()
                    .unwrap_or_else(|| io::Error::other("rename reported success without moving wrapper")),
            });
        }
        ArchivedStatePruneMoveOutcome::Ambiguous => {
            return Err(ArchivedStatePruneError::AmbiguousLayout {
                state: i32::from(state.state),
                quarantine: state.quarantine_path.clone(),
            });
        }
    }
    checkpoint(ArchivedStatePruneFaultPoint::AfterQuarantineMove)?;
    finish_detach_suffix(installation, roots, quarantine, state)
}

fn finish_detach_suffix(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    state
        .wrapper
        .sync("sync detached archived wrapper")
        .map_err(|source| identity_error(state.state, &state.quarantine_path, source))?;
    roots
        .sync("sync archived roots after prune detachment")
        .map_err(|source| identity_error(state.state, &roots.path, source))?;
    state
        .slot
        .sync("sync archived-state prune slot after detachment")
        .map_err(|source| identity_error(state.state, &state.slot.path, source))?;
    quarantine
        .sync("sync archived-state quarantine after detachment")
        .map_err(|source| identity_error(state.state, &quarantine.path, source))?;
    require_detached_layout(installation, roots, quarantine, state)?;
    require_strict_wrapper(state)
}

fn restore_one(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &mut PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    revalidate_parents(installation, roots, quarantine, state)?;
    match observe_move_layout(roots, state)? {
        ArchivedStatePruneMoveOutcome::NotApplied => {
            return finish_restore_suffix(installation, roots, quarantine, state);
        }
        ArchivedStatePruneMoveOutcome::Applied => {}
        ArchivedStatePruneMoveOutcome::Ambiguous => {
            return Err(ArchivedStatePruneError::AmbiguousLayout {
                state: i32::from(state.state),
                quarantine: state.quarantine_path.clone(),
            });
        }
    }
    require_strict_wrapper(state)?;
    let syscall = renameat2_noreplace_once(&state.slot.file, WRAPPER_NAME, &roots.file, &state.canonical_name);
    match observe_move_layout(roots, state)? {
        ArchivedStatePruneMoveOutcome::NotApplied => {}
        ArchivedStatePruneMoveOutcome::Applied => {
            return Err(ArchivedStatePruneError::Move {
                state: i32::from(state.state),
                outcome: ArchivedStatePruneMoveOutcome::NotApplied,
                quarantine: state.quarantine_path.clone(),
                source: syscall
                    .err()
                    .unwrap_or_else(|| io::Error::other("restore reported success without moving wrapper")),
            });
        }
        ArchivedStatePruneMoveOutcome::Ambiguous => {
            return Err(ArchivedStatePruneError::AmbiguousLayout {
                state: i32::from(state.state),
                quarantine: state.quarantine_path.clone(),
            });
        }
    }
    finish_restore_suffix(installation, roots, quarantine, state)
}

fn finish_restore_suffix(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    state
        .wrapper
        .sync("sync restored archived wrapper")
        .map_err(|source| identity_error(state.state, &state.wrapper.path, source))?;
    roots
        .sync("sync archived roots after prune restoration")
        .map_err(|source| identity_error(state.state, &roots.path, source))?;
    state
        .slot
        .sync("sync archived-state prune slot after restoration")
        .map_err(|source| identity_error(state.state, &state.slot.path, source))?;
    quarantine
        .sync("sync archived-state quarantine after restoration")
        .map_err(|source| identity_error(state.state, &quarantine.path, source))?;
    revalidate_parents(installation, roots, quarantine, state)?;
    require_location(roots, &state.canonical_name, &state.wrapper, true, &state.wrapper.path)?;
    require_strict_wrapper(state)
}

fn require_detached_layout(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    revalidate_parents(installation, roots, quarantine, state)?;
    require_name_absent(&roots.file, &state.canonical_name, &state.wrapper.path)?;
    require_location(&state.slot, WRAPPER_NAME, &state.wrapper, true, &state.quarantine_path)?;
    state
        .identity
        .revalidate_retained()
        .map_err(|source| identity_error(state.state, &state.quarantine_path, source))?;
    require_strict_wrapper(state)
}

fn require_strict_wrapper(state: &PreparedArchivedState) -> Result<(), ArchivedStatePruneError> {
    let expected_entries = match &state._slot_marker {
        Some(marker) => vec![USR_NAME.to_bytes(), marker.name_bytes()],
        None => vec![USR_NAME.to_bytes()],
    };
    state
        .wrapper
        .require_exact_entries(&expected_entries)
        .map_err(|source| identity_error(state.state, &state.wrapper.path, source))?;
    if let Some(marker) = &state._slot_marker {
        marker
            .require_named(&state.wrapper)
            .map_err(super::Error::from)
            .map_err(|source| identity_error(state.state, &state.wrapper.path, source))?;
    }
    let usr_path = state.wrapper.path.join("usr");
    let usr = state
        .wrapper
        .open_child(USR_NAME, usr_path.clone())
        .map_err(|source| identity_error(state.state, &usr_path, source))?;
    let named_store = TreeMarkerStore::open(&usr.file, &usr_path)
        .map_err(super::Error::from)
        .map_err(|source| identity_error(state.state, &usr_path, source))?;
    state
        .identity
        .verify_store_with_state_id(&named_store)
        .map_err(|source| identity_error(state.state, &usr_path, source))
}

fn revalidate_parents(
    installation: &Installation,
    roots: &RetainedDirectory,
    quarantine: &RetainedDirectory,
    state: &PreparedArchivedState,
) -> Result<(), ArchivedStatePruneError> {
    installation.revalidate_root_directory()?;
    roots
        .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
        .map_err(|source| identity_error(state.state, &roots.path, source))?;
    quarantine
        .revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)
        .map_err(|source| identity_error(state.state, &quarantine.path, source))?;
    state
        .slot
        .revalidate_child(quarantine, &state.slot_name)
        .map_err(|source| identity_error(state.state, &state.slot.path, source))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameState {
    Absent,
    Exact,
    Foreign,
}

fn observe_move_layout(
    roots: &RetainedDirectory,
    state: &PreparedArchivedState,
) -> Result<ArchivedStatePruneMoveOutcome, ArchivedStatePruneError> {
    let source = location(roots, &state.canonical_name, &state.wrapper, &state.wrapper.path)?;
    let destination = location(&state.slot, WRAPPER_NAME, &state.wrapper, &state.quarantine_path)?;
    Ok(match (source, destination) {
        (NameState::Exact, NameState::Absent) => ArchivedStatePruneMoveOutcome::NotApplied,
        (NameState::Absent, NameState::Exact) => ArchivedStatePruneMoveOutcome::Applied,
        _ => ArchivedStatePruneMoveOutcome::Ambiguous,
    })
}

fn location(
    parent: &RetainedDirectory,
    name: &CStr,
    expected: &RetainedDirectory,
    path: &Path,
) -> Result<NameState, ArchivedStatePruneError> {
    match openat2_file(
        parent.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(NameState::Absent),
        Err(source)
            if matches!(
                source.raw_os_error(),
                Some(nix::libc::ENOTDIR) | Some(nix::libc::ELOOP) | Some(nix::libc::EXDEV)
            ) =>
        {
            Ok(NameState::Foreign)
        }
        Err(source) => Err(error::prune_io("observe archived-state prune name", path, source)),
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|source| error::prune_io("inspect archived-state prune name", path, source))?;
            if (metadata.dev(), metadata.ino()) == (expected.witness.device, expected.witness.inode) {
                Ok(NameState::Exact)
            } else {
                Ok(NameState::Foreign)
            }
        }
    }
}

fn require_location(
    parent: &RetainedDirectory,
    name: &CStr,
    expected: &RetainedDirectory,
    exact: bool,
    path: &Path,
) -> Result<(), ArchivedStatePruneError> {
    let actual = location(parent, name, expected, path)?;
    if (actual == NameState::Exact) == exact {
        Ok(())
    } else {
        Err(ArchivedStatePruneError::WrapperLayoutChanged {
            state: 0,
            path: path.to_owned(),
        })
    }
}

fn require_name_absent(parent: &std::fs::File, name: &CStr, path: &Path) -> Result<(), ArchivedStatePruneError> {
    match openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Err(source) if source.raw_os_error() == Some(nix::libc::EXDEV) => {
            Err(ArchivedStatePruneError::MountedEntry { path: path.to_owned() })
        }
        Err(source) => Err(error::prune_io("prove archived-state prune name absence", path, source)),
        Ok(_) => Err(ArchivedStatePruneError::WrapperLayoutChanged {
            state: 0,
            path: path.to_owned(),
        }),
    }
}

fn require_clean_baseline(
    journal: &TransitionJournalStore,
    state_db: &db::state::Database,
) -> Result<(), ArchivedStatePruneError> {
    if let Some(record) = journal.load()? {
        return Err(ArchivedStatePruneError::UnresolvedJournal {
            transition: record.transition_id.as_str().to_owned(),
        });
    }
    if let Some(orphan) = state_db.audit_in_flight_transition()? {
        return Err(ArchivedStatePruneError::UnresolvedJournal {
            transition: orphan.transition_id.as_str().to_owned(),
        });
    }
    Ok(())
}

fn require_exact_database_snapshots(
    state_db: &db::state::Database,
    expected: &[State],
) -> Result<(), ArchivedStatePruneError> {
    for expected in expected {
        let actual = state_db
            .get(expected.id)
            .map_err(db::state::ExactArchivedRemovalError::from)?;
        if actual != *expected {
            return Err(ArchivedStatePruneError::StateDatabase(
                db::state::ExactArchivedRemovalError::Changed {
                    state_id: i32::from(expected.id),
                },
            ));
        }
    }
    Ok(())
}

pub(crate) fn archived_state_prune_quarantine_name(
    state: state::Id,
    token: &str,
) -> Result<CString, ArchivedStatePruneError> {
    let state_value = i32::from(state);
    if state_value <= 0
        || token.len() != state::TransitionId::TEXT_LENGTH
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ArchivedStatePruneError::WrapperLayoutChanged {
            state: state_value,
            path: PathBuf::from("<invalid-prune-quarantine-name>"),
        });
    }
    CString::new(format!("state-prune-{state_value}-{token}"))
        .map_err(|source| error::prune_io("encode archived-state prune quarantine name", "<name>", source.into()))
}

fn identity_error(state: state::Id, path: &Path, source: super::Error) -> ArchivedStatePruneError {
    ArchivedStatePruneError::Identity {
        state: i32::from(state),
        path: path.to_owned(),
        source: Box::new(source),
    }
}
