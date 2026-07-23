//! Client-owned capabilities for one coordinator-authorized `/usr` exchange.
//!
//! This module is deliberately crate-private and has no live callsite.  It
//! binds the exact mutable Installation clone, old-live active-state writer
//! lease, retained-root ABI preflight, and (only for ActiveReblit) immutable
//! state snapshot into one consuming effect authority.

use thiserror::Error;

use crate::{
    Installation, State, state,
    transition_identity::{Error as TreeIdentityError, StatefulTreeIdentity},
    transition_journal::{Operation, StorageError, TransitionJournalStore},
};

use super::{
    Error as ClientError, RetainedRootAbi, RootAbiPreflight,
    active_state_authority::ActiveStateAuthority, active_state_authority::AppliedActiveStateWriterAuthority,
    active_state_snapshot::ActiveStateReservation,
};

#[derive(Debug, Error)]
pub(crate) enum JournalUsrExchangeAuthorityError {
    #[error("revalidate coordinator-owned client authority before or after the /usr exchange")]
    Client(#[source] ClientError),
    #[error("inspect the canonical transition journal without waiting while writer authority is held")]
    Journal(#[source] StorageError),
    #[error("prepare coordinator-owned tree identity without waiting behind the journal")]
    Identity(#[source] TreeIdentityError),
    #[error("pre-journal /usr exchange authority found unresolved transition {transition}")]
    UnresolvedJournal { transition: String },
    #[error("{operation:?} expected old active state {expected:?}, retained active-state authority names {actual:?}")]
    ActiveStateMismatch {
        operation: Operation,
        expected: Option<i32>,
        actual: Option<i32>,
    },
    #[error("{operation:?} requires ActiveReblit state snapshot presence={expected}, found presence={actual}")]
    ActiveReblitPresenceMismatch {
        operation: Operation,
        expected: bool,
        actual: bool,
    },
    #[error("ActiveReblit expected state {expected}, retained exact snapshot is state {actual}")]
    ActiveReblitStateMismatch { expected: i32, actual: i32 },
}

/// Writer-first authority captured before tree identity opens and retains the
/// journal lock.  Marker publication intentionally invalidates its old-live
/// proof, so callers must consume this value through the refresh handoff
/// before beginning a durable transition.
#[allow(dead_code)] // carried by the intentionally unwired coordinator contract
pub(crate) struct JournalUsrExchangeAuthorityPreflight {
    installation: Installation,
    active_state: ActiveStateAuthority,
    root_abi: RootAbiPreflight,
    active_reblit: Option<State>,
}

/// Unforgeable proof that tree-identity preparation is running while the
/// coordinator still owns writer-first client authority.  The identity layer
/// uses this proof only to select nonblocking journal acquisition; legacy
/// callers retain their blocking journal behavior.
pub(crate) struct JournalUsrExchangePreparationSeal {
    _private: (),
}

impl std::fmt::Debug for JournalUsrExchangeAuthorityPreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JournalUsrExchangeAuthorityPreflight")
            .field("installation_root", &self.installation.root)
            .field("active_state", &"<redacted old-live proof>")
            .field("root_abi", &self.root_abi)
            .field("active_reblit", &self.active_reblit)
            .finish()
    }
}

/// Pre-effect authority whose active-state proof still names the old live
/// tree.  It is non-cloneable and must be consumed by the exchange effect.
pub(crate) struct JournalUsrExchangeAuthority {
    installation: Installation,
    active_state: ActiveStateAuthority,
    root_abi: RootAbiPreflight,
    active_reblit: Option<State>,
}

impl std::fmt::Debug for JournalUsrExchangeAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JournalUsrExchangeAuthority")
            .field("installation_root", &self.installation.root)
            .field("active_state", &"<redacted old-live proof>")
            .field("root_abi", &self.root_abi)
            .field("active_reblit", &self.active_reblit)
            .finish()
    }
}

/// Post-effect authority.  The invalidated old-live proof is reduced to an
/// opaque writer guard, while the retained Installation and root-ABI proof
/// remain available for later coordinator phases.  No root links are
/// published by this type.
#[derive(Debug)]
pub(crate) struct AppliedJournalUsrExchangeAuthority {
    installation: Installation,
    _active_state_writer: AppliedActiveStateWriterAuthority,
    root_abi: RootAbiPreflight,
    active_reblit: Option<State>,
}

/// Post-publication authority for the exact merged-/usr root ABI.
///
/// This value can be obtained only by consuming the pre-journal root-ABI
/// preflight carried through the `/usr` exchange.  It retains the same
/// Installation and cooperating-writer lease together with descriptor-pinned
/// evidence for all five public links; no caller can reconstruct it by
/// reopening the installation root after the exchange.
#[derive(Debug)]
pub(crate) struct PublishedJournalRootAbiAuthority {
    installation: Installation,
    _active_state_writer: AppliedActiveStateWriterAuthority,
    root_abi: RetainedRootAbi,
    active_reblit: Option<State>,
}

