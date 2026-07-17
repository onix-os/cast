.PHONY: forge-startup-usr-exchange-parent-durability-test

forge-startup-usr-exchange-parent-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_exchange_parent_durability::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 10; \
	for test in \
		client::startup_recovery::usr_exchange_parent_durability::tests::matrix::startup_usr_exchange_parent_durability_intent_post_matrix_persists_exact_pending_reverse_plan \
		client::startup_recovery::usr_exchange_parent_durability::tests::matrix::startup_usr_exchange_parent_durability_bypasses_non_intent_post_sources \
		client::startup_recovery::usr_exchange_parent_durability::tests::matrix::startup_usr_exchange_parent_durability_changes_only_parent_durability_and_canonical_journal \
		client::startup_recovery::usr_exchange_parent_durability::tests::durability_faults::startup_usr_exchange_parent_durability_syncs_each_parent_once_in_exact_order \
		client::startup_recovery::usr_exchange_parent_durability::tests::durability_faults::startup_usr_exchange_parent_durability_staging_sync_failure_retains_exact_intent_post \
		client::startup_recovery::usr_exchange_parent_durability::tests::durability_faults::startup_usr_exchange_parent_durability_root_sync_failure_retains_exact_intent_post \
		client::startup_recovery::usr_exchange_parent_durability::tests::evidence_races::startup_usr_exchange_parent_durability_final_revalidation_races_never_advance \
		client::startup_recovery::usr_exchange_parent_durability::tests::durability_faults::startup_usr_exchange_parent_durability_retry_is_idempotent_and_never_reexchanges \
		client::startup_recovery::usr_exchange_parent_durability::tests::evidence_races::startup_usr_exchange_parent_durability_binding_database_and_namespace_conflicts_never_advance \
		client::startup_recovery::usr_exchange_parent_durability::tests::evidence_races::startup_usr_exchange_parent_durability_historical_epoch_and_active_reblit_evidence_are_exact \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	normalizer=crates/forge/src/client/startup_recovery/usr_exchange_parent_durability.rs; \
	parent=crates/forge/src/client/startup_reconciliation/activation_namespace/parent_durability.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_decision_authority.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	coordinator_test=crates/forge/src/transition_identity/journal_coordinator/tests/usr_exchange_effect.rs; \
	shared_support=crates/forge/src/client/startup_recovery/test_support.rs; \
	timeout 10s grep -Fq 'arm_retained_exchange_fault(point);' "$$coordinator_test"; \
	timeout 10s grep -Fq 'let failure = intent.execute_usr_exchange(authority).unwrap_err();' "$$coordinator_test"; \
	timeout 10s grep -Fq 'assert_usr_exchange_intent_post_recovers_to_pending_reverse(' "$$coordinator_test"; \
	exchange_count_assertions="$$( timeout 10s rg -n 'assert_eq!\(retained_exchange_syscall_count\(\), 1,' "$$coordinator_test" | timeout 10s wc -l )"; \
	timeout 10s test "$$exchange_count_assertions" = 5; \
	if timeout 10s rg -n 'forward_fault_residue|ForwardFaultPrefix' "$$shared_support"; then exit 1; fi; \
	sync_count="$$( timeout 10s rg -n '\.sync_all\(\)' "$$normalizer" "$$parent" | timeout 10s wc -l )"; \
	timeout 10s test "$$sync_count" = 2; \
	timeout 10s grep -Fq '        staging.sync_all().map_err(|source|' "$$parent"; \
	timeout 10s grep -Fq '    installation.root_directory().sync_all().map_err(|source|' "$$normalizer"; \
	if timeout 10s rg -n 'sync_data|File::open|open_beneath|openat2|openat\(' "$$normalizer" "$$parent"; then exit 1; fi; \
	timeout 10s awk '$$0 ~ /authority\.sync_retained_staging_parent/ { state = 1; next } state == 1 && $$0 ~ /installation\.root_directory\(\)\.sync_all/ { state = 2; next } state == 2 && $$0 == "    authority.revalidate(journal)?;" { found = 1 } END { exit !found }' "$$normalizer"; \
	revalidation_count="$$( timeout 10s grep -Fxc '    authority.revalidate(journal)?;' "$$normalizer" )"; \
	timeout 10s test "$$revalidation_count" = 2; \
	timeout 10s awk '$$0 ~ /^pub\(in crate::client\) fn normalize_usr_exchange_parent_durability/ { active = 1; next } active && $$0 == "    journal: &TransitionJournalStore," { journal = 1; next } active && $$0 ~ /^    authority: UsrExchangeParentDurabilityAuthority/ { authority = 1; next } active && $$0 ~ /^\) -> Result<UsrRollbackDecisionAuthority/ { result = 1; exit } END { exit !(journal && authority && result) }' "$$normalizer"; \
	if timeout 10s rg -n 'journal: TransitionJournalStore|&UsrExchangeParentDurabilityAuthority' "$$normalizer"; then exit 1; fi; \
	timeout 10s grep -Fqx '        self,' "$$authority"; \
	timeout 10s grep -Fqx '        _seal: UsrExchangeParentDurabilityCompletionSeal,' "$$authority"; \
	seal_new_count="$$( timeout 10s rg -n 'UsrExchangeParentDurabilityCompletionSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_new_count" = 1; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrExchangeParentDurabilityCompletionSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$normalizer"; \
	parent_admission_count="$$( timeout 10s rg -n 'UsrRollbackDecisionAdmission::ParentDurabilityRequired\(' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$parent_admission_count" = 1; \
	normalize_definition_count="$$( timeout 10s rg -n '^pub\(in crate::client\) fn normalize_usr_exchange_parent_durability' "$$normalizer" | timeout 10s wc -l )"; \
	timeout 10s test "$$normalize_definition_count" = 1; \
	normalize_call_count="$$( timeout 10s rg -n 'normalize_usr_exchange_parent_durability\(&journal, authority\)' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$normalize_call_count" = 1; \
	timeout 10s grep -Fq 'super::startup_recovery::normalize_usr_exchange_parent_durability(&journal, authority)?' "$$startup_gate"; \
	timeout 10s grep -Fqx '            observations: rollback_observations(operation, InitialRollbackAction::Pending),' "$$authority"; \
	if timeout 10s rg -n 'renameat|rename\(|exchange_forward|exchange_reverse|rollback_successor|forward_successor|journal\.advance|\.advance\(|unlinkat|linkat|create_dir|remove_dir|remove_file|set_permissions|write_all|run_transaction_triggers|run_system_triggers|root_links|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.execute\(|\.transaction\(' "$$normalizer" "$$parent" "$$authority"; then exit 1; fi; \
	if timeout 10s rg -n 'ForwardExchangeDurabilityUnproven|UsrExchanged|forward_successor' "$$normalizer" "$$parent"; then exit 1; fi; \
	timeout 10s awk '$$0 == "#[cfg(test)]" { gated = 1; next } gated && $$0 ~ /^#\[derive/ { next } gated && $$0 == "pub(crate) enum UsrExchangeParentDurabilityFaultPoint {" { faults = 1; gated = 0; next } gated && $$0 == "pub(crate) enum UsrExchangeParentDurabilityEvent {" { events = 1; gated = 0; next } { gated = 0 } END { exit !(faults && events) }' "$$normalizer"; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_exchange_parent_durability::tests::' \
		-- --test-threads=1; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored' \
		-- --exact --test-threads=1
