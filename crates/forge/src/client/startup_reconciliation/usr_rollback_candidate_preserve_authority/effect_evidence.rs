//! Shared non-namespace evidence sandwich for candidate-preservation effects.
//!
//! Target creation, residue normalization, and candidate movement have
//! different namespace authority, but all require the same exact journal,
//! database, plan, installation, and per-open binding checks immediately
//! around that authority's consumption.

use crate::{
    Installation, db,
    transition_journal::{Operation, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveAuthorityErrorKind,
    candidate_preserve_plan_is_exact, inspect_current_database, require_exact_database,
    require_journal_record_binding,
};

pub(super) fn require_effect_binding(
    installation: &Installation,
    expected: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    require_journal_record_binding(installation, journal, expected, record)
}

pub(super) fn require_pre_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal_record_binding: &TransitionJournalRecordBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_new_state_candidate_preserve_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

pub(super) fn require_post_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal_record_binding: &TransitionJournalRecordBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    require_exact_new_state_candidate_preserve_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_new_state_candidate_preserve_plan(
    record: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if record.operation == Operation::NewState && candidate_preserve_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
    }
}
