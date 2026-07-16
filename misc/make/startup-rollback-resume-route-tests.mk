.PHONY: forge-startup-usr-rollback-resume-route-test

forge-startup-usr-rollback-resume-route-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_rollback_resume_route::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_pending_matrix_persists_reverse_exchange_intent \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_satisfied_matrix_skips_reverse_exchange \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_routes_only_and_preserves_exact_plan \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_rejects_a_different_open_journal_binding \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_database_and_provenance_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_namespace_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_capture_and_final_revalidation_races_fail_before_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_historical_and_active_reblit_evidence_remain_exact \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_storage_faults_reopen_to_exact_source_or_successor \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_rejects_cross_root_authority_and_reopens_success \
		client::startup_recovery::usr_rollback_resume_route::tests::end_to_end::startup_usr_rollback_resume_route_decision_then_route_uses_one_persistence_boundary_per_entry \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_resume_route.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_resume_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/resume_route_proof.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	journal_store=crates/forge/src/transition_journal/store.rs; \
	coordinator_test=crates/forge/src/transition_identity/journal_coordinator/tests/usr_exchange_effect.rs; \
	forward_support=crates/forge/src/client/startup_recovery/forward_origin_test_support.rs; \
	successor_count="$$( timeout 10s rg -n '\.rollback_successor\(' "$$executor" "$$authority" "$$proof" | timeout 10s wc -l )"; \
	timeout 10s test "$$successor_count" = 1; \
	timeout 10s grep -Fqx '    let successor = match source_record.rollback_successor(None) {' "$$executor"; \
	advance_count="$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$proof" | timeout 10s wc -l )"; \
	timeout 10s test "$$advance_count" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	if timeout 10s rg -n 'forward_successor|RollbackActionOutcome|transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_all|sync_data|write_all|set_permissions|create_dir|remove_dir|remove_file|hard_link|symlink|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|exchange_forward|exchange_reverse|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor" "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'PendingSystemTransition|ActivationNamespaceEvidence' "$$executor" "$$authority" "$$proof"; then exit 1; fi; \
	timeout 10s awk '$$0 == "pub(in crate::client) fn persist_usr_rollback_resume_route_and_reopen(" { state = 1; next } state == 1 && $$0 == "    journal: TransitionJournalStore," { state = 2; next } state == 2 && $$0 ~ /authority: UsrRollbackResumeRouteAuthority/ { found = 1 } END { exit !found }' "$$executor"; \
	if timeout 10s rg -n 'journal: &[[:space:]]*TransitionJournalStore' "$$executor"; then exit 1; fi; \
	timeout 10s grep -Fq 'if actual == source_record' "$$executor"; \
	timeout 10s grep -Fq 'if actual == successor' "$$executor"; \
	seal_count="$$( timeout 10s rg -n '^pub\(in crate::client\) struct UsrRollbackResumeRouteSeal \{' "$$startup_gate" | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_count" = 1; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackResumeRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$startup_gate"; \
	seal_call_count="$$( timeout 10s rg -n 'UsrRollbackResumeRouteSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_call_count" = 1; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackResumeRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 1; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackResumeRouteSeal,' "$$authority"; \
	timeout 10s grep -Fqx '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n 'let journal_binding = journal\.binding\(\);' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'journal\.has_binding\(&self\.journal_binding\)' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s awk '$$0 ~ /^    pub\(in crate::client\) fn revalidate\(/ { active = 1; next } active && $$0 == "        if !journal.has_binding(&self.journal_binding) {" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /journal\.load/) { exit 1 } END { exit !found }' "$$authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct TransitionJournalBinding(Arc<()>);' "$$journal_store"; \
	timeout 10s grep -Fq 'if record.phase != Phase::RollbackDecided || !is_usr_exchange_rollback_source(record)' "$$authority"; \
	timeout 10s grep -Fq '(RollbackAction::Pending, UsrExchangeLayout::Post)' "$$authority"; \
	timeout 10s grep -Fq '(RollbackAction::AlreadySatisfied, UsrExchangeLayout::Pre)' "$$authority"; \
	timeout 10s grep -Fq 'super::startup_recovery::persist_usr_rollback_resume_route_and_reopen(journal, authority)?' "$$startup_gate"; \
	timeout 10s grep -Fq 'assert_usr_rollback_decision_routes_to_reverse_exchange_intent(' "$$coordinator_test"; \
	timeout 10s grep -Fq 'decision.rollback_successor(None).unwrap()' "$$coordinator_test"; \
	timeout 10s grep -Fq 'retained_exchange_syscall_count() == 1' "$$coordinator_test"; \
	timeout 10s grep -Fq 'assert_eq!(pending.phase(), Phase::ReverseExchangeIntent);' "$$forward_support"; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_rollback_resume_route::tests::' \
		-- --test-threads=1; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry' \
		-- --exact --test-threads=1
