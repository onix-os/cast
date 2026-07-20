.PHONY: forge-startup-usr-rollback-fresh-db-invalidation-persistence-test

forge-startup-usr-rollback-fresh-db-invalidation-persistence-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-list.XXXXXXXXXXXX" )"; \
	symbol_refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-symbols.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$symbol_refs" "$$production_code"' EXIT; \
	timeout 600s $(CARGO) test -p forge --lib -- --list | timeout 600s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_recovery::usr_rollback_fresh_db_invalidation_persistence::tests::'; \
	count="$$( timeout 10s awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	timeout 10s test "$$count" = 14; \
	for name in \
		fresh_reopen::startup_fresh_db_invalidation_source_durable_fresh_handle_reopen_uses_zero_removal_finish \
		fresh_reopen::startup_fresh_db_invalidation_successor_durable_fresh_handle_reopen_skips_invalidation \
		matrix::startup_fresh_db_invalidation_persistence_applied_matrix_persists_exact_owned_successor \
		matrix::startup_fresh_db_invalidation_persistence_changes_only_the_canonical_journal \
		matrix::startup_fresh_db_invalidation_persistence_finish_matrix_persists_exact_owned_successor \
		races::startup_fresh_db_invalidation_persistence_final_races_fail_before_advance \
		races::startup_fresh_db_invalidation_persistence_rejects_reopened_and_cross_root_journals \
		record_binding::startup_fresh_db_invalidation_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_fresh_db_invalidation_same_byte_successor_replacement_fails_reopened_binding \
		record_binding::startup_fresh_db_invalidation_same_byte_successor_replacement_fails_same_store_binding \
		root_abi_races::startup_root_links_fresh_db_invalidation_final_persistence_revalidation_rejects_all_root_abi_mutations \
		root_abi_races::startup_root_links_fresh_db_invalidation_initial_persistence_revalidation_rejects_all_root_abi_mutations \
		storage_reopen::startup_fresh_db_invalidation_persistence_consumes_old_store_and_reopens_exact_success \
		storage_reopen::startup_fresh_db_invalidation_persistence_faults_reopen_exact_intent_or_invalidated_record; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence.rs; \
	persistence_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/effect_reconciliation/persistence.rs; \
	journal_record_binding=crates/forge/src/transition_journal/store/record_binding.rs; \
	effect=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/effect_reconciliation.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/fresh_db_invalidation_proof.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_dispatch.rs; \
	complete_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_complete_route_authority.rs; \
	finalization_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	exact=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/storage_reopen.rs; \
	record_binding=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/record_binding.rs; \
	fresh_reopen=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/fresh_reopen.rs; \
	root_abi=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/root_abi_races.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_persistence;' "$$recovery_root"; \
	timeout 10s grep -Fqx 'mod persistence;' "$$effect"; \
	for module in fresh_reopen matrix races record_binding root_abi_races storage_reopen support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Ec '^mod [a-z_]+;' "$$tests" )" = 7; \
	timeout 10s test ! -e crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/restart.rs; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_fresh_db_invalidation_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackFreshDbInvalidationEffectAuthority<'\''_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackFreshDbInvalidationPersistenceError> \{' "$$executor"; \
	timeout 10s grep -Fqx '    UsrRollbackFreshDbInvalidationPersistenceError, persist_usr_rollback_fresh_db_invalidation_and_reopen,' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)' "$$production_dispatch" )" = 1; \
	timeout 10s rg -n -F 'persist_usr_rollback_fresh_db_invalidation_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_fresh_db_invalidation_persistence.rs' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$recovery_root:" "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" "$$symbol_refs" )" = 2; \
	timeout 10s grep -Fq 'journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	timeout 10s grep -Fq 'journal_record_binding: TransitionJournalRecordBinding,' "$$effect"; \
	timeout 10s grep -Fq 'journal_record_binding: &TransitionJournalRecordBinding,' "$$proof"; \
	timeout 10s grep -Fq 'pub(in crate::client) struct UsrRollbackFreshDbInvalidationPublishedRecord {' "$$persistence_authority"; \
	timeout 10s grep -Fq 'binding: TransitionJournalRecordBinding,' "$$persistence_authority"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn advance_fresh_db_invalidated_record_binding(' "$$persistence_authority"; \
	timeout 10s grep -Fq '.rollback_successor(Some(self.origin))' "$$persistence_authority"; \
	timeout 10s grep -Fq 'if successor.phase != Phase::FreshDbInvalidated {' "$$persistence_authority"; \
	timeout 10s grep -Fq 'journal.advance_record_binding(cast, self.journal_record_binding, &successor)' "$$persistence_authority"; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance_record_binding\(' "$$executor" "$$persistence_authority" | timeout 10s wc -l )" = 1; \
	if timeout 10s rg -n 'journal\.advance\(|fresh_db_invalidated_successor' "$$executor" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	timeout 10s grep -Fq 'authority.advance_fresh_db_invalidated_record_binding(&journal)' "$$executor"; \
	timeout 10s grep -Fq 'revalidate_published_fresh_db_invalidated_binding(' "$$executor"; \
	timeout 10s grep -Fq '.has_record_binding(cast, successor_binding, successor)' "$$executor"; \
	timeout 10s grep -Fq 'revalidate_reopened_fresh_db_invalidated_binding(' "$$executor"; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor"; \
	timeout 10s grep -Fq 'before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation();' "$$executor"; \
	timeout 10s grep -Fq 'after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen();' "$$executor"; \
	advance_line="$$( timeout 10s grep -nF 'authority.advance_fresh_db_invalidated_record_binding(&journal)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	same_store_line="$$( timeout 10s grep -nF 'revalidate_published_fresh_db_invalidated_binding(' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	reopened_binding_line="$$( timeout 10s grep -nF 'revalidate_reopened_fresh_db_invalidated_binding(' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$advance_line" -lt "$$same_store_line"; \
	timeout 10s test "$$same_store_line" -lt "$$drop_line"; \
	timeout 10s test "$$drop_line" -lt "$$reopen_line"; \
	timeout 10s test "$$reopen_line" -lt "$$reopened_binding_line"; \
	for variant in FreshDbInvalidationIntent FreshDbInvalidated; do \
		timeout 10s grep -Fqx "    $$variant," "$$executor"; \
	done; \
	for error in 'SuccessorRecordBinding {' 'SuccessorRecordBindingAndReopen {'; do \
		timeout 10s grep -Fq "$$error" "$$executor"; \
	done; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal\.binding\(|journal\.has_binding\(|journal\.load\(' "$$authority" "$$effect" "$$proof" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(TransitionJournalRecordBinding|UsrRollbackFreshDbInvalidation(?:Authority|ApplyAuthority|FinishAuthority|DatabaseEvidence|EffectAuthority|PublishedRecord))' "$$journal_record_binding" "$$authority" "$$effect" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl[[:space:]]+Clone[[:space:]]+for[[:space:]]+TransitionJournalRecordBinding' "$$journal_record_binding"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -w -F 'remove_exact_fresh_transition' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$symbol_refs" )" = "$$effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'remove_exact_fresh_transition(' "$$exact" )" = 1; \
	for source_matrix in "$$matrix" "$$storage" "$$record_binding" "$$fresh_reopen"; do \
		timeout 10s grep -Fq 'Source::THROUGH_FRESH_DB_INVALIDATED' "$$source_matrix"; \
	done; \
	if timeout 10s rg -n 'Source::ALL|CandidateSource::ALL' "$$matrix" "$$storage" "$$record_binding" "$$fresh_reopen" "$$root_abi"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(cases, 24, "{origin:?}");' "$$matrix" )" = 1; \
	timeout 10s grep -Fq 'assert_eq!(executions, 240);' "$$storage"; \
	timeout 10s grep -Fq 'assert_eq!(executions, 96);' "$$record_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(executions, 48);' "$$record_binding" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(executions, 48);' "$$fresh_reopen" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(executions, 240);' "$$root_abi" )" = 2; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fqx "            arm_next_$${fault}_fault," "$$storage"; \
	done; \
	for boundary in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding; do \
		timeout 10s grep -Fq "PublicBindingRevalidationBoundary::$$boundary" "$$record_binding"; \
	done; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation(hook);' "$$record_binding"; \
	timeout 10s grep -Fq 'arm_after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen(hook);' "$$record_binding"; \
	timeout 10s grep -Fq 'FreshInvalidationHandles::open(retained.path())' "$$fresh_reopen"; \
	if timeout 10s rg -ni 'process.kill|SIGKILL|fork\(|reboot|power.loss' "$$fresh_reopen"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for item in 'const ROOT_ABI: [(&str, &str); 5]' 'Missing,' 'WrongTarget,' 'SameTargetDifferentInode,' 'assert!(!displaced_directory.path().starts_with(&root));' 'assert_eq!(after[index].as_ref(), Some(original), "{label}");'; do \
		timeout 10s grep -Fq "$$item" "$$root_abi"; \
	done; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$persistence_authority" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'remove_exact_fresh_transition|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|preserve_failed|cleanup|finalize|diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$complete_authority"; \
	if timeout 10s rg -n 'RootLinksComplete' "$$complete_authority" "$$finalization_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$executor" "$$persistence_authority" "$$journal_record_binding" "$$effect" "$$authority" "$$proof" "$$recovery_root" "$$reconciliation_root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$record_binding" "$$fresh_reopen" "$$root_abi" misc/make/startup-fresh-db-invalidation-persistence-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
