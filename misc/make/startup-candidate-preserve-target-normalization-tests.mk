.PHONY: forge-startup-usr-rollback-candidate-preserve-target-normalization-test

forge-startup-usr-rollback-candidate-preserve-target-normalization-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::semantics::startup_new_state_target_normalization_reconciles_raw_reports_semantically_and_requires_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::semantics::startup_new_state_target_normalization_accepts_every_restrictive_mode_for_every_origin \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::semantics::startup_new_state_target_normalization_accepts_concurrent_same_inode_canonicalization_only_as_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::semantics::startup_new_state_target_normalization_stays_bound_to_the_retained_inode_after_public_replacement \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::semantics::startup_new_state_target_normalization_enforces_payload_acl_and_xattr_boundaries \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::races::startup_new_state_target_normalization_consumption_starts_with_the_open_journal_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::races::startup_new_state_target_normalization_final_pre_races_prevent_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::races::startup_new_state_target_normalization_post_attempt_ambiguity_consumes_all_retry_capability \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::races::startup_new_state_target_normalization_rechecks_database_and_journal_after_the_attempt; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_normalize=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/target_normalization.rs; \
	effect_evidence=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_evidence.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_normalize=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/target_normalization.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation.rs; \
	namespace_normalize=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation/normalize.rs; \
	namespace_reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation/normalize/reconciliation.rs; \
	linux_fs=crates/forge/src/linux_fs/descriptor_metadata.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/target_normalization; \
	timeout 10s grep -Fqx 'mod target_normalization;' "$$authority"; \
	timeout 10s grep -Fqx 'mod target_normalization;' "$$proof"; \
	timeout 10s grep -Fqx 'mod normalize;' "$$namespace"; \
	timeout 10s grep -Fqx 'mod reconciliation;' "$$namespace_normalize"; \
	timeout 10s grep -Fqx '    NormalizeNewStateTarget(UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'\''reservation>),' "$$authority"; \
	timeout 10s grep -Fqx '    RestartRequired,' "$$authority_normalize"; \
	timeout 10s grep -Fqx '    NotApplied,' "$$authority_normalize"; \
	timeout 10s grep -Fqx '    Ambiguous,' "$$authority_normalize"; \
	timeout 10s awk '$$0 == "pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation {" { active = 1; seen = 1; next } active && $$0 == "}" { closed = 1; active = 0; next } active && /[(][^)]|[{][^}]/ { payload = 1 } END { exit !(seen && closed && !payload) }' "$$authority_normalize"; \
	timeout 10s grep -Fq 'require_effect_binding(&self.effect.journal_binding, journal)?;' "$$authority_normalize"; \
	timeout 10s grep -Fq 'let prepared_namespace = namespace.prepare_target_normalization(&installation, &record);' "$$authority_normalize"; \
	timeout 10s grep -Fq 'let namespace_result = prepared_namespace.reconcile_target_normalization(&installation, &record);' "$$authority_normalize"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_new_state_target_normalize_final_pre_capture' "$$proof_normalize"; \
	timeout 10s grep -Fq 'arm_before_new_state_target_normalize_attempt' "$$namespace_normalize"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'chmod_path_descriptor_once(' "$$namespace_normalize" )" = 1; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::ErrorAfterApply' "$$namespace_normalize"; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::ErrorWithoutApply' "$$namespace_normalize"; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::SuccessWithoutApply' "$$namespace_normalize"; \
	timeout 10s grep -Fq 'pub(crate) fn chmod_path_descriptor_once(file: &std::fs::File, mode: u32) -> io::Result<()> {' "$$linux_fs"; \
	production_normalize="$$( timeout 10s sed -E 's,//.*$$,,' "$$effect_evidence" "$$authority_normalize" "$$proof_normalize" "$$namespace_normalize" "$$namespace_reconciliation" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_normalize"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_normalize" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'MoveNewState|reconcile_move|attempt_move|renameat|rename\(|mkdirat|mkdir\(' "$$effect_evidence" "$$authority_normalize" "$$proof_normalize" "$$namespace_normalize" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'sync_all|sync_data|complete_.*durability|RollbackActionOutcome|rollback_successor|forward_successor|\.advance[[:space:]]*\(|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|insert_fresh_metadata|delete_metadata|\.execute\(|\.transaction\(|\.delete\(' "$$effect_evidence" "$$authority_normalize" "$$proof_normalize" "$$namespace_normalize" "$$namespace_reconciliation"; then exit 1; fi; \
	production_seal_calls="$$( timeout 10s rg -n 'UsrRollbackCandidatePreserveEffectSeal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_seal_calls" = 0; \
	production_target_normalize_dispatch_calls="$$( timeout 10s rg -n 'NormalizeNewStateTarget|NormalizeTargetReconciliation|new_state_target_normalize|target_normalization' crates/forge/src/client/startup_recovery.rs crates/forge/src/client/startup_recovery --glob '*.rs' --glob '!**/tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_target_normalize_dispatch_calls" = 0; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::ErrorAfterApply' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::ErrorWithoutApply' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'NewStateTargetNormalizeFault::SuccessWithoutApply' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'arm_before_new_state_target_normalize_attempt' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'system.posix_acl_access' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'system.posix_acl_default' "$$tests/semantics.rs"; \
	timeout 10s grep -Fq 'user.cast.target-normalize-boundary' "$$tests/support.rs"; \
	timeout 10s test "$$( timeout 10s rg -c 'assert_effect_attempts\(&fixture, 1\)' "$$tests/semantics.rs" "$$tests/races.rs" | timeout 10s awk -F: '{ total += $$2 } END { print total }' )" = 7; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_effect_attempts(&fixture, 0);' "$$tests/races.rs" )" = 2; \
	for file in "$$authority" "$$authority_normalize" "$$effect_evidence" "$$proof" "$$proof_normalize" "$$namespace" "$$namespace_normalize" "$$namespace_reconciliation" "$$tests.rs" "$$tests/support.rs" "$$tests/semantics.rs" "$$tests/races.rs" crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests.rs misc/make/startup-candidate-preserve-target-normalization-tests.mk misc/make/startup-candidate-preserve-target-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_normalization::' \
		-- --test-threads=1
