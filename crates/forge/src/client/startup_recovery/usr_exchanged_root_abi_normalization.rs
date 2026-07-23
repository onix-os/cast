//! One-shot executor for the exact forward-`UsrExchanged` root ABI boundary.
//!
//! This leaf has no journal-advance capability. Incomplete canonical subsets
//! receive one retained publication attempt; complete sets receive one retained
//! root-directory synchronization before the gate fresh-captures rollback
//! decision authority.

use crate::transition_journal::TransitionJournalStore;

use super::super::startup_reconciliation::{
    UsrExchangedRootAbiDurabilityAuthority, UsrExchangedRootAbiNormalizationAuthority,
    UsrExchangedRootAbiNormalizationAuthorityError,
};

#[cfg(test)]
mod tests;

pub(in crate::client) fn normalize_usr_exchanged_root_abi(
    journal: &TransitionJournalStore,
    authority: UsrExchangedRootAbiNormalizationAuthority<'_>,
) -> Result<(), UsrExchangedRootAbiNormalizationExecutionError> {
    authority.normalize(journal)?;
    Ok(())
}

pub(in crate::client) fn synchronize_usr_exchanged_root_abi(
    journal: &TransitionJournalStore,
    authority: UsrExchangedRootAbiDurabilityAuthority<'_>,
) -> Result<(), UsrExchangedRootAbiNormalizationExecutionError> {
    authority.synchronize(journal)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrExchangedRootAbiNormalizationExecutionError {
    #[error("consume exact UsrExchanged root ABI normalization authority")]
    Authority(#[from] UsrExchangedRootAbiNormalizationAuthorityError),
}
