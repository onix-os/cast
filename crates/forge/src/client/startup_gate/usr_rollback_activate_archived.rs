//! Test-sealed startup capability for the ActivateArchived rollback suffix.
//!
//! Production dispatch is deliberately absent. The first route foundation is
//! exercised only through focused contracts until the operation-specific
//! candidate rearchive checkpoint is independently complete.

/// Unforgeable safe-code token limiting future ActivateArchived completion
/// routing to its operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteSeal {
    _private: (),
}

impl UsrRollbackActivateArchivedCompleteRouteSeal {
    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider candidate-authority helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider recovery construction helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;
