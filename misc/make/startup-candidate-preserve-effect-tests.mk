.PHONY: forge-startup-usr-rollback-candidate-preserve-effect-test

forge-startup-usr-rollback-candidate-preserve-effect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 8; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_candidate_preserve_effect_selects_only_new_state_empty_quarantine_prefix \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_reconciles_every_raw_result_for_every_origin \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_ambiguity_consumes_all_retry_capability \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_final_prefix_race_prevents_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_effect_selection_starts_with_the_open_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_consumption_starts_with_the_open_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_rechecks_database_and_journal_after_namespace_use \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_candidate_presync_race_prevents_the_attempt; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_effect=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/effect_reconciliation.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve.rs; \
	namespace_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/effect.rs; \
	namespace_reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/effect/reconciliation.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/tests/mod.rs; \
	timeout 10s grep -Fqx 'mod effect_reconciliation;' "$$authority"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$authority_effect"; \
	timeout 10s grep -Fqx 'mod tests;' "$$authority_effect"; \
	timeout 10s grep -Fqx 'mod new_state_candidate_preserve;' crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	timeout 10s grep -Fqx 'mod effect;' "$$namespace"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {' "$$startup_recovery"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$startup_recovery"; \
	timeout 10s awk '$$0 == "impl UsrRollbackCandidatePreserveEffectSeal {" { state = 1; next } state == 1 && $$0 == "    #[cfg(test)]" { gated = 1; next } state == 1 && gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { test_only = 1; gated = 0; next } state == 1 && gated { exit 1 } state == 1 && $$0 ~ /^    .*fn new/ { exit 1 } state == 1 && $$0 == "}" { found = test_only; exit !found } END { exit !found }' "$$startup_recovery"; \
	production_seal_calls="$$( timeout 10s rg -n 'UsrRollbackCandidatePreserveEffectSeal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_seal_calls" = 0; \
	production_selection_calls="$$( timeout 10s rg -n '\.into_effect_selection\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_selection_calls" = 0; \
	timeout 10s grep -Fqx '    MoveNewState(UsrRollbackNewStateCandidatePreserveEffectLease<'\''reservation>),' "$$authority"; \
	timeout 10s grep -Fqx '    Unsupported,' "$$authority"; \
	timeout 10s grep -Fqx '    Applied(UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'\''reservation>),' "$$authority_effect"; \
	timeout 10s grep -Fqx '    NotApplied,' "$$authority_effect"; \
	timeout 10s grep -Fqx '    Ambiguous,' "$$authority_effect"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.require_journal_binding(journal)?;' "$$authority" )" -ge 2; \
	timeout 10s grep -Fq 'if !journal.has_binding(&self.effect.journal_binding) {' "$$authority_effect"; \
	timeout 10s grep -Fq 'let trailing_evidence = require_post_effect_evidence' "$$authority_effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_noreplace_once(' "$$namespace_effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.sync_retained_candidate_for_move()?' "$$proof_effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.attempt_move_once()' "$$proof_effect" )" = 1; \
	pre_line="$$( timeout 10s grep -nF '} = self.final_exact_pre(installation, record)?;' "$$proof_effect" | timeout 10s cut -d: -f1 )"; \
	attempt_line="$$( timeout 10s grep -nF 'let pending = parents.attempt_move_once();' "$$proof_effect" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$pre_line" -lt "$$attempt_line"; \
	if timeout 10s rg -n 'renameat2_noreplace\(' "$$namespace_effect"; then exit 1; fi; \
	if timeout 10s rg -n 'syscall[[:space:]]*\(|unsafe[[:space:]]*\{' "$$namespace_effect"; then exit 1; fi; \
	production_effect_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$authority_effect" "$$proof_effect" "$$namespace_effect" "$$namespace_reconciliation" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_effect_code"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	timeout 10s grep -Fq 'parents.sync_retained_candidate_for_move()?' "$$proof_effect"; \
	timeout 10s grep -Fq 'self.candidate.sync_retained_tree()' "$$namespace"; \
	if timeout 10s rg -n 'sync_all|sync_data|complete_.*durability|RollbackActionOutcome|rollback_successor|forward_successor|\.advance[[:space:]]*\(|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived' "$$authority_effect" "$$proof_effect" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|path|quarantine_name|topology|wrapper_index)|AsRawFd|IntoRawFd|FromRawFd|BorrowedFd|OwnedFd' "$$authority_effect" "$$namespace" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	finish_body="$$( timeout 10s sed -n '/impl<'\''reservation> UsrRollbackCandidatePreserveFinishAuthority/,/^}/p' "$$authority" )"; \
	if timeout 10s rg -n 'into_effect|reconcile|MoveNewState' <<<"$$finish_body"; then exit 1; fi; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::ErrorAfterApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::ErrorWithoutApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::SuccessWithoutApply' "$$tests"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'new_state_candidate_preserve_move_attempt_count()' "$$tests" )" = 18; \
	for file in "$$authority" "$$authority_effect" "$$proof" "$$proof_effect" "$$namespace" "$$namespace_effect" "$$namespace_reconciliation" "$$tests" crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs misc/make/startup-candidate-preserve-effect-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::' \
		-- --test-threads=1
