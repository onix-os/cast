//! Direct same-store ActivateArchived terminal-delete reconciliation contracts.

#[allow(dead_code)]
#[path = "../../../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[allow(dead_code)]
#[path = "../../test_support.rs"]
mod test_fixture;

mod reconcile_delete;
mod support;
