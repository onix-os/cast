.PHONY: forge-startup-usr-rollback-reverse-admission-test

forge-startup-usr-rollback-reverse-admission-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_reverse_authority::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	for test in \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::admission::startup_usr_rollback_reverse_admission_splits_post_apply_from_pre_finish \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::admission::startup_usr_rollback_reverse_admission_accepts_historical_runtime_evidence \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::admission::startup_usr_rollback_reverse_admission_bypasses_usr_restored_and_other_phases \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::admission::startup_usr_rollback_reverse_plan_requires_exact_pending_usr_action \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::evidence::startup_usr_rollback_reverse_rejects_a_different_open_journal_binding \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::evidence::startup_usr_rollback_reverse_database_and_provenance_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::evidence::startup_usr_rollback_reverse_namespace_and_journal_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::evidence::startup_usr_rollback_reverse_capture_races_defer_without_authority \
		client::startup_reconciliation::usr_rollback_reverse_authority::tests::evidence::startup_usr_rollback_reverse_fresh_namespace_race_fails_revalidation; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackReverseSeal {' "$$startup_gate"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackReverseSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$startup_gate"; \
	timeout 10s awk '$$0 == "impl UsrRollbackReverseSeal {" { state = 1; next } state == 1 && $$0 == "    #[cfg(test)]" { gated = 1; next } state == 1 && gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { constructor = 1; next } state == 1 && $$0 ~ /fn new\(/ { exit 1 } state == 1 && $$0 == "}" { exit !constructor } END { exit !constructor }' "$$startup_gate"; \
	seal_call_count="$$( timeout 10s rg -n 'UsrRollbackReverseSeal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_call_count" = 0; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackReverseAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 0; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackReverseSeal,' "$$authority"; \
	timeout 10s grep -Fqx '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s grep -Fqx "    _active_state_reservation: &'reservation ActiveStateReservation," "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n 'let journal_binding = journal\.binding\(\);' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'journal\.has_binding\(&self\.journal_binding\)' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s awk '$$0 == "    fn revalidate(" { active = 1; next } active && $$0 == "        if !journal.has_binding(&self.journal_binding) {" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /inspect_current_database/ || $$0 ~ /self\.namespace/) { exit 1 } END { exit !found }' "$$authority"; \
	timeout 10s grep -Fq 'if record.phase != Phase::ReverseExchangeIntent {' "$$authority"; \
	timeout 10s grep -Fq '|| rollback.usr_exchange != RollbackAction::Pending' "$$authority"; \
	timeout 10s grep -Fq 'UsrExchangeLayout::Post =>' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackReverseAdmission::Apply(UsrRollbackReverseApplyAuthority' "$$authority"; \
	timeout 10s grep -Fq 'UsrExchangeLayout::Pre =>' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackReverseAdmission::Finish(UsrRollbackReverseFinishAuthority' "$$authority"; \
	timeout 10s grep -Fq 'self.evidence.revalidate(journal, UsrExchangeLayout::Post)' "$$authority"; \
	timeout 10s grep -Fq 'self.evidence.revalidate(journal, UsrExchangeLayout::Pre)' "$$authority"; \
	if timeout 10s rg -n 'renameat|rename\(|exchange_forward|exchange_reverse|exchange_live_and_staged|finish_applied_reverse|sync_all|sync_data|\.sync\(|\.advance\(|forward_successor|rollback_successor|unlinkat|linkat|create_dir|remove_dir|remove_file|set_permissions|write_all|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'std::fs::File|fs::File|AsRawFd|RawFd|BorrowedFd|OwnedFd|root_directory\(|retained_staging_parent|PendingSystemTransition|ActivationNamespaceEvidence' "$$authority" "$$proof"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s wc -l < "$$authority" )" -le 1000; \
	timeout 10s test "$$( timeout 10s wc -l < "$$proof" )" -le 1000; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_reverse_authority::tests::' \
		-- --test-threads=1
