//! Admissibility of the journal-only NewState boot-tail edges.
//!
//! A first install has no predecessor tree, so the two edges that follow the
//! commit decision reclaim nothing: `CommitDecided -> CommitCleanupComplete`
//! and `CommitCleanupComplete -> Complete` are pure journal advances.
//!
//! Admission deliberately refuses a record that carries a predecessor. That
//! shape still needs the reclaiming ActiveReblit route, and advancing it
//! without the exchange would strand the predecessor's staging wrapper outside
//! quarantine.

use crate::transition_journal::{Operation, Phase, TransitionRecord};

/// Whether this record is one of the exact first-install journal-only edges,
/// advancing to `successor`.
pub(in crate::client) fn new_state_boot_tail_edge_is_admissible(record: &TransitionRecord, successor: Phase) -> bool {
    let edge_is_known = matches!(
        (record.phase, successor),
        (Phase::CommitDecided, Phase::CommitCleanupComplete) | (Phase::CommitCleanupComplete, Phase::Complete)
    );
    edge_is_known && record_is_exact_first_install_boot_tail(record)
}

/// Whether this record is the exact first-install terminal deletion point.
pub(in crate::client) fn new_state_boot_tail_terminal_is_admissible(record: &TransitionRecord) -> bool {
    record.phase == Phase::Complete && record_is_exact_first_install_boot_tail(record)
}

fn record_is_exact_first_install_boot_tail(record: &TransitionRecord) -> bool {
    record.operation == Operation::NewState
        && record.rollback.is_none()
        // No predecessor, and therefore nothing archived and nothing to reclaim.
        && record.previous.id.is_none()
        && !record.options.archive_previous
        && record.boot_publication_receipts.is_some()
        && crate::client::active_reblit_boot_sync_staging::supports_boot_sync(record.operation)
        && crate::client::active_reblit_boot_sync_staging::boot_tail_generation_is_exact(record)
        && crate::client::active_reblit_boot_sync_staging::boot_tail_options_are_exact(record)
        && crate::client::active_reblit_boot_sync_staging::boot_tail_identity_is_exact(record)
}

#[cfg(test)]
pub(in crate::client) fn new_state_boot_tail_edge_is_admissible_for_test(
    record: &TransitionRecord,
    successor: Phase,
) -> bool {
    new_state_boot_tail_edge_is_admissible(record, successor)
}

#[cfg(test)]
pub(in crate::client) fn new_state_boot_tail_terminal_is_admissible_for_test(record: &TransitionRecord) -> bool {
    new_state_boot_tail_terminal_is_admissible(record)
}
