.PHONY: forge-startup-usr-rollback-reverse-dispatch-test

forge-startup-usr-rollback-reverse-dispatch-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_reverse_dispatch::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	for name in \
		durability_restart::startup_usr_rollback_reverse_dispatch_durability_faults_restart_as_pre_without_second_exchange \
		evidence_races::startup_usr_rollback_reverse_dispatch_admission_races_are_zero_effect_zero_advance \
		evidence_races::startup_usr_rollback_reverse_dispatch_effect_boundary_races_never_advance_or_retry \
		evidence_races::startup_usr_rollback_reverse_dispatch_final_durable_revalidation_races_leave_source_for_fresh_startup \
		fresh_handle_restart::startup_usr_rollback_reverse_dispatch_fresh_handles_restart_pre_without_second_exchange \
		journal_restart::startup_usr_rollback_reverse_dispatch_journal_faults_restart_to_exact_source_or_usr_restored \
		journal_update_process_kill::startup_usr_rollback_reverse_dispatch_journal_update_process_kills_restart_exactly \
		process_kill_restart::startup_usr_rollback_reverse_dispatch_process_kills_restart_to_exact_already_satisfied \
		success_matrix::startup_usr_rollback_reverse_dispatch_post_and_pre_matrix_reaches_exact_usr_restored \
		success_matrix::startup_usr_rollback_reverse_dispatch_usr_restored_routes_without_reverse_redispatch \
		syscall_results::startup_usr_rollback_reverse_dispatch_classifies_all_raw_syscall_reports_by_fresh_layout \
		syscall_results::startup_usr_rollback_reverse_dispatch_ambiguous_post_attempt_evidence_consumes_retry_capability; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	coordinator_contract='transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored'; \
	coordinator_journal_contract='transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_completion_faults_recover_from_exact_source_to_usr_restored'; \
	timeout 10s grep -Fqx "$$coordinator_contract: test" <<<"$$listed"; \
	timeout 10s grep -Fqx "$$coordinator_journal_contract: test" <<<"$$listed"; \
	dispatcher=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch.rs; \
	gate=crates/forge/src/client/startup_gate.rs; \
	root=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/support.rs; \
	durability=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/durability_restart.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/evidence_races.rs; \
	fresh=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/fresh_handle_restart.rs; \
	journal=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/journal_restart.rs; \
	journal_process_kill=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/journal_update_process_kill.rs; \
	process_kill=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/process_kill_restart.rs; \
	success=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/success_matrix.rs; \
	syscalls=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/syscall_results.rs; \
	coordinator=crates/forge/src/transition_identity/journal_coordinator/tests/usr_exchange_effect.rs; \
	forward_support=crates/forge/src/client/startup_recovery/forward_origin_test_support.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_reverse_dispatch;' "$$root"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$dispatcher"; \
	timeout 10s grep -Fqx 'mod tests;' "$$dispatcher"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn dispatch_usr_rollback_reverse_and_reopen<.*\(\n    journal: TransitionJournalStore,\n    ready: UsrRollbackReverseReady<'"'"'reservation>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackReverseDispatchError> \{' "$$dispatcher"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'let effect_seal = UsrRollbackReverseEffectSeal::new();' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_reverse_and_reopen(journal, durable)' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseApplyReconciliation::NotApplied' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseApplyReconciliation::Ambiguous' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Err(UsrRollbackReverseDispatchError::NotApplied);' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Err(UsrRollbackReverseDispatchError::Ambiguous);' "$$dispatcher" )" = 1; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor|CandidatePreserveIntent|\.advance\(|RetainedExchangeSyscallFault|renameat|RENAME_EXCHANGE|run_transaction_triggers|run_system_triggers|root_links|clear_transition_if_matches|remove_transition_if_matches|archive_previous|preserve_failed|loop|while' "$$dispatcher"; then exit 1; fi; \
	seal_count="$$( timeout 10s rg -n 'UsrRollbackReverseSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_count" = 1; \
	capture_count="$$( timeout 10s rg -n 'UsrRollbackReverseAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_count" = 1; \
	caller_count="$$( timeout 10s rg -n 'dispatch_usr_rollback_reverse_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_reverse_dispatch.rs' | timeout 10s wc -l )"; \
	timeout 10s test "$$caller_count" = 1; \
	timeout 10s grep -Fq 'super::startup_recovery::dispatch_usr_rollback_reverse_and_reopen(journal, ready)?' "$$gate"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$support" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$support" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -F -n 'for kind in OperationKind::ALL {' "$$success" "$$syscalls" | timeout 10s wc -l )" = 4; \
	timeout 10s grep -Fqx '    const ALL: [Self; 3] = [' "$$durability"; \
	timeout 10s grep -Fqx '        Self::FinalPreCapture,' "$$durability"; \
	timeout 10s grep -Fq 'for layout in [ReverseLayout::Post, ReverseLayout::Pre] {' "$$durability"; \
	timeout 10s grep -Fqx '    const ALL: [Self; 3] = [Self::Database, Self::Journal, Self::Namespace];' "$$races"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for race in EvidenceRace::ALL {' "$$races" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_before_usr_rollback_reverse_persistence_final_revalidation' "$$races" )" = 2; \
	timeout 10s grep -Fqx '    const ALL: [Self; 2] = [Self::FinalPreCapture, Self::JournalTemporarySync];' "$$fresh"; \
	timeout 10s grep -Fq 'for kind in OperationKind::ALL {' "$$fresh"; \
	timeout 10s grep -Fq 'let installation = Installation::open(&root, None).unwrap();' "$$fresh"; \
	timeout 10s grep -Fq 'let state_database = open_state_database(&installation);' "$$fresh"; \
	timeout 10s grep -Fqx 'const JOURNAL_FAULTS: [JournalFault; 5] = [' "$$journal"; \
	timeout 10s grep -Fqx '    const ALL: [Self; 5] = [' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'for kind in OperationKind::ALL {' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'for layout in ProcessLayout::ALL {' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'arm_journal_update_durability_callback(case.boundary.durability_boundary(), kill_self);' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'nix::libc::SIGKILL' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'power-loss oracle' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'if case.boundary.canonical_is_source() {' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'The newly production-wired archived child then owns a' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'assert_candidate_preserved_pending(&handled);' "$$journal_process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);' "$$journal_process_kill"; \
	if timeout 10s rg -n 'arm_next_.*fault|StorageFault' "$$journal_process_kill"; then exit 1; fi; \
	timeout 10s grep -Fqx '    const ALL: [Self; 4] = [' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	timeout 10s grep -Fq 'nix::libc::SIGKILL' "$$process_kill"; \
	timeout 10s grep -Fq 'status.signal()' "$$process_kill"; \
	for hook in arm_before_reverse_exchange_reconciliation_capture arm_before_usr_rollback_reverse_namespace_installation_root_sync arm_before_usr_rollback_reverse_namespace_final_pre_capture arm_before_usr_rollback_reverse_persistence_final_revalidation; do \
		timeout 10s grep -Fq "$$hook" "$$process_kill"; \
	done; \
	if timeout 10s rg -n 'arm_retained_exchange_syscall_fault|arm_usr_rollback_reverse_namespace_durability_fault|arm_next_.*fault' "$$process_kill"; then exit 1; fi; \
	timeout 10s grep -Fqx '    const ALL: [Self; 4] = [' "$$syscalls"; \
	timeout 10s test "$$( timeout 10s rg -n '^        Self::(SuccessAfterApply|ErrorAfterApply|ErrorWithoutApply|SuccessWithoutApply),$$' "$$syscalls" | timeout 10s wc -l )" = 4; \
	timeout 10s grep -Fqx '        for raw_error in [false, true] {' "$$syscalls"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseDispatchError::NotApplied' "$$support" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseDispatchError::Ambiguous' "$$support" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_reverse_exchange_intent_recovers_to_usr_restored' "$$coordinator" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_usr_restored_routes_to_candidate_preserve_intent' "$$coordinator" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'retained_exchange_syscall_count(), 2' "$$coordinator" )" = 4; \
	timeout 10s grep -Fq 'assert_eq!(pending.phase(), Phase::UsrRestored);' "$$forward_support"; \
	for file in "$$dispatcher" "$$gate" "$$root" "$$tests" "$$support" "$$durability" "$$races" "$$fresh" "$$journal" "$$journal_process_kill" "$$process_kill" "$$success" "$$syscalls" "$$coordinator" "$$forward_support" misc/make/startup-rollback-reverse-dispatch-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1; \
	for contract in "$$coordinator_contract" "$$coordinator_journal_contract"; do \
		timeout 1200s $(CARGO) test -p forge --lib "$$contract" -- --exact --test-threads=1; \
	done
