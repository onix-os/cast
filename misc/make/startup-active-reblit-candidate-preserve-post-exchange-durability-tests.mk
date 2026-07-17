.PHONY: forge-startup-active-reblit-candidate-preserve-post-exchange-durability-test

forge-startup-active-reblit-candidate-preserve-post-exchange-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::active_reblit_post_exchange_durability::'; \
	count="$$( timeout 10s grep -c "^$${prefix}.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 6; \
	for test in \
		startup_active_reblit_post_exchange_durability_orders_identical_events_for_applied_and_finish \
		startup_active_reblit_post_exchange_durability_faults_consume_exact_prefixes_and_fresh_finish_reruns_without_exchange \
		startup_active_reblit_post_exchange_durability_rejects_namespace_public_name_inode_and_mode_races_at_every_boundary \
		startup_active_reblit_post_exchange_durability_rejects_database_and_journal_drift_with_authority_withheld \
		startup_active_reblit_post_exchange_durability_converges_success_error_after_apply_and_finish_independent_of_raw_status \
		startup_active_reblit_post_exchange_durability_production_selection_remains_unsupported_without_events_or_attempts; do \
		timeout 10s grep -Fqx "$${prefix}$${test}: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_active=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect.rs; \
	authority_post=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect/post_exchange_durability.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_active=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/active_reblit_effect.rs; \
	proof_post=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/active_reblit_effect/post_exchange_durability.rs; \
	capture=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve.rs; \
	namespace_post=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/post_exchange_durability.rs; \
	namespace_effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/effect.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/active_reblit_post_exchange_durability.rs; \
	timeout 10s grep -Fqx 'mod post_exchange_durability;' "$$authority_active"; \
	timeout 10s grep -Fqx 'mod post_exchange_durability;' "$$proof_active"; \
	timeout 10s grep -Fqx 'mod post_exchange_durability;' "$$namespace"; \
	for pair in "$$authority:mod active_reblit_effect;" "$$proof:mod active_reblit_effect;" "$$capture:mod active_reblit_candidate_preserve;"; do \
		file="$${pair%%:*}"; declaration="$${pair#*:}"; \
		timeout 10s awk -v declaration="$$declaration" 'previous == "#[cfg(test)]" && $$0 == declaration { found = 1 } { previous = $$0 } END { exit !found }' "$$file"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackActiveReblitCandidatePreserveDurabilitySeal \{' crates/forge/src/client --glob '*.rs' )" = "$$authority_post"; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn new_for_test() -> Self {' "$$authority_post"; \
	if timeout 10s grep -Fq '    fn new() -> Self {' "$$authority_post"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'parents.candidate.sync_retained_tree()' "$$namespace_post" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.sync_all()' "$$namespace_post" )" = 4; \
	candidate="$$( timeout 10s grep -nF 'parents.candidate.sync_retained_tree()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	candidate_wrapper="$$( timeout 10s grep -nF 'parents.candidate_wrapper.sync_all()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	reservation_wrapper="$$( timeout 10s grep -nF 'parents.reservation_wrapper.sync_all()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	roots="$$( timeout 10s grep -nF 'parents.roots.sync_all()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	quarantine="$$( timeout 10s grep -nF 'parents.quarantine.sync_all()' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	final="$$( timeout 10s grep -nF 'let final_post = capture_snapshot(installation, record)?;' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	final_event="$$( timeout 10s grep -nF 'record_final_post_proven();' "$$namespace_post" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$candidate" -lt "$$candidate_wrapper"; \
	timeout 10s test "$$candidate_wrapper" -lt "$$reservation_wrapper"; \
	timeout 10s test "$$reservation_wrapper" -lt "$$roots"; \
	timeout 10s test "$$roots" -lt "$$quarantine"; \
	timeout 10s test "$$quarantine" -lt "$$final"; \
	timeout 10s test "$$final" -lt "$$final_event"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fn complete_post_exchange_durability(' "$$proof_post" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fn complete_post_exchange_durability(' "$$authority_post" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: ActiveReblitDurabilityOrigin::Applied,' "$$authority_post" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: ActiveReblitDurabilityOrigin::AlreadySatisfied,' "$$authority_post" )" = 1; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$authority_post"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s rg -l -F 'renameat2_exchange_once(' crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve --glob '*.rs' )" = "$$namespace_effect"; \
	durability_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$authority_post" "$$proof_post" "$$namespace_post" )"; \
	if timeout 10s rg -n 'renameat|exchange_once|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|mkdir|create_dir|set_permissions|chmod|unlink|remove_dir|remove_file' <<<"$$durability_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' <<<"$$durability_code"; then exit 1; fi; \
	if timeout 10s rg -n '\.advance[[:space:]]*\(|rollback_successor|forward_successor|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|insert_fresh_metadata|delete_metadata|\.execute\(|\.transaction\(|\.delete\(|cleanup|archive_previous|rearchive_archived|preserve_failed' <<<"$$durability_code"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_post" "$$proof_post" "$$authority_post"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|wrapper_index|target_name)|AsRawFd|RawFd' "$$namespace_post" "$$proof_post" "$$authority_post"; then exit 1; fi; \
	production_calls="$$( timeout 10s rg -n -F '.complete_post_exchange_durability(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/active_reblit_effect.rs' --glob '!**/active_reblit_effect/**' || true )"; \
	timeout 10s test -z "$$production_calls"; \
	timeout 10s grep -Fqx '    Unsupported,' "$$authority"; \
	timeout 10s grep -Fq 'ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply' "$$tests"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::Unsupported' "$$tests"; \
	timeout 10s test "$$( timeout 10s grep -Fc '#[test]' "$$tests" )" = 6; \
	for file in "$$authority" "$$authority_active" "$$authority_post" "$$proof" "$$proof_active" "$$proof_post" "$$capture" "$$namespace" "$$namespace_post" "$$namespace_effect" "$$tests" misc/make/startup-active-reblit-candidate-preserve-post-exchange-durability-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$${prefix}" -- --test-threads=1
