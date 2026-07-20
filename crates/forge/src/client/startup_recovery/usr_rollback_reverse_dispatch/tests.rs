//! Real mutable-startup contracts for rollback-reverse dispatch.

mod durability_restart;
mod evidence_races;
mod fresh_handle_restart;
mod journal_restart;
mod journal_update_process_kill;
mod process_kill_restart;
mod root_links_record_binding;
mod success_matrix;
mod support;
mod syscall_results;
