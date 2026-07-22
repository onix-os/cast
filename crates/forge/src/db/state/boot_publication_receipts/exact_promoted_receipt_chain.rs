//! Exact read-only loading of one promoted receipt and its predecessor body.
//!
//! The compact journal pair names the installed receipt and its optional
//! predecessor, but startup cannot safely reconstruct either body from caller
//! data. This module loads both through the existing exact promoted-state
//! validator in one deferred SQLite transaction and returns authority-free
//! canonical data. It performs no head mutation, body mutation, publication,
//! replacement, deletion, or cleanup effect.

use diesel::{Connection as _, QueryDsl as _, RunQueryDsl as _};

use super::{
    BootPublicationReceiptPair, BootPublicationReceiptState,
    CanonicalBootPublicationReceipt, Database, boot_publication_receipts,
    load_receipt_state,
    promotion::{
        ExactPromotedBootPublicationReceiptStateError,
        load_exact_promoted_state_with_predecessor,
    },
};
use crate::state::TransitionId;

/// Exact canonical promoted receipt chain authenticated from durable storage.
///
/// The installed receipt remains inside its validated promoted state. The
/// optional predecessor is the exact immutable body named by both the compact
/// pair and the installed body's `committed_predecessor` field. These values
/// are inert data and grant no filesystem or database mutation authority.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ExactPromotedBootPublicationReceiptChain {
    promoted_state: BootPublicationReceiptState,
    committed_predecessor: Option<CanonicalBootPublicationReceipt>,
}

/// Current installed receipt state authenticated without caller correlation.
///
/// `Empty` means both the exact durable head and immutable receipt storage are
/// empty. `Installed` contains the sole committed head body and the immutable
/// predecessor body it names, when one exists. Neither variant grants mutation
/// authority.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CurrentExactPromotedBootPublicationReceiptChain {
    Empty,
    Installed(ExactPromotedBootPublicationReceiptChain),
}

/// Fail-closed error while deriving the current installed chain from storage.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CurrentExactPromotedBootPublicationReceiptChainError {
    #[error(transparent)]
    ExactPromoted(#[from] ExactPromotedBootPublicationReceiptStateError),
    #[error(
        "the boot-publication receipt head is empty but immutable storage retains {count} bodies"
    )]
    ReceiptBodiesWithoutCommittedHead { count: i64 },
}

impl From<diesel::result::Error>
    for CurrentExactPromotedBootPublicationReceiptChainError
{
    fn from(source: diesel::result::Error) -> Self {
        Self::ExactPromoted(ExactPromotedBootPublicationReceiptStateError::from(source))
    }
}

impl ExactPromotedBootPublicationReceiptChain {
    /// Return the exact receipt installed at the authenticated committed head.
    pub(crate) fn installed_receipt(&self) -> &CanonicalBootPublicationReceipt {
        self.promoted_state
            .committed()
            .expect("exact promoted chain retains its installed receipt")
    }

    /// Return the exact canonical predecessor named by the installed receipt.
    pub(crate) fn committed_predecessor_receipt(
        &self,
    ) -> Option<&CanonicalBootPublicationReceipt> {
        self.committed_predecessor.as_ref()
    }
}

impl Database {
    /// Load the current exact installed receipt chain without caller identity.
    ///
    /// The transition, installed fingerprint, and predecessor fingerprint are
    /// derived only from the strict head and canonical body loaded inside this
    /// deferred read transaction. A pending head is never update-admission
    /// evidence. An empty head is admitted only when immutable receipt storage
    /// is also empty, so a lost head cannot silently become first adoption.
    pub(crate) fn load_current_exact_promoted_boot_publication_receipt_chain(
        &self,
    ) -> Result<
        CurrentExactPromotedBootPublicationReceiptChain,
        CurrentExactPromotedBootPublicationReceiptChainError,
    > {
        self.conn.exec(|connection| {
            connection.transaction(|connection| {
                let state = load_receipt_state(connection)
                    .map_err(ExactPromotedBootPublicationReceiptStateError::from)?;
                if let Some(pending) = state.head().pending() {
                    return Err(
                        ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent {
                            transition_id: pending.transition_id().clone(),
                            fingerprint: pending.fingerprint(),
                        }
                        .into(),
                    );
                }
                if state.pending().is_some() {
                    return Err(
                        ExactPromotedBootPublicationReceiptStateError::PendingBodyPresent.into(),
                    );
                }

                let Some(installed) = state.committed() else {
                    let stored_receipts = boot_publication_receipts::table
                        .count()
                        .get_result::<i64>(connection)
                        .map_err(ExactPromotedBootPublicationReceiptStateError::from)?;
                    if stored_receipts != 0 {
                        return Err(
                            CurrentExactPromotedBootPublicationReceiptChainError::ReceiptBodiesWithoutCommittedHead {
                                count: stored_receipts,
                            },
                        );
                    }
                    return Ok(CurrentExactPromotedBootPublicationReceiptChain::Empty);
                };

                let transition_id = installed.body().transition_id().clone();
                let pair = BootPublicationReceiptPair {
                    committed: installed.body().committed_predecessor(),
                    pending: installed.fingerprint(),
                };
                let (promoted_state, committed_predecessor) =
                    load_exact_promoted_state_with_predecessor(
                        connection,
                        &transition_id,
                        &pair,
                    )?;
                Ok(CurrentExactPromotedBootPublicationReceiptChain::Installed(
                    ExactPromotedBootPublicationReceiptChain {
                        promoted_state,
                        committed_predecessor,
                    },
                ))
            })
        })
    }

    /// Load one exact promoted installed receipt and its optional predecessor.
    ///
    /// The existing promoted-state validator authenticates the empty pending
    /// slot, exact committed head and body, transition, and installed-to-
    /// predecessor correlation. The predecessor lookup then decodes its
    /// canonical body and verifies its storage key, body fingerprint, and
    /// transition field before either receipt leaves this single read
    /// transaction.
    pub(crate) fn load_exact_promoted_boot_publication_receipt_chain(
        &self,
        transition_id: &TransitionId,
        pair: &BootPublicationReceiptPair,
    ) -> Result<
        ExactPromotedBootPublicationReceiptChain,
        ExactPromotedBootPublicationReceiptStateError,
    > {
        self.conn.exec(|connection| {
            connection.transaction(|connection| {
                let (promoted_state, committed_predecessor) =
                    load_exact_promoted_state_with_predecessor(
                        connection,
                        transition_id,
                        pair,
                    )?;
                let loaded_predecessor = committed_predecessor
                    .as_ref()
                    .map(CanonicalBootPublicationReceipt::fingerprint);
                if loaded_predecessor != pair.committed {
                    return Err(
                        ExactPromotedBootPublicationReceiptStateError::CommittedPredecessorMismatch {
                            expected: pair.committed,
                            actual: loaded_predecessor,
                        },
                    );
                }
                Ok(ExactPromotedBootPublicationReceiptChain {
                    promoted_state,
                    committed_predecessor,
                })
            })
        })
    }
}
