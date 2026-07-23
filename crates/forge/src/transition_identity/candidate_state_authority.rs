//! Three-way retained authority for a stateful candidate's `.stateID`.
//!
//! A missing state ID is not one generic condition. A fresh candidate has no
//! state yet, while an active reblit already has a database identity whose
//! newly materialized tree must remain undecorated until the durable
//! `CandidatePrepareStarted` boundary. Archived activation instead retains an
//! exact existing `.stateID` inode from preparation onward.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateStatePreparation {
    UnknownIdAbsent,
    KnownIdAbsent(state::Id),
    ExistingId(state::Id),
}

#[derive(Debug)]
pub(super) enum RetainedCandidateStateId {
    UnknownIdAbsent,
    KnownIdAbsent(state::Id),
    ExistingId(state_tree_metadata::RetainedTreeStateId),
}

impl CandidateStatePreparation {
    pub(super) fn requires_absent_id(self) -> bool {
        matches!(self, Self::UnknownIdAbsent | Self::KnownIdAbsent(_))
    }

    pub(super) fn existing_id(self) -> Option<state::Id> {
        match self {
            Self::ExistingId(state) => Some(state),
            Self::UnknownIdAbsent | Self::KnownIdAbsent(_) => None,
        }
    }

    pub(super) fn retain(
        self,
        candidate: &mut RetainedIdentity,
        candidate_path: &Path,
    ) -> Result<RetainedCandidateStateId, Error> {
        match self {
            Self::UnknownIdAbsent => Ok(RetainedCandidateStateId::UnknownIdAbsent),
            Self::KnownIdAbsent(state) => Ok(RetainedCandidateStateId::KnownIdAbsent(state)),
            Self::ExistingId(expected) => {
                let retained = candidate.state_id.take().ok_or_else(|| {
                    live_usr_io(
                        "retain exact existing candidate state ID authority",
                        candidate_path,
                        io::Error::other("candidate state ID identity was not retained"),
                    )
                })?;
                if retained.state() != expected {
                    return Err(live_usr_io(
                        "bind existing candidate state ID authority",
                        candidate_path,
                        io::Error::other(format!(
                            "expected candidate state {}, retained {}",
                            i32::from(expected),
                            i32::from(retained.state())
                        )),
                    ));
                }
                Ok(RetainedCandidateStateId::ExistingId(retained))
            }
        }
    }
}

impl RetainedCandidateStateId {
    pub(super) fn kind_and_state(&self) -> (&'static str, Option<state::Id>) {
        match self {
            Self::UnknownIdAbsent => ("unknown-ID/absent", None),
            Self::KnownIdAbsent(state) => ("known-ID/absent", Some(*state)),
            Self::ExistingId(retained) => ("existing-ID", Some(retained.state())),
        }
    }

    pub(super) fn verify_initial(&self, candidate: &RetainedIdentity) -> Result<(), Error> {
        match self {
            Self::UnknownIdAbsent | Self::KnownIdAbsent(_) => {
                candidate.verify_store_read_only(&candidate.store)?;
                state_tree_metadata::RetainedTreeStateId::require_absent(&candidate.store)?;
                candidate.verify_store_read_only(&candidate.store)?;
                state_tree_metadata::RetainedTreeStateId::require_absent(&candidate.store)
            }
            Self::ExistingId(state_id) => candidate.verify_store_with_retained_state_id(&candidate.store, state_id),
        }
    }

    pub(super) fn verify_named_existing(&self, candidate: &RetainedIdentity, path: &Path) -> Result<(), Error> {
        let Self::ExistingId(state_id) = self else {
            return Err(live_usr_io(
                "load retained candidate state ID authority",
                path,
                io::Error::other("candidate state ID has not been published"),
            ));
        };
        candidate.verify_named_with_retained_state_id(path, state_id)
    }

    pub(super) fn verify_store_existing(&self, candidate: &RetainedIdentity) -> Result<(), Error> {
        let Self::ExistingId(state_id) = self else {
            return Err(live_usr_io(
                "load retained candidate state ID authority",
                candidate.store.display_path(),
                io::Error::other("candidate state ID has not been published"),
            ));
        };
        candidate.verify_store_with_retained_state_id(&candidate.store, state_id)
    }
}

impl RetainedIdentity {
    fn verify_named_with_retained_state_id(
        &self,
        path: &Path,
        state_id: &state_tree_metadata::RetainedTreeStateId,
    ) -> Result<(), Error> {
        self.verify_named_read_only(path)?;
        let named_store = TreeMarkerStore::open_path(path)?;
        self.verify_store_with_retained_state_id(&named_store, state_id)
    }

    fn verify_store_with_retained_state_id(
        &self,
        named_store: &TreeMarkerStore,
        state_id: &state_tree_metadata::RetainedTreeStateId,
    ) -> Result<(), Error> {
        self.verify_store_read_only(named_store)?;
        self.store.require_same_directory(named_store)?;
        state_id.revalidate(&self.store, named_store)?;
        self.verify_store_read_only(named_store)
    }
}

impl StatefulTreeIdentity {
    pub(super) fn verify_candidate_named_with_state_id(&self, path: &Path) -> Result<(), Error> {
        self.candidate_state_id.verify_named_existing(&self.candidate, path)
    }

    pub(super) fn verify_candidate_store_with_state_id(&self) -> Result<(), Error> {
        self.candidate_state_id.verify_store_existing(&self.candidate)
    }
}
