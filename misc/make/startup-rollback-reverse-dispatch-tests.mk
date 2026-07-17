.PHONY: forge-startup-usr-rollback-reverse-dispatch-test

forge-startup-usr-rollback-reverse-dispatch-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	prefix='client::startup_recovery::usr_rollback_reverse_dispatch::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 4; \
	for name in \
		success_matrix::startup_usr_rollback_reverse_dispatch_post_and_pre_matrix_reaches_exact_usr_restored \
		success_matrix::startup_usr_rollback_reverse_dispatch_usr_restored_is_not_redispatched_or_chained \
		syscall_results::startup_usr_rollback_reverse_dispatch_classifies_all_raw_syscall_reports_by_fresh_layout \
		syscall_results::startup_usr_rollback_reverse_dispatch_ambiguous_post_attempt_evidence_consumes_retry_capability; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	dispatcher=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch.rs; \
	gate=crates/forge/src/client/startup_gate.rs; \
	root=crates/forge/src/client/startup_recovery.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/support.rs; \
	success=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/success_matrix.rs; \
	syscalls=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch/tests/syscall_results.rs; \
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
	timeout 10s grep -Fqx '    const ALL: [Self; 4] = [' "$$syscalls"; \
	timeout 10s test "$$( timeout 10s rg -n '^        Self::(SuccessAfterApply|ErrorAfterApply|ErrorWithoutApply|SuccessWithoutApply),$$' "$$syscalls" | timeout 10s wc -l )" = 4; \
	timeout 10s grep -Fqx '        for raw_error in [false, true] {' "$$syscalls"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseDispatchError::NotApplied' "$$support" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseDispatchError::Ambiguous' "$$support" )" = 1; \
	for file in "$$dispatcher" "$$gate" "$$root" "$$tests" "$$support" "$$success" "$$syscalls" misc/make/startup-rollback-reverse-dispatch-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
