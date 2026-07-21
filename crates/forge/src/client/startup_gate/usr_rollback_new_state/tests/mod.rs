//! Real startup-entry contracts for the NewState rollback suffix.

mod candidate_move_process_harness;
mod candidate_move_process_kill;
mod candidate_process_kill_boundaries;
mod exclusions;
mod failures;
mod finalization;
mod finalization_restart;
mod fresh_db_invalidation_process_boundaries;
mod fresh_db_invalidation_process_evidence;
mod fresh_db_invalidation_process_harness;
mod fresh_db_invalidation_process_kill;
mod matrix;
mod preparation_failures;
mod sequence;
mod storage_faults;
mod support;
mod terminal_delete_process_kill;
