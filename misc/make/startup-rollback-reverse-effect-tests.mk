.PHONY: forge-startup-usr-rollback-reverse-effect-test

forge-startup-usr-rollback-reverse-effect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 6; \
	for test in \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_apply_reconciles_every_raw_result_for_every_operation \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_apply_ambiguity_consumes_all_retry_capability \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_apply_final_post_race_prevents_the_attempt \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_finish_is_zero_call_for_every_operation \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_effect_consumption_starts_with_the_open_binding \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::startup_usr_rollback_reverse_apply_rechecks_database_after_namespace_use; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof/effect_reconciliation.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation/tests/mod.rs; \
	timeout 10s grep -Fqx 'mod effect_reconciliation;' crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority.rs; \
	timeout 10s grep -Fqx 'mod effect_reconciliation;' crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	timeout 10s grep -Fqx '    Applied(UsrRollbackReverseAppliedEffectAuthority<'\''reservation>),' "$$authority"; \
	timeout 10s grep -Fqx '    NotApplied,' "$$authority"; \
	timeout 10s grep -Fqx '    Ambiguous,' "$$authority"; \
	if timeout 10s rg -n 'RollbackActionOutcome|outcome:' "$$authority"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '        _effect_seal: &UsrRollbackReverseEffectSeal,' "$$authority" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '        self,' "$$authority" )" -ge 2; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(|journal\.load\(\)|journal\.advance\(' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'journal\.load\(\)' "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_journal_record_binding(' "$$authority" )" = 6; \
	timeout 10s test "$$( timeout 10s grep -Fc '&self.lease.journal_record_binding,' "$$authority" )" = 2; \
	timeout 10s awk 'pending { if ($$0 ~ /revalidate_mutable_namespace\(\)\?;/) pairs += 1; pending = 0 } /require_journal_record_binding\(/ { if (/\)\?;/) pending = 1; else in_call = 1; next } in_call && $$0 ~ /^[[:space:]]*\)\?;$$/ { in_call = 0; pending = 1; next } END { exit pairs != 4 }' "$$authority"; \
	timeout 10s grep -Fq 'let pending = parents.attempt_usr_exchange_once();' "$$namespace"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'attempt_usr_exchange_once()' "$$namespace" )" = 1; \
	finish_body="$$( timeout 10s sed -n '/fn reconcile_finish(/,/^    }/p' "$$namespace" )"; \
	if timeout 10s rg -n 'attempt_usr_exchange_once|exchange_retained_usr_once|renameat2|RENAME_EXCHANGE' <<<"$$finish_body"; then exit 1; fi; \
	timeout 10s grep -Fq 'self.final_exact_namespace(installation, record, UsrExchangeLayout::Post)?' "$$namespace"; \
	timeout 10s grep -Fq 'self.final_exact_namespace(installation, record, UsrExchangeLayout::Pre)?' "$$namespace"; \
	timeout 10s test "$$( timeout 10s rg -n 'exchange_retained_usr_once\(' crates/forge/src --glob '*.rs' | timeout 10s wc -l )" = 3; \
	if timeout 10s rg -n 'sync_all|sync_data|syncfs|fsync|fdatasync|sync_file_range|\.sync[[:space:]]*\(|\.advance[[:space:]]*\(|rollback_successor|forward_successor|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|clear_transition_if_matches|remove_transition_if_matches' "$$authority" "$$namespace"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|std::fs::File|fs::File|unsafe[[:space:]]*\{' "$$authority" "$$namespace"; then exit 1; fi; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$authority" "$$namespace" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]' <<<"$$production_code"; then exit 1; fi; \
	for file in "$$authority" "$$namespace" "$$tests" misc/make/startup-rollback-reverse-effect-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::tests::' \
		-- --test-threads=1
