.PHONY: forge-startup-usr-rollback-reverse-persistence-test

forge-startup-usr-rollback-reverse-persistence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_reverse_persistence::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 10; \
	for name in \
		matrix::startup_usr_rollback_reverse_persistence_applied_matrix_persists_exact_usr_restored \
		matrix::startup_usr_rollback_reverse_persistence_already_satisfied_matrix_persists_exact_usr_restored \
		matrix::startup_usr_rollback_reverse_persistence_changes_only_the_canonical_journal \
		evidence_races::startup_usr_rollback_reverse_persistence_rejects_different_open_and_cross_root_journal_bindings \
		evidence_races::startup_usr_rollback_reverse_persistence_database_journal_and_namespace_changes_never_advance \
		evidence_races::startup_usr_rollback_reverse_persistence_final_revalidation_races_fail_before_advance \
		storage_reopen::startup_usr_rollback_reverse_persistence_storage_faults_reopen_to_exact_source_or_usr_restored \
		storage_reopen::startup_usr_rollback_reverse_persistence_consumes_old_journal_and_reopens_exact_success \
		restart::startup_usr_rollback_reverse_persistence_source_fault_restart_finishes_without_second_exchange \
		restart::startup_usr_rollback_reverse_persistence_usr_restored_fault_restart_skips_reverse_effect; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation/durability.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	dispatcher=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch.rs; \
	root=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/restart.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_reverse_persistence;' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_reverse_persistence;' "$$root" )" = 1; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_reverse_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackReverseDurableEffectAuthority<'"'"'_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackReversePersistenceError> \{' "$$executor"; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor\(' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.usr_restored_successor()' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '        self._effect.record.rollback_successor(Some(self.outcome))' "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	timeout 10s grep -Fqx '        Ok(successor) if successor.phase == Phase::UsrRestored => successor,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_reverse_and_reopen(' "$$executor" )" = 1; \
	callers="$$( timeout 10s rg -n 'persist_usr_rollback_reverse_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_reverse_persistence.rs' | timeout 10s wc -l )"; \
	timeout 10s test "$$callers" = 1; \
	timeout 10s grep -Fqx '    persist_usr_rollback_reverse_and_reopen(journal, durable).map_err(UsrRollbackReverseDispatchError::from)' "$$dispatcher"; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	successor_line="$$( timeout 10s grep -nF '    let successor = match authority.usr_restored_successor() {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_reverse_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	final_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &successor);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackReverseReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$successor_line"; \
	timeout 10s test "$$successor_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$final_revalidate_line"; \
	timeout 10s test "$$final_revalidate_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = journal.advance(&source_record, &successor);/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(authority);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(journal);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	if timeout 10s rg -n 'retained_mutable_cast_directory|open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackReverseRecord::Source,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackReverseRecord::UsrRestored,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	if timeout 10s rg -n 'exchange_retained_usr_once|attempt_usr_exchange_once|renameat2|RENAME_EXCHANGE|unlinkat|linkat|symlinkat|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|std::fs|fs::|File::open|OpenOptions|openat|unsafe[[:space:]]*\{' "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for kind in OperationKind::ALL {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -F -n 'for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' "$$storage" "$$restart" | timeout 10s wc -l )" = 3; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_reverse_durable_namespace_capture(namespace_change);' "$$races"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$restart" )" = 4; \
	for file in "$$executor" "$$authority" "$$reopen" "$$root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" misc/make/startup-rollback-reverse-persistence-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
