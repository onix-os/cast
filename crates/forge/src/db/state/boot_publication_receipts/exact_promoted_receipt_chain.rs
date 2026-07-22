//! Exact read-only loading of one promoted receipt and its predecessor body.
//!
//! The compact journal pair names the installed receipt and its optional
//! predecessor, but startup cannot safely reconstruct either body from caller
//! data. This module loads both through the existing exact promoted-state
//! validator in one deferred SQLite transaction and returns authority-free
//! canonical data. It performs no head mutation, body mutation, publication,
//! replacement, deletion, or cleanup effect.

use diesel::Connection as _;

use super::{
    BootPublicationReceiptPair, BootPublicationReceiptState,
    CanonicalBootPublicationReceipt, Database,
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