impl JournalUsrExchangeAuthority {
    pub(crate) fn require_pre_exchange(
        &self,
        operation: Operation,
        candidate: state::Id,
        previous: Option<state::Id>,
    ) -> Result<(), JournalUsrExchangeAuthorityError> {
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.active_state
            .revalidate(&self.installation)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;

        let actual = self.active_state.active();
        if actual != previous {
            return Err(JournalUsrExchangeAuthorityError::ActiveStateMismatch {
                operation,
                expected: previous.map(i32::from),
                actual: actual.map(i32::from),
            });
        }
        let expected_reblit = operation == Operation::ActiveReblit;
        if self.active_reblit.is_some() != expected_reblit {
            return Err(JournalUsrExchangeAuthorityError::ActiveReblitPresenceMismatch {
                operation,
                expected: expected_reblit,
                actual: self.active_reblit.is_some(),
            });
        }
        if let Some(active_reblit) = &self.active_reblit
            && active_reblit.id != candidate
        {
            return Err(JournalUsrExchangeAuthorityError::ActiveReblitStateMismatch {
                expected: i32::from(candidate),
                actual: i32::from(active_reblit.id),
            });
        }

        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)
    }

    pub(crate) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(crate) fn active_reblit(&self) -> Option<&State> {
        self.active_reblit.as_ref()
    }

    pub(crate) fn into_applied(self) -> AppliedJournalUsrExchangeAuthority {
        let Self {
            installation,
            active_state,
            root_abi,
            active_reblit,
        } = self;
        AppliedJournalUsrExchangeAuthority {
            installation,
            _active_state_writer: active_state.into_applied_writer_authority(),
            root_abi,
            active_reblit,
        }
    }
}

impl JournalUsrExchangeAuthorityPreflight {
    /// Acquire every client capability in writer-before-journal order, then
    /// inspect journal absence through a nonblocking canonical lock attempt.
    #[allow(dead_code)] // deliberately unwired until startup recovery executes phases
    pub(in crate::client) fn inspect(
        installation: &Installation,
        active_state: ActiveStateAuthority,
        active_reblit: Option<State>,
    ) -> Result<Self, JournalUsrExchangeAuthorityError> {
        installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        active_state
            .revalidate(installation)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        let root_abi = RootAbiPreflight::open_retained(&installation.root, installation.root_directory())
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;

        let cast = installation
            .retained_mutable_cast_directory()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root)
            .map_err(JournalUsrExchangeAuthorityError::Journal)?;
        if let Some(record) = journal.load().map_err(JournalUsrExchangeAuthorityError::Journal)? {
            return Err(JournalUsrExchangeAuthorityError::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }
        drop(journal);
        active_state
            .revalidate(installation)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        Ok(Self {
            installation: installation.clone(),
            active_state,
            root_abi,
            active_reblit,
        })
    }

    #[cfg(test)]
    pub(crate) fn acquire_prejournal_for_test(
        installation: &Installation,
        active_reblit: Option<State>,
    ) -> Result<Self, JournalUsrExchangeAuthorityError> {
        let active_state =
            ActiveStateAuthority::acquire(installation).map_err(JournalUsrExchangeAuthorityError::Client)?;
        Self::inspect(installation, active_state, active_reblit)
    }

    /// Prepare a fresh-state identity without ever waiting behind a journal
    /// owner which may itself be waiting for this authority's writer lease.
    pub(crate) fn prepare_unallocated_candidate(
        self,
        state_db: &crate::db::state::Database,
        candidate_path: &std::path::Path,
    ) -> Result<(StatefulTreeIdentity, JournalUsrExchangeAuthority), JournalUsrExchangeAuthorityError> {
        let identity = StatefulTreeIdentity::prepare_usr_exchange_unallocated_candidate(
            &self.installation,
            state_db,
            candidate_path,
            &JournalUsrExchangePreparationSeal { _private: () },
        )
        .map_err(JournalUsrExchangeAuthorityError::Identity)?;
        let authority = self.refresh_after_tree_identity_preparation()?;
        Ok((identity, authority))
    }

    /// Prepare an archived-state identity under the same nonblocking journal
    /// handoff used by every coordinator-owned exchange operation.
    pub(crate) fn prepare_candidate(
        self,
        state_db: &crate::db::state::Database,
        candidate_path: &std::path::Path,
        candidate_state: state::Id,
    ) -> Result<(StatefulTreeIdentity, JournalUsrExchangeAuthority), JournalUsrExchangeAuthorityError> {
        let identity = StatefulTreeIdentity::prepare_usr_exchange_candidate(
            &self.installation,
            state_db,
            candidate_path,
            candidate_state,
            &JournalUsrExchangePreparationSeal { _private: () },
        )
        .map_err(JournalUsrExchangeAuthorityError::Identity)?;
        let authority = self.refresh_after_tree_identity_preparation()?;
        Ok((identity, authority))
    }

