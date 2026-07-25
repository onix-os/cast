//! Direct same-store ActiveReblit bound-delete error contracts.

#[allow(dead_code)] // shared `#[path]` test-support module; each including parent consumes only a subset
#[path = "../../../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[allow(dead_code)] // shared `#[path]` test-support module; each including parent consumes only a subset
#[path = "../../test_support.rs"]
mod test_fixture;

mod reconcile_delete;
mod support;
