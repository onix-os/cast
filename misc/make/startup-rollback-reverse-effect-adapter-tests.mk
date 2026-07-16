.PHONY: forge-startup-usr-rollback-reverse-effect-adapter-test

forge-startup-usr-rollback-reverse-effect-adapter-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 4; \
	for test in \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::reverse_exchange_effect_retains_normal_success_after_single_applied_attempt \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::reverse_exchange_effect_retains_error_after_single_applied_attempt \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::reverse_exchange_effect_retains_success_after_single_unapplied_attempt \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::reverse_exchange_effect_never_retries_reported_error_without_application; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/effect.rs; \
	effect_tests=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/effect/tests.rs; \
	projection=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange.rs; \
	timeout 10s grep -Fqx 'mod effect;' "$$projection"; \
	timeout 10s grep -Fqx 'pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingReverseExchangeReconciliation {' "$$effect"; \
	timeout 10s grep -Fqx '    parents: RetainedReverseExchangeParents,' "$$effect"; \
	timeout 10s grep -Fqx '    raw_report: Result<(), Error>,' "$$effect"; \
	timeout 10s grep -Fq 'fn attempt_usr_exchange_once(' "$$effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        self,' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'raw_report' "$$effect" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exchange_retained_usr_once(' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'exchange_retained_usr_once\(' crates/forge/src --glob '*.rs' | timeout 10s wc -l )" = 3; \
	effect_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$effect" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$effect_code"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|FnOnce|FnMut|dyn[[:space:]]+Fn|&File|->[[:space:]]*File' <<<"$$effect_code"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat2|RENAME_EXCHANGE|linux_fs|syscall[[:space:]]*\(|unsafe[[:space:]]*\{' <<<"$$effect_code"; then exit 1; fi; \
	if timeout 10s rg -n 'sync_all|sync_data|syncfs|fsync|fdatasync|sync_file_range|\.sync[[:space:]]*\(|\.advance[[:space:]]*\(|rollback_successor|forward_successor|TransitionJournal|state_db|Database' <<<"$$effect_code"; then exit 1; fi; \
	if timeout 10s rg -n '^impl PendingReverseExchangeReconciliation|^[[:space:]]*pub(\([^)]*\))?[[:space:]]+[A-Za-z_][A-Za-z0-9_]*:' "$$effect"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(retained_exchange_syscall_count(), 1);' "$$effect_tests" )" = 4; \
	timeout 10s grep -Fq 'reset_retained_exchange_syscall_count();' "$$effect_tests"; \
	timeout 10s grep -Fq 'RetainedExchangeSyscallFault::ErrorAfterApply' "$$effect_tests"; \
	timeout 10s grep -Fq 'RetainedExchangeSyscallFault::SuccessWithoutApply' "$$effect_tests"; \
	timeout 10s grep -Fq 'RetainedExchangeSyscallFault::ErrorWithoutApply' "$$effect_tests"; \
	for file in "$$effect" "$$effect_tests" misc/make/startup-rollback-reverse-effect-adapter-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::tests::' \
		-- --test-threads=1
