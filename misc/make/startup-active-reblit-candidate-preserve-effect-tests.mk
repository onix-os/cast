.PHONY: forge-startup-usr-rollback-active-reblit-preserve-effect-test

forge-startup-usr-rollback-active-reblit-preserve-effect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::active_reblit_effect::'; \
	count="$$( timeout 10s grep -c "^$${prefix}.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	for test in \
		startup_active_reblit_whole_wrapper_exchange_preserves_the_original_wrapper_and_non_namespace_evidence \
		startup_active_reblit_whole_wrapper_exchange_classifies_both_unapplied_raw_reports_from_fresh_pre \
		startup_active_reblit_whole_wrapper_exchange_classifies_applied_error_from_fresh_post \
		startup_active_reblit_whole_wrapper_exchange_classifies_changed_post_evidence_as_ambiguous \
		startup_active_reblit_whole_wrapper_exchange_refuses_a_rebound_fixed_name_before_attempt \
		startup_active_reblit_whole_wrapper_exchange_refuses_a_rebound_staging_name_before_attempt \
		startup_active_reblit_whole_wrapper_exchange_refuses_post_lease_database_drift_before_attempt \
		startup_active_reblit_whole_wrapper_exchange_refuses_post_lease_journal_drift_before_attempt \
		startup_active_reblit_whole_wrapper_exchange_refuses_reservation_mode_drift_before_attempt \
		startup_active_reblit_whole_wrapper_exchange_preserves_a_nonzero_private_index \
		startup_active_reblit_finish_reconciles_exact_post_without_an_exchange \
		startup_active_reblit_production_selection_remains_fieldless_unsupported; do \
		timeout 10s grep -Fqx "$${prefix}$${test}: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_effect=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect.rs; \
	authority_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/active_reblit_effect.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/active_reblit_effect.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve.rs; \
	namespace_pre=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/pre_exchange_durability.rs; \
	namespace_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/effect.rs; \
	namespace_reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/effect/reconciliation.rs; \
	timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod active_reblit_effect;" { found = 1 } { previous = $$0 } END { exit !found }' "$$authority"; \
	timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod active_reblit_effect;" { found = 1 } { previous = $$0 } END { exit !found }' "$$proof"; \
	timeout 10s grep -Fqx '    Unsupported,' "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n -F 'renameat2_exchange_once(' "$$namespace" "$$namespace_pre" "$$namespace_effect" "$$namespace_reconciliation" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_exchange_once(&parents.roots' "$$namespace_effect" )" = 1; \
	if timeout 10s rg -n 'renameat2_noreplace|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|syscall[[:space:]]*\(|unsafe[[:space:]]*\{' "$$namespace" "$$namespace_pre" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|wrapper_index|target_name|raw_report)' "$$authority_effect" "$$proof_effect" "$$namespace" "$$namespace_pre" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	effect_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$authority_effect" "$$proof_effect" "$$namespace" "$$namespace_pre" "$$namespace_effect" "$$namespace_reconciliation" )"; \
	if timeout 10s rg -n 'retry|persist_|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|\.advance[[:space:]]*\(' <<<"$$effect_code"; then exit 1; fi; \
	timeout 10s grep -Fq '.checked_add(1)' "$$namespace_effect"; \
	timeout 10s grep -Fq 'ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply' "$$authority_tests"; \
	timeout 10s grep -Fq 'ActiveReblitCandidatePreserveExchangeFault::ErrorWithoutApply' "$$authority_tests"; \
	timeout 10s grep -Fq 'ActiveReblitCandidatePreserveExchangeFault::SuccessWithoutApply' "$$authority_tests"; \
	timeout 10s test "$$( timeout 10s grep -Fc '#[test]' "$$authority_tests" )" = 12; \
	for file in "$$authority" "$$authority_effect" "$$authority_tests" "$$proof" "$$proof_effect" "$$namespace" "$$namespace_pre" "$$namespace_effect" "$$namespace_reconciliation" misc/make/startup-active-reblit-candidate-preserve-effect-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::active_reblit_effect::' \
		-- --test-threads=1