    /// Prepare an active-reblit identity under the same nonblocking journal
    /// handoff used by every coordinator-owned exchange operation.
    pub(crate) fn prepare_active_reblit_identity(
        self,
        state_db: &crate::db::state::Database,
        candidate_path: &std::path::Path,
        candidate_state: state::Id,
    ) -> Result<(StatefulTreeIdentity, JournalUsrExchangeAuthority), JournalUsrExchangeAuthorityError> {
        let identity = StatefulTreeIdentity::prepare_usr_exchange_active_reblit_candidate(
            &self.installation,
            state_db,
            candidate_path,
            candidate_state,
            &JournalUsrExchangePreparationSeal { _private: () },
        )
        .map_err(JournalUsrExchangeAuthorityError::Identity)?;
        let authority = self.refresh_after_tree_identity_preparation()?;
        Ok((identity, authority))
    }

    /// Prepare an ActiveReblit identity from the candidate capability retained
    /// by fixed staging. `candidate_path` must be the canonical fixed-staging
    /// `/usr` path. This constructor does not resolve that path while preparing
    /// the identity; later coordinator phases may use it for canonical-name
    /// checks.
    pub(crate) fn prepare_retained_active_reblit_identity(
        self,
        state_db: &crate::db::state::Database,
        candidate_usr: &std::fs::File,
        candidate_path: &std::path::Path,
        candidate_state: state::Id,
    ) -> Result<(StatefulTreeIdentity, JournalUsrExchangeAuthority), JournalUsrExchangeAuthorityError> {
        let identity = StatefulTreeIdentity::prepare_usr_exchange_retained_active_reblit_candidate(
            &self.installation,
            state_db,
            candidate_usr,
            candidate_path,
            candidate_state,
            &JournalUsrExchangePreparationSeal { _private: () },
        )
        .map_err(JournalUsrExchangeAuthorityError::Identity)?;
        let authority = self.refresh_after_tree_identity_preparation()?;
        Ok((identity, authority))
    }

    /// Admit exactly the marker change performed by tree-identity
    /// preparation while preserving the old live tree and writer mutex.
    fn refresh_after_tree_identity_preparation(
        mut self,
    ) -> Result<JournalUsrExchangeAuthority, JournalUsrExchangeAuthorityError> {
        self.active_state
            .revalidate(&self.installation)
            .or_else(|_| {
                self.active_state
                    .refresh_after_tree_identity_preparation(&self.installation)
            })
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        Ok(JournalUsrExchangeAuthority {
            installation: self.installation,
            active_state: self.active_state,
            root_abi: self.root_abi,
            active_reblit: self.active_reblit,
        })
    }
}

impl AppliedJournalUsrExchangeAuthority {
    pub(crate) fn require_post_exchange(&self) -> Result<(), JournalUsrExchangeAuthorityError> {
        // The old-live ActiveStateAuthority must never be revalidated here:
        // the intentional exchange invalidated it.  The opaque replacement
        // above retains only its cooperating-writer exclusion.
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)
    }

    pub(crate) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(crate) fn active_reblit(&self) -> Option<&State> {
        self.active_reblit.as_ref()
    }

    /// Consume the original pre-journal authorization and publish the public
    /// merged-/usr links through its already-retained installation root.
    /// Publication is monotonic and may have partially applied when an error
    /// is returned, so neither this authority nor its writer lease is returned
    /// on failure.
    pub(crate) fn publish_root_abi(self) -> Result<PublishedJournalRootAbiAuthority, ClientError> {
        let Self {
            installation,
            _active_state_writer,
            root_abi,
            active_reblit,
        } = self;
        let root_abi = root_abi.publish()?;
        Ok(PublishedJournalRootAbiAuthority {
            installation,
            _active_state_writer,
            root_abi,
            active_reblit,
        })
    }
}

impl PublishedJournalRootAbiAuthority {
    /// Revalidate the exchanged namespace and every descriptor-pinned public
    /// root link without reacquiring mutation authority through a pathname.
    pub(crate) fn require_post_exchange(&self) -> Result<(), JournalUsrExchangeAuthorityError> {
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi()
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.installation
            .revalidate_mutable_namespace()
            .map_err(ClientError::from)
            .map_err(JournalUsrExchangeAuthorityError::Client)?;
        self.root_abi()
            .revalidate()
            .map_err(JournalUsrExchangeAuthorityError::Client)
    }

    pub(crate) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(crate) fn active_reblit(&self) -> Option<&State> {
        self.active_reblit.as_ref()
    }

    pub(crate) fn root_abi(&self) -> &RetainedRootAbi {
        &self.root_abi
    }

    /// Consume every post-publication proof while preserving the exact
    /// cooperating-writer lease as startup-style reservation authority.
    pub(crate) fn into_active_state_reservation(self) -> ActiveStateReservation {
        let Self {
            installation: _,
            _active_state_writer,
            root_abi: _,
            active_reblit: _,
        } = self;
        _active_state_writer.into_active_state_reservation()
    }
}
