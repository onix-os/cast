.PHONY: forge-startup-usr-rollback-reverse-persistence-test

forge-startup-usr-rollback-reverse-persistence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_reverse_persistence::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
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
		restart::startup_usr_rollback_reverse_persistence_usr_restored_fault_restart_skips_reverse_effect \
		root_links_bound_update::startup_root_links_reverse_all_bound_update_faults_reopen_exact_record_across_operations_and_epochs \
		root_links_bound_update::startup_root_links_reverse_same_byte_successor_replacement_after_publication_fails_exact_binding \
		root_links_bound_update::startup_root_links_reverse_same_byte_successor_replacement_after_binding_before_reopen_never_succeeds; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation/durability.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	dispatcher=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch.rs; \
	root=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/restart.rs; \
	root_links=crates/forge/src/client/startup_recovery/usr_rollback_reverse_persistence/tests/root_links_bound_update.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_reverse_persistence;' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_reverse_persistence;' "$$root" )" = 1; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_reverse_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackReverseDurableEffectAuthority<'"'"'_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackReversePersistenceError> \{' "$$executor"; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor\(' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	if timeout 10s rg -n 'usr_restored_successor\(' "$$executor" "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '.rollback_successor(Some(self.outcome))' "$$authority"; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(|journal\.advance\(|journal\.load\(' "$$executor" "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    let advance = match authority.advance_usr_restored_record_binding(&journal) {' "$$executor"; \
	if timeout 10s rg -n 'advance_usr_restored_record_binding\(&journal[[:space:]]*,' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor)' "$$authority"; \
	timeout 10s grep -Fqx '            let (successor, successor_binding) = published.into_parts();' "$$executor"; \
	publication_consumers="$$( timeout 10s rg -n 'published\.into_parts\(\)' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$publication_consumers" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_reverse_and_reopen(' "$$executor" )" = 1; \
	callers="$$( timeout 10s rg -n 'persist_usr_rollback_reverse_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_reverse_persistence.rs' | timeout 10s wc -l )"; \
	timeout 10s test "$$callers" = 1; \
	timeout 10s grep -Fqx '    persist_usr_rollback_reverse_and_reopen(journal, durable).map_err(UsrRollbackReverseDispatchError::from)' "$$dispatcher"; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_reverse_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_usr_restored_record_binding(&journal) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_seam_line="$$( timeout 10s grep -nF '        after_usr_rollback_reverse_successor_binding_check_before_reopen();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackReverseReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_seam_line"; \
	timeout 10s test "$$reopen_seam_line" -lt "$$reopen_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_usr_restored_record_binding/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	advance_match="$$( timeout 10s sed -n '/    let advance = match authority.advance_usr_restored_record_binding/,/^    };/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            return Err(UsrRollbackReversePersistenceError::' <<<"$$advance_match" )" = 4; \
	for terminal in Authority Installation Successor UnexpectedSuccessor; do timeout 10s grep -Fq "Err(UsrRollbackReverseRecordAdvanceError::$$terminal" <<<"$$advance_match"; done; \
	timeout 10s rg -U -q 'Err\(UsrRollbackReverseRecordAdvanceError::Storage \{ source, successor \}\) => \{\n            UsrRollbackReverseAdvanceOutcome::StorageFailed \{ successor, source \}\n        \}' <<<"$$advance_match"; \
	timeout 10s awk ' \
		function fail() { bad = 1; exit } \
		$$0 == "        Ok(published) => {" { if (active || seen) fail(); active = 1; seen = 1; next } \
		active && $$0 == "        Err(UsrRollbackReverseRecordAdvanceError::Authority(source)) => {" { active = 0; closed = 1; next } \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if (index($$0, "UsrRollbackReverseAdvanceOutcome::SuccessorBindingFailed {")) failures += 1; \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || failures < 1) exit 1 } \
	' "$$executor"; \
	timeout 10s awk ' \
		function fail() { bad = 1; exit } \
		function finish_branch() { \
			if (branch == 0) return; \
			if (errors != 1 || error_open || !error_closed || !branch_closed) fail(); \
			if (branch == 1 && (record_binding != 1 || durable_source != 1 || durable_successor != 0 || combined != 0)) fail(); \
			if (branch == 2 && (record_binding != 1 || durable_source != 0 || durable_successor != 1 || combined != 0)) fail(); \
			if ((branch == 3 || branch == 4) && (record_binding != 0 || durable_source != 0 || durable_successor != 0 || combined != 1)) fail(); \
		} \
		function reset_branch() { \
			errors = record_binding = durable_source = durable_successor = combined = 0; \
			error_open = error_closed = branch_closed = 0; \
		} \
		$$0 == "        UsrRollbackReverseAdvanceOutcome::SuccessorBindingFailed {" { \
			if (active || seen != 0) fail(); active = 1; header = 1; seen = 1; next; \
		} \
		active && header && $$0 == "        } => match reopened {" { header = 0; next; } \
		active && header { next; } \
		active && $$0 == "        }," { finish_branch(); active = 0; closed = 1; next; } \
		active && $$0 ~ /^            .* => / { \
			finish_branch(); branch += 1; reset_branch(); \
			if (branch == 1 && $$0 != "            Ok((reopened, Some(actual))) if actual == source_record => {") fail(); \
			if (branch == 2 && $$0 != "            Ok((reopened, Some(actual))) if actual == successor => {") fail(); \
			if (branch == 3 && $$0 != "            Ok((reopened, actual)) => {") fail(); \
			if (branch == 4) { \
				if ($$0 != "            Err(reopen) => Err(UsrRollbackReversePersistenceError::SuccessorRecordBindingAndReopen {") fail(); \
				errors = 1; combined = 1; error_open = 1; \
			} \
			if (branch > 4) fail(); next; \
		} \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") != 0 || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if ($$0 ~ /^[[:space:]]*Ok\(/) fail(); \
			if (branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
			if ($$0 == "                Err(UsrRollbackReversePersistenceError::SuccessorRecordBinding {") { \
				if (branch > 2 || error_open || error_closed) fail(); errors += 1; record_binding += 1; error_open = 1; next; \
			} \
			if ($$0 == "                Err(UsrRollbackReversePersistenceError::SuccessorRecordBindingAndReopen {") { \
				if (branch != 3 || error_open || error_closed) fail(); errors += 1; combined += 1; error_open = 1; next; \
			} \
			if ($$0 == "                    durable: DurableUsrRollbackReverseRecord::Source,") durable_source += 1; \
			if ($$0 == "                    durable: DurableUsrRollbackReverseRecord::UsrRestored,") durable_successor += 1; \
			if (branch <= 3 && error_open && $$0 == "                })") { error_open = 0; error_closed = 1; next; } \
			if (branch <= 3 && $$0 == "            }") { if (!error_closed || error_open || branch_closed) fail(); branch_closed = 1; next; } \
			if (branch == 4 && error_open && $$0 == "            }),") { error_open = 0; error_closed = 1; branch_closed = 1; next; } \
			if (error_closed && !branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || header || branch != 4) exit 1 } \
	' "$$executor"; \
	if timeout 10s rg -n 'open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	timeout 10s grep -Fqx '    Published {' "$$executor"; \
	timeout 10s grep -Fqx '        UsrRollbackReverseAdvanceOutcome::Published {' "$$executor"; \
	timeout 10s grep -Fqx '    if let UsrRollbackReverseAdvanceOutcome::Published { .. } = &advance {' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, &successor_binding, &successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	reopened_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_reopened_reverse_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' <<<"$$reopened_binding_helper"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackReverseRecord::Source,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackReverseRecord::UsrRestored,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 3; \
	timeout 10s grep -Fqx '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	if timeout 10s rg -n 'exchange_retained_usr_once|attempt_usr_exchange_once|renameat2|RENAME_EXCHANGE|unlinkat|linkat|symlinkat|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|std::fs|fs::|File::open|OpenOptions|openat|unsafe[[:space:]]*\{' "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for kind in OperationKind::ALL {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -F -n 'for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' "$$storage" "$$restart" | timeout 10s wc -l )" = 3; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	timeout 10s grep -Fqx 'mod root_links_bound_update;' "$$tests"; \
	timeout 10s grep -Fqx 'const STORAGE_FAULTS: [StorageFault; 5] = [' "$$root_links"; \
	timeout 10s grep -Fq 'SourceCase::RootLinksCompletePost' "$$root_links"; \
	timeout 10s grep -Fq 'arm_after_usr_rollback_reverse_successor_binding_check_before_reopen(hook);' "$$root_links"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_reverse_durable_namespace_capture(namespace_change);' "$$races"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$restart" )" = 4; \
	for file in "$$executor" "$$authority" "$$reopen" "$$root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" "$$root_links" misc/make/startup-rollback-reverse-persistence-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
