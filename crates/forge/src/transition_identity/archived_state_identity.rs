//! Read-only authentication for one canonical archived-state wrapper.
//!
//! This module owns only retained descriptors and immutable identity evidence.
//! It deliberately has no rename, link, cleanup, or deletion operation.  The
//! prune coordinator and boot-state projection share this exact authentication
//! prefix instead of independently deciding what constitutes an archived tree.

use std::{ffi::CString, path::PathBuf};

use thiserror::Error;

use crate::{state, tree_marker::TreeMarkerStore};

use super::{
    Error as IdentityError, RetainedDirectory, RetainedIdentity, canonical_state_name,
    state_slot_marker::RetainedStateSlotMarker,
};

const USR_NAME: &std::ffi::CStr = c"usr";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArchivedStateIdentityStage {
    StateName,
    Wrapper,
    Usr,
    TreeMarker,
    StateId,
    SlotMarker,
    WrapperLayout,
    FinalVerification,
}

impl ArchivedStateIdentityStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::StateName => "state name",
            Self::Wrapper => "wrapper",
            Self::Usr => "usr directory",
            Self::TreeMarker => "tree marker",
            Self::StateId => "state ID",
            Self::SlotMarker => "state-slot marker",
            Self::WrapperLayout => "wrapper layout",
            Self::FinalVerification => "final identity",
        }
    }
}

#[derive(Debug, Error)]
#[error(
    "authenticate archived state {state} {stage} at `{}`",
    path.display(),
    stage = .stage.as_str()
)]
pub(crate) struct ArchivedStateIdentityError {
    state: state::Id,
    stage: ArchivedStateIdentityStage,
    path: PathBuf,
    #[source]
    source: IdentityError,
}

impl ArchivedStateIdentityError {
    pub(super) fn stage(&self) -> ArchivedStateIdentityStage {
        self.stage
    }

    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(super) fn source_identity(&self) -> &IdentityError {
        &self.source
    }

    pub(super) fn into_source(self) -> IdentityError {
        self.source
    }
}

#[derive(Debug)]
pub(super) struct RetainedArchivedStateIdentity {
    pub(super) state: state::Id,
    pub(super) canonical_name: CString,
    pub(super) wrapper: RetainedDirectory,
    pub(super) identity: RetainedIdentity,
    pub(super) slot_marker: Option<RetainedStateSlotMarker>,
}

impl RetainedArchivedStateIdentity {
    pub(super) fn retain(roots: &RetainedDirectory, state: state::Id) -> Result<Self, ArchivedStateIdentityError> {
        let canonical_name = canonical_state_name(state)
            .map_err(|source| failure(state, ArchivedStateIdentityStage::StateName, roots.path.clone(), source))?;
        let wrapper_path = roots.path.join(canonical_name.to_string_lossy().as_ref());
        let wrapper = roots
            .open_child(&canonical_name, wrapper_path.clone())
            .map_err(|source| failure(state, ArchivedStateIdentityStage::Wrapper, wrapper_path.clone(), source))?;
        let usr_path = wrapper_path.join("usr");
        let usr = wrapper
            .open_child(USR_NAME, usr_path.clone())
            .map_err(|source| failure(state, ArchivedStateIdentityStage::Usr, usr_path.clone(), source))?;
        let store = TreeMarkerStore::open(&usr.file, &usr_path)
            .map_err(IdentityError::from)
            .map_err(|source| failure(state, ArchivedStateIdentityStage::Usr, usr_path.clone(), source))?;
        let marker = store
            .read_for_transition_recovery()
            .map_err(IdentityError::from)
            .map_err(|source| failure(state, ArchivedStateIdentityStage::TreeMarker, usr_path.clone(), source))?;
        let identity = RetainedIdentity::with_marker(store, marker, Some(state))
            .map_err(|source| failure(state, ArchivedStateIdentityStage::StateId, usr_path.clone(), source))?;

        let slot_marker = if identity.marker.needs_slot_link_authorization() {
            let slot_marker = RetainedStateSlotMarker::open_recovery_candidate(&wrapper, state, &identity.marker)
                .map_err(IdentityError::from)
                .map_err(|source| {
                    failure(
                        state,
                        ArchivedStateIdentityStage::SlotMarker,
                        wrapper_path.clone(),
                        source,
                    )
                })?;
            wrapper
                .require_exact_entries(&[USR_NAME.to_bytes(), slot_marker.name_bytes()])
                .map_err(|source| {
                    failure(
                        state,
                        ArchivedStateIdentityStage::WrapperLayout,
                        wrapper_path.clone(),
                        source,
                    )
                })?;
            identity
                .marker
                .authorize_recovered_slot_link()
                .map_err(IdentityError::from)
                .map_err(|source| failure(state, ArchivedStateIdentityStage::SlotMarker, usr_path.clone(), source))?;
            slot_marker
                .require_named(&wrapper)
                .map_err(IdentityError::from)
                .map_err(|source| {
                    failure(
                        state,
                        ArchivedStateIdentityStage::SlotMarker,
                        wrapper_path.clone(),
                        source,
                    )
                })?;
            Some(slot_marker)
        } else {
            wrapper
                .require_exact_entries(&[USR_NAME.to_bytes()])
                .map_err(|source| {
                    failure(
                        state,
                        ArchivedStateIdentityStage::WrapperLayout,
                        wrapper_path.clone(),
                        source,
                    )
                })?;
            None
        };

        let retained = Self {
            state,
            canonical_name,
            wrapper,
            identity,
            slot_marker,
        };
        retained.revalidate_contents()?;
        Ok(retained)
    }

