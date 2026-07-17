.PHONY: forge-startup-usr-rollback-candidate-preserve-admission-test

forge-startup-usr-rollback-candidate-preserve-admission-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 19; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_splits_every_exact_staged_and_preserved_matrix_case \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_accepts_new_state_empty_quarantine_prefix \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_accepts_historical_runtime_evidence \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_bypasses_other_phases_and_sources \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_plan_requires_the_exact_operation_matrix \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_rejects_a_different_open_journal_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_database_and_provenance_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_namespace_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_capture_races_defer_without_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_fresh_namespace_race_fails_revalidation \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_an_occupied_new_state_target \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_missing_wrong_extra_and_transferred_archived_slots \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_missing_duplicate_and_wrong_active_reblit_reservations \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_generic_quarantine_for_active_reblit \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_empty_and_foreign_current_state_wrappers \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_empty_transition_wrapper_for_archived_and_active_reblit \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_allows_fingerprint_bound_unrelated_state_wrappers \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_retains_a_nonzero_active_reblit_reservation_index; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {' "$$startup_gate"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$startup_gate"; \
	timeout 10s awk '$$0 == "impl UsrRollbackCandidatePreserveSeal {" { state = 1; next } state == 1 && $$0 == "    #[cfg(test)]" { gated = 1; next } state == 1 && gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { test_only = 1; gated = 0; next } state == 1 && gated { exit 1 } state == 1 && $$0 ~ /^    .*fn new/ { exit 1 } state == 1 && $$0 == "}" { found = test_only; exit !found } END { exit !found }' "$$startup_gate"; \
	production_seal_calls="$$( timeout 10s rg -n 'UsrRollbackCandidatePreserveSeal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_seal_calls" = 0; \
	production_capture_calls="$$( timeout 10s rg -n 'UsrRollbackCandidatePreserveAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_capture_calls" = 0; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackCandidatePreserveSeal,' "$$authority"; \
	timeout 10s grep -Fqx '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s grep -Fqx "    _active_state_reservation: &'reservation ActiveStateReservation," "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n 'let journal_binding = journal\.binding\(\);' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'journal\.has_binding\(&self\.journal_binding\)' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s awk '$$0 == "    fn revalidate_kind(" { active = 1; next } active && $$0 == "        self.require_journal_binding(journal)?;" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /inspect_current_database/ || $$0 ~ /self\.namespace/) { exit 1 } END { exit !found }' "$$authority"; \
	timeout 10s grep -Fq 'if record.phase != Phase::CandidatePreserveIntent {' "$$authority"; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$authority"; \
	timeout 10s grep -Fq 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority"; \
	timeout 10s grep -Fq 'Operation::ActivateArchived => rollback.candidate.disposition == AbortDisposition::Rearchive' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Apply' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Finish' "$$authority"; \
	timeout 10s grep -Fq 'NewStateStagedWithEmptyQuarantine' "$$proof"; \
	timeout 10s grep -Fq 'ArchivedStagedWithCanonicalSlot' "$$proof"; \
	timeout 10s grep -Fq 'ActiveReblitStaged { wrapper_index: usize }' "$$proof"; \
	timeout 10s grep -Fq 'candidate.marker_links() != 2' "$$proof"; \
	timeout 10s grep -Fq 'UnexpectedParkingWrapper' "$$proof"; \
	timeout 10s grep -Fq 'UnexpectedCurrentStateWrapper' "$$proof"; \
	timeout 10s grep -Fqx 'pub(in crate::client::startup_reconciliation) enum UsrRollbackCandidatePreserveTopology {' "$$proof"; \
	timeout 10s test "$$( timeout 10s rg -n 'pub\(in crate::client::startup_reconciliation\) fn topology\(' "$$authority" | timeout 10s wc -l )" = 2; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client::startup_reconciliation) fn topology(&self) -> UsrRollbackCandidatePreserveTopology {" { count++; gated = 0; next } gated { gated = 0 } END { exit count != 2 }' "$$authority"; \
	if timeout 10s rg -n 'pub\(in crate::client\) (enum|use).*UsrRollbackCandidatePreserveTopology|pub\(in crate::client\) fn topology\(' "$$authority" "$$proof" crates/forge/src/client/startup_reconciliation.rs crates/forge/src/client/startup_reconciliation/activation_namespace.rs; then exit 1; fi; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_missing_wrong_extra_and_transferred_archived_slots' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_missing_duplicate_and_wrong_active_reblit_reservations' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_empty_and_foreign_current_state_wrappers' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_empty_transition_wrapper_for_archived_and_active_reblit' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states' "$$tests/topology_refusal.rs"; \
	if timeout 10s rg -n 'dispatcher|dispatch_' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|rename\(|exchange_forward|exchange_reverse|sync_all|sync_data|\.sync\(|\.advance\(|forward_successor|rollback_successor|unlinkat|linkat|create_dir|remove_dir|remove_file|set_permissions|write_all|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'std::fs::File|fs::File|AsRawFd|RawFd|BorrowedFd|OwnedFd|root_directory\(|retained_staging_parent|PendingSystemTransition|ActivationNamespaceEvidence' "$$authority" "$$proof"; then exit 1; fi; \
	for file in "$$authority" "$$proof" "$$tests.rs" "$$tests/support.rs" "$$tests/admission.rs" "$$tests/evidence.rs" "$$tests/topology_refusal.rs"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::' \
		-- --test-threads=1
