.PHONY: forge-startup-usr-rollback-candidate-preserve-post-move-durability-test

forge-startup-usr-rollback-candidate-preserve-post-move-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 6; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_orders_exact_events_for_applied_and_finish_matrices \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_faults_stop_at_exact_prefixes_and_fresh_admission_repeats \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_rejects_exact_post_races_at_every_barrier \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_rejects_evidence_races_and_fresh_admission_reruns \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_converges_applied_error_after_apply_and_finish_origins \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_non_new_state_finish_durability_is_fieldless_unsupported_without_events; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_effect=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation.rs; \
	authority_post=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/post_move_durability.rs; \
	proof_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/effect_reconciliation.rs; \
	proof_post=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/effect_reconciliation/post_move_durability.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve.rs; \
	namespace_post=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve/post_move_durability.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/post_move_durability.rs; \
	timeout 10s grep -Fqx 'mod post_move_durability;' "$$authority_effect"; \
	timeout 10s grep -Fqx 'mod post_move_durability;' "$$proof_effect"; \
	timeout 10s grep -Fqx 'mod post_move_durability;' "$$namespace"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveDurabilitySeal {' "$$startup_recovery"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveDurabilitySeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$startup_recovery"; \
	timeout 10s awk '$$0 == "impl UsrRollbackCandidatePreserveDurabilitySeal {" { state = 1; next } state == 1 && $$0 == "    #[cfg(test)]" { gated = 1; next } state == 1 && gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { test_only = 1; gated = 0; next } state == 1 && gated { exit 1 } state == 1 && $$0 ~ /^    .*fn new/ { exit 1 } state == 1 && $$0 == "}" { found = test_only; exit !found } END { exit !found }' "$$startup_recovery"; \
	production_seal_calls="$$( timeout 10s rg -n 'UsrRollbackCandidatePreserveDurabilitySeal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_seal_calls" = 0; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.candidate.sync_retained_tree()' "$$namespace_post" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.sync_all()' "$$namespace_post" )" = 3; \
	candidate_line="$$( timeout 10s grep -nF '.sync_retained_tree()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	staging_line="$$( timeout 10s grep -nF '        parents.staging.sync_all().map_err(|source| {' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	target_line="$$( timeout 10s grep -nF '        parents.target.sync_all().map_err(|source| {' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	quarantine_line="$$( timeout 10s grep -nF '        parents.quarantine.sync_all().map_err(|source| {' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	final_line="$$( timeout 10s grep -nF '        let final_post = capture_snapshot(installation, record)?;' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	final_event_line="$$( timeout 10s grep -nF '        record_final_post_proven();' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test -n "$$candidate_line"; \
	timeout 10s test "$$candidate_line" -lt "$$staging_line"; \
	timeout 10s test "$$staging_line" -lt "$$target_line"; \
	timeout 10s test "$$target_line" -lt "$$quarantine_line"; \
	timeout 10s test "$$quarantine_line" -lt "$$final_line"; \
	timeout 10s test "$$final_line" -lt "$$final_event_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.complete(installation, record)?;' "$$proof_post" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'\''reservation>' "$$authority_post" )" -ge 3; \
	timeout 10s grep -Fqx '    NewState(UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'\''reservation>),' "$$authority_post"; \
	timeout 10s grep -Fqx '    Unsupported,' "$$authority_post"; \
	timeout 10s grep -Fq '            UsrRollbackCandidatePreserveTopology::NewStatePreserved => {' "$$authority_post"; \
	timeout 10s awk '/UsrRollbackCandidatePreserveTopology::ArchivedPreserved/ { archived = NR } /UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved/ { active = NR } /Ok\(UsrRollbackCandidatePreserveFinishDurabilitySelection::Unsupported\)/ { unsupported = NR } END { exit !(archived && active && unsupported && archived < active && active < unsupported) }' "$$authority_post"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: RollbackActionOutcome::Applied,' "$$authority_post" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: RollbackActionOutcome::AlreadySatisfied,' "$$authority_post" )" = 1; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$authority_post"; then exit 1; fi; \
	production_post_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$authority_post" "$$proof_post" "$$namespace_post" )"; \
	if timeout 10s rg -n 'renameat|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|attempt_move|reconcile_move|move_attempt|mkdir|create_dir|set_permissions|chmod|unlink|remove_dir|remove_file' <<<"$$production_post_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' <<<"$$production_post_code"; then exit 1; fi; \
	if timeout 10s rg -n '\.advance[[:space:]]*\(|rollback_successor|forward_successor|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|insert_fresh_metadata|delete_metadata|\.execute\(|\.transaction\(|\.delete\(|cleanup|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived' <<<"$$production_post_code"; then exit 1; fi; \
	production_completion_calls="$$( timeout 10s rg -n '\.complete_post_move_durability\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/effect_reconciliation/post_move_durability.rs' | timeout 10s wc -l )"; \
	timeout 10s test "$$production_completion_calls" = 0; \
	timeout 10s test "$$( timeout 10s grep -Fc '#[test]' "$$tests" )" = 6; \
	timeout 10s grep -Fq 'NewStateCandidatePreserveMoveFault::ErrorAfterApply' "$$tests"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::Unsupported' "$$tests"; \
	for file in "$$authority" "$$authority_effect" "$$authority_post" "$$proof_effect" "$$proof_post" "$$namespace" "$$namespace_post" "$$startup_recovery" "$$tests" misc/make/startup-candidate-preserve-post-move-durability-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::' \
		-- --test-threads=1
