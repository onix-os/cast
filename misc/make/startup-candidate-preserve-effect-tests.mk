.PHONY: forge-startup-usr-rollback-candidate-preserve-effect-test

forge-startup-usr-rollback-candidate-preserve-effect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 14; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::target_durability::startup_new_state_candidate_preserve_target_durability_orders_barriers_for_every_origin_and_outcome \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::target_durability::startup_new_state_candidate_preserve_target_durability_faults_stop_at_exact_prefixes_before_move \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::target_durability::startup_new_state_candidate_preserve_target_durability_namespace_races_fail_at_exact_prefixes \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::target_durability::startup_new_state_candidate_preserve_fresh_move_lease_repeats_target_durability_after_failure \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_candidate_preserve_effect_selects_disjoint_new_state_prefix_leases \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_reconciles_every_raw_result_for_every_origin \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_ambiguity_consumes_all_retry_capability \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_final_prefix_race_prevents_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_final_target_mode_race_prevents_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_effect_selection_starts_with_the_open_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_consumption_starts_with_the_open_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_rechecks_database_and_journal_after_namespace_use \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_pre_candidate_sync_evidence_races_prevent_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::startup_new_state_candidate_preserve_move_candidate_presync_race_prevents_the_attempt; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	effect_evidence=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_evidence.rs; \
	authority_effect=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/effect_reconciliation.rs; \
	model=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/model.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve.rs; \
	target_durability=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/target_durability.rs; \
	namespace_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/effect.rs; \
	namespace_reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/effect/reconciliation.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/tests/mod.rs; \
	target_durability_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/tests/target_durability.rs; \
	timeout 10s grep -Fqx 'mod effect_reconciliation;' "$$authority"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$authority_effect"; \
	timeout 10s grep -Fqx 'mod tests;' "$$authority_effect"; \
	timeout 10s grep -Fqx 'mod new_state_candidate_preserve;' crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	timeout 10s grep -Fqx 'mod effect;' "$$namespace"; \
	timeout 10s grep -Fqx 'mod target_durability;' "$$namespace"; \
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
	timeout 10s grep -Fq '        require_effect_binding(&self.effect.journal_binding, journal)?;' "$$authority_effect"; \
	timeout 10s grep -Fq '    if journal.has_binding(expected) {' "$$effect_evidence"; \
	timeout 10s grep -Fq 'let trailing_evidence = require_post_effect_evidence' "$$authority_effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        require_pre_effect_evidence(' "$$authority_effect" )" = 2; \
	prepared_line="$$( timeout 10s grep -nF '        let prepared_namespace = namespace.prepare_move(&installation, &record);' "$$authority_effect" | timeout 10s cut -d: -f1 )"; \
	second_binding_line="$$( timeout 10s grep -nF '        require_effect_binding(&journal_binding, journal)?;' "$$authority_effect" | timeout 10s cut -d: -f1 )"; \
	second_evidence_line="$$( timeout 10s grep -nF '        require_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;' "$$authority_effect" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	attempt_call_line="$$( timeout 10s grep -nF '        let namespace_result = prepared_namespace.reconcile_move(&installation, &record);' "$$authority_effect" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test -n "$$prepared_line"; \
	timeout 10s test -n "$$second_binding_line"; \
	timeout 10s test -n "$$second_evidence_line"; \
	timeout 10s test -n "$$attempt_call_line"; \
	timeout 10s test "$$prepared_line" -lt "$$second_binding_line"; \
	timeout 10s test "$$second_binding_line" -lt "$$second_evidence_line"; \
	timeout 10s test "$$second_evidence_line" -lt "$$attempt_call_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_noreplace_once(' "$$target_durability" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_noreplace_once(' "$$namespace_effect" )" = 0; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.sync_retained_candidate_for_move()?' "$$proof_effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.complete_target_durability(installation, record, baseline, projection)?' "$$proof_effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'durable_pre.attempt_move_once(installation, record)?' "$$proof_effect" )" = 1; \
	pre_line="$$( timeout 10s grep -nF 'let durable_pre = parents.complete_target_durability(installation, record, baseline, projection)?;' "$$proof_effect" | timeout 10s cut -d: -f1 )"; \
	attempt_line="$$( timeout 10s grep -nF 'let pending = durable_pre.attempt_move_once(installation, record)?;' "$$proof_effect" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$pre_line" -lt "$$attempt_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'sync_all()' "$$target_durability" )" = 2; \
	timeout 10s awk '/NewStateCandidatePreserveTargetDurabilityError::TargetSync [{]/ { target = NR } /NewStateCandidatePreserveTargetDurabilityError::QuarantineParentSync [{]/ { parent = NR } END { exit !(target && parent && target < parent) }' "$$target_durability"; \
	timeout 10s awk '/^impl RetainedNewStateCandidatePreserveParents [{]/ { raw = 1; next } /^impl TargetDurableNewStateCandidatePreservePre [{]/ { raw = 0; durable = 1; next } raw && /fn attempt_move_once/ { raw_attempt = 1 } durable && /fn attempt_move_once/ { durable_attempt = 1 } END { exit !(!raw_attempt && durable_attempt) }' "$$target_durability"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'attempt_raw_move_once(&parents.staging, &parents.target)' "$$target_durability" )" = 1; \
	timeout 10s grep -Fq 'fn attempt_raw_move_once(staging: &File, target: &File) -> io::Result<()> {' "$$target_durability"; \
	timeout 10s test "$$( timeout 10s rg -n 'attempt_raw_move_once' crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve --glob '*.rs' | timeout 10s wc -l )" = 2; \
	if timeout 10s rg -n 'pub.*fn[[:space:]]+attempt_raw_move_once' "$$target_durability"; then exit 1; fi; \
	if timeout 10s rg -n 'pub.*fn[[:space:]]+attempt_raw_move_once|fn[[:space:]]+attempt_move_once' "$$namespace_effect"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat2_noreplace\(' "$$namespace_effect" "$$target_durability"; then exit 1; fi; \
	if timeout 10s rg -n 'syscall[[:space:]]*\(|unsafe[[:space:]]*\{' "$$namespace_effect" "$$target_durability"; then exit 1; fi; \
	production_effect_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$effect_evidence" "$$authority_effect" "$$proof_effect" "$$namespace_effect" "$$namespace_reconciliation" "$$target_durability" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_effect_code"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'retry|persistence|persist_|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|rollback_successor|forward_successor|\.advance[[:space:]]*\(|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|insert_fresh_metadata|delete_metadata|\.execute\(|\.transaction\(|\.delete\(' <<<"$$( timeout 10s sed -E 's,//.*$$,,' "$$target_durability" )"; then exit 1; fi; \
	timeout 10s grep -Fq 'parents.sync_retained_candidate_for_move()?' "$$proof_effect"; \
	timeout 10s grep -Fq 'self.candidate.sync_retained_tree()' "$$namespace"; \
	timeout 10s grep -Fq 'self.witness.mode & 0o7777 == 0o700' "$$model"; \
	timeout 10s grep -Fq 'if !target.has_exact_private_permissions() {' "$$namespace"; \
	if timeout 10s rg -n 'sync_all|sync_data|RollbackActionOutcome|rollback_successor|forward_successor|\.advance[[:space:]]*\(|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived' "$$effect_evidence" "$$authority_effect" "$$proof_effect" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|path|quarantine_name|topology|wrapper_index)|AsRawFd|IntoRawFd|FromRawFd|BorrowedFd|OwnedFd' "$$effect_evidence" "$$authority_effect" "$$namespace" "$$namespace_effect" "$$namespace_reconciliation"; then exit 1; fi; \
	finish_body="$$( timeout 10s sed -n '/impl<'\''reservation> UsrRollbackCandidatePreserveFinishAuthority/,/^}/p' "$$authority" )"; \
	if timeout 10s rg -n 'into_effect|reconcile|MoveNewState' <<<"$$finish_body"; then exit 1; fi; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::ErrorAfterApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::ErrorWithoutApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::SuccessWithoutApply' "$$tests"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'new_state_candidate_preserve_move_attempt_count()' "$$tests" )" = 26; \
	timeout 10s test "$$( timeout 10s grep -Fc '#[test]' "$$target_durability_tests" )" = 4; \
	timeout 10s grep -Fq 'reset_new_state_candidate_preserve_target_durability_events' "$$target_durability_tests"; \
	timeout 10s grep -Fq 'take_new_state_candidate_preserve_target_durability_events' "$$target_durability_tests"; \
	for file in "$$authority" "$$effect_evidence" "$$authority_effect" "$$proof" "$$proof_effect" "$$namespace" "$$target_durability" "$$namespace_effect" "$$namespace_reconciliation" "$$tests" "$$target_durability_tests" crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs misc/make/startup-candidate-preserve-effect-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::effect_reconciliation::tests::' \
		-- --test-threads=1