    pub(super) fn revalidate_named(&self, roots: &RetainedDirectory) -> Result<(), ArchivedStateIdentityError> {
        self.wrapper
            .revalidate_child(roots, &self.canonical_name)
            .map_err(|source| {
                failure(
                    self.state,
                    ArchivedStateIdentityStage::Wrapper,
                    self.wrapper.path.clone(),
                    source,
                )
            })?;
        self.revalidate_contents()
    }

    pub(super) fn revalidate_contents(&self) -> Result<(), ArchivedStateIdentityError> {
        Self::revalidate_parts(self.state, &self.wrapper, &self.identity, self.slot_marker.as_ref())
    }

    pub(super) fn revalidate_parts(
        state: state::Id,
        wrapper: &RetainedDirectory,
        identity: &RetainedIdentity,
        slot_marker: Option<&RetainedStateSlotMarker>,
    ) -> Result<(), ArchivedStateIdentityError> {
        let expected_entries = match slot_marker {
            Some(marker) => vec![USR_NAME.to_bytes(), marker.name_bytes()],
            None => vec![USR_NAME.to_bytes()],
        };
        wrapper.require_exact_entries(&expected_entries).map_err(|source| {
            failure(
                state,
                ArchivedStateIdentityStage::WrapperLayout,
                wrapper.path.clone(),
                source,
            )
        })?;
        if let Some(marker) = slot_marker {
            marker
                .require_named(wrapper)
                .map_err(IdentityError::from)
                .map_err(|source| {
                    failure(
                        state,
                        ArchivedStateIdentityStage::SlotMarker,
                        wrapper.path.clone(),
                        source,
                    )
                })?;
        }

        let usr_path = wrapper.path.join("usr");
        let usr = wrapper
            .open_child(USR_NAME, usr_path.clone())
            .map_err(|source| failure(state, ArchivedStateIdentityStage::Usr, usr_path.clone(), source))?;
        let named_store = TreeMarkerStore::open(&usr.file, &usr_path)
            .map_err(IdentityError::from)
            .map_err(|source| failure(state, ArchivedStateIdentityStage::Usr, usr_path.clone(), source))?;
        identity
            .verify_store_with_state_id(&named_store)
            .map_err(|source| failure(state, ArchivedStateIdentityStage::FinalVerification, usr_path, source))
    }

    pub(super) fn usr(&self) -> &std::fs::File {
        self.identity.store.retained_directory()
    }
}

fn failure(
    state: state::Id,
    stage: ArchivedStateIdentityStage,
    path: PathBuf,
    source: IdentityError,
) -> ArchivedStateIdentityError {
    ArchivedStateIdentityError {
        state,
        stage,
        path,
        source,
    }
}
