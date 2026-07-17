//! Focused executor contracts for terminal NewState rollback finalization.

mod delete_report;
mod evidence_races;
mod matrix;
mod post_delete_evidence;
mod public_binding_races;
#[allow(dead_code, unused_imports)]
#[path = "../../usr_rollback_complete_route/tests/support.rs"]
mod route_support;
mod storage_reconciliation;
mod support;
