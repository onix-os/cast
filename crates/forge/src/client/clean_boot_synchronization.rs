//! Clean-journal authority for standalone boot synchronization.
//!
//! A previously constructed client can outlive a failed transition.  The
//! active-state writer lease alone therefore cannot authorize `boot sync`:
//! the exact mutable journal must remain locked and absent, and the state
//! database must remain free of orphan transition ownership, across the boot
//! backend call.

use thiserror::Error;

use crate::{Installation, db, transition_journal};

use super::{Error as ClientError, active_state_snapshot::ActiveStateLease};

/// Retained proof that standalone boot synchronization is disjoint from every
/// journal-owned transition.
///
/// This value is intentionally non-`Clone`.  Its journal store keeps the
/// canonical exclusive lock alive from admission through post-effect
/// revalidation, while the borrowed active-state lease retains the
/// coordinator acquired before that journal lock.
pub(super) struct CleanBootSynchronizationAuthority<'authority> {
    installation: &'authority Installation,
    state_db: &'authority db::state::Database,
    active_state: &'authority ActiveStateLease,
    journal: transition_journal::TransitionJournalStore,
}

impl<'authority> CleanBootSynchronizationAuthority<'authority> {
    pub(super) fn capture(
        installation: &'authority Installation,
        state_db: &'authority db::state::Database,
        active_state: &'authority ActiveStateLease,
    ) -> Result<Self, CleanBootSynchronizationAuthorityError> {
        installation.revalidate_mutable_namespace()?;
        active_state.revalidate(installation).map_err(active_state_error)?;
        let cast = installation.retained_mutable_cast_directory()?;
        let journal = transition_journal::TransitionJournalStore::open_in_retained_cast(cast, &installation.root);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let authority = Self {
            installation,
            state_db,
            active_state,
            journal: journal?,
        };
        authority.revalidate()?;
        Ok(authority)
    }

    /// Repeat the public-aware journal, database, mutable-namespace, and live
    /// state proof without releasing the retained journal lock.
    pub(super) fn revalidate(&self) -> Result<(), CleanBootSynchronizationAuthorityError> {
        self.installation.revalidate_mutable_namespace()?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        let record = self.journal.load_revalidated_retained_cast(cast);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let record = record?;
        if let Some(record) = record {
            return Err(CleanBootSynchronizationAuthorityError::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }

        let in_flight = self.state_db.audit_in_flight_transition();
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        self.active_state
            .revalidate(self.installation)
            .map_err(active_state_error)?;

        // The database and active-state checks do not retain the public
        // `journal` child. Repeat the public-aware load last so replacing that
        // child after the leading check cannot overlap this clean authority
        // through a different lock inode.
        before_final_journal_revalidation();
        let cast = self.installation.retained_mutable_cast_directory()?;
        let trailing_record = self.journal.load_revalidated_retained_cast(cast);
        let namespace = self.installation.revalidate_mutable_namespace();
        namespace?;
        let trailing_record = trailing_record?;
        if let Some(record) = trailing_record {
            return Err(CleanBootSynchronizationAuthorityError::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }
        if let Some(orphan) = in_flight? {
            return Err(CleanBootSynchronizationAuthorityError::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Test-only boundary immediately after the backend returns but before
    /// its result can be selected over retained authority failure.
    pub(super) fn before_post_revalidation(&self) {
        before_post_revalidation(&self.journal);
    }
}

fn active_state_error(source: ClientError) -> CleanBootSynchronizationAuthorityError {
    CleanBootSynchronizationAuthorityError::ActiveState(Box::new(source))
}

#[derive(Debug, Error)]
pub(super) enum CleanBootSynchronizationAuthorityError {
    #[error("revalidate retained mutable installation during standalone boot synchronization")]
    Installation(#[from] crate::installation::Error),
    #[error("open, bind, or load the canonical transition journal during standalone boot synchronization")]
    Journal(#[from] transition_journal::StorageError),
    #[error("audit transition ownership during standalone boot synchronization")]
    Database(#[from] db::state::TransitionEvidenceError),
    #[error("revalidate retained active-state authority during standalone boot synchronization")]
    ActiveState(#[source] Box<ClientError>),
    #[error("standalone boot synchronization is blocked by unresolved transition {transition}")]
    UnresolvedJournal { transition: String },
    #[error("standalone boot synchronization is blocked by orphan transition {transition} on state {state}")]
    OrphanTransitionRow { state: i32, transition: String },
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_POST_REVALIDATION: std::cell::RefCell<
        Option<Box<dyn FnOnce(&transition_journal::TransitionJournalStore)>>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_post_revalidation(hook: impl FnOnce(&transition_journal::TransitionJournalStore) + 'static) {
    BEFORE_POST_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_post_revalidation(journal: &transition_journal::TransitionJournalStore) {
    BEFORE_POST_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(journal);
        }
    });
}

#[cfg(not(test))]
fn before_post_revalidation(_journal: &transition_journal::TransitionJournalStore) {}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_JOURNAL_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_final_journal_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_JOURNAL_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_final_journal_revalidation() {
    BEFORE_FINAL_JOURNAL_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_final_journal_revalidation() {}

#[cfg(test)]
mod tests;
