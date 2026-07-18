.PHONY: forge-startup-usr-rollback-active-reblit-candidate-preserve-persistence-test

forge-startup-usr-rollback-active-reblit-candidate-preserve-persistence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_active_reblit_candidate_preserve_persistence::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for name in \
		matrix::startup_active_reblit_candidate_preserve_persistence_applied_matrix_persists_exact_successor \
		matrix::startup_active_reblit_candidate_preserve_persistence_finish_matrix_persists_exact_successor \
		matrix::startup_active_reblit_candidate_preserve_persistence_changes_only_the_canonical_journal \
		evidence_races::startup_active_reblit_candidate_preserve_persistence_rejects_mixed_and_cross_root_journals \
		evidence_races::startup_active_reblit_candidate_preserve_persistence_final_races_fail_before_advance \
		storage_reopen::startup_active_reblit_candidate_preserve_persistence_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_active_reblit_candidate_preserve_persistence_consumes_old_store_and_reopens_exact_success \
		restart::startup_active_reblit_candidate_preserve_persistence_source_fault_restart_finishes_without_second_exchange \
		restart::startup_active_reblit_candidate_preserve_persistence_successor_fault_restart_skips_preservation \
		production_dispatch::startup_active_reblit_candidate_preserve_production_leaf_dispatches_applied_and_finish_exactly_once \
		production_dispatch::startup_active_reblit_candidate_preserve_production_leaf_source_fault_restarts_finish_without_second_exchange; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	authority_root=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_active=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect.rs; \
	authority_post=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect/post_exchange_durability.rs; \
	authority_persistence=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect/post_exchange_durability/persistence.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	capture=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve.rs; \
	namespace_post=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/post_exchange_durability.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/restart.rs; \
	production_tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/production_dispatch.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_candidate_preserve_persistence;' "$$recovery_root"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_active_reblit_candidate_preserve_persistence;" { found = 1 } { previous = $$0 } END { exit !found }' "$$recovery_root"; then exit 1; fi; \
	for pair in "$$authority_root:mod active_reblit_effect;" "$$proof:mod active_reblit_effect;" "$$capture:mod active_reblit_candidate_preserve;"; do \
		file="$${pair%%:*}"; declaration="$${pair#*:}"; \
		timeout 10s grep -Fqx "$$declaration" "$$file"; \
		if timeout 10s awk -v declaration="$$declaration" 'previous == "#[cfg(test)]" && $$0 == declaration { found = 1 } { previous = $$0 } END { exit !found }' "$$file"; then exit 1; fi; \
	done; \
	timeout 10s grep -Fqx 'mod persistence;' "$$authority_post"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_active_reblit_candidate_preserve_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'"'"'_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackActiveReblitCandidatePreservePersistenceError> \{' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.candidate_preserved_successor()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority_persistence" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	timeout 10s grep -Fqx '        Ok(successor) if successor.phase == Phase::CandidatePreserved => successor,' "$$executor"; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$executor" "$$authority_persistence"; then exit 1; fi; \
	timeout 10s grep -Fq 'self._effect.record.rollback_successor(Some(outcome))' "$$authority_persistence"; \
	first_revalidate="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	successor_line="$$( timeout 10s grep -nF '    let successor = match authority.candidate_preserved_successor() {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	final_revalidate="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &successor);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate" -lt "$$successor_line"; \
	timeout 10s test "$$successor_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$final_revalidate"; \
	timeout 10s test "$$final_revalidate" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority"; \
	timeout 10s test "$$drop_authority" -lt "$$drop_journal"; \
	timeout 10s test "$$drop_journal" -lt "$$reopen_line"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,' "$$executor"; \
	timeout 10s grep -Fq 'run_before_durable_post_revalidation_capture();' "$$namespace_post"; \
	timeout 10s grep -Fq 'run_before_persistence_durable_trailing_evidence();' "$$authority_persistence"; \
	persistence_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority_persistence" )"; \
	if timeout 10s rg -n 'renameat|exchange_once|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|mkdir|create_dir|set_permissions|chmod|unlink|remove_dir|remove_file|sync_all|sync_data' <<<"$$persistence_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' <<<"$$persistence_code"; then exit 1; fi; \
	if timeout 10s rg -n 'clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|run_transaction_triggers|run_system_triggers|cleanup|archive_previous|rearchive_archived|preserve_failed|\.execute\(|\.transaction\(|\.delete\(' <<<"$$persistence_code"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|wrapper_index|target_name)|AsRawFd|RawFd' "$$executor" "$$authority_persistence"; then exit 1; fi; \
	production_refs="$$( timeout 10s rg -n -F 'persist_usr_rollback_active_reblit_candidate_preserve_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_active_reblit_candidate_preserve_persistence.rs' || true )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_refs" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$recovery_root:" <<<"$$production_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" <<<"$$production_refs" )" = 2; \
	timeout 10s grep -Fq 'persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, durable)' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::ActiveReblit' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveDispatchError::ActiveReblitPersistence' "$$production_tests"; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$matrix" "$$races" "$$storage" "$$restart" "$$production_tests" | timeout 10s wc -l )" = 11; \
	for race in Database Provenance Journal Installation Namespace Plan; do timeout 10s grep -Fq "EvidenceRace::$$race" "$$races"; done; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$storage"; done; \
	timeout 10s grep -Fq 'expected_post_events(&fixture)' "$$restart"; \
	timeout 10s grep -Fq 'drop(reservation);' "$$restart"; \
	for file in "$$executor" "$$recovery_root" "$$production_dispatch" "$$authority_root" "$$authority_active" "$$authority_post" "$$authority_persistence" "$$proof" "$$capture" "$$namespace" "$$namespace_post" "$$reconciliation_root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" "$$production_tests" misc/make/startup-active-reblit-candidate-preserve-persistence-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
