.PHONY: forge-startup-usr-rollback-reverse-namespace-durability-test

forge-startup-usr-rollback-reverse-namespace-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_reconciliation::activation_namespace::rollback_reverse_proof::effect_reconciliation::durability::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 5; \
	for name in \
		reverse_parent_durability_syncs_staging_then_root_then_proves_pre_for_both_sources \
		reverse_parent_durability_faults_stop_at_the_exact_event_prefix_for_both_sources \
		reverse_parent_durability_between_sync_namespace_race_prevents_root_sync \
		reverse_parent_durability_final_fresh_pre_race_rejects_completion_after_both_syncs \
		reverse_durable_namespace_revalidation_requires_fresh_exact_pre_for_both_sources; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	low=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/durability.rs; \
	bridge=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof/effect_reconciliation/durability.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	tests=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof/effect_reconciliation/durability/tests.rs; \
	timeout 10s test "$$( timeout 10s grep -Fc '.sync_all()' "$$low" )" = 2; \
	timeout 10s rg -U -q 'self\.staging\n[[:space:]]*\.sync_all\(\)' "$$low"; \
	timeout 10s rg -U -q 'self\.root\n[[:space:]]*\.sync_all\(\)' "$$low"; \
	staging_line="$$( timeout 10s grep -nF '        self.sync_staging_parent()?;' "$$low" | timeout 10s cut -d: -f1 )"; \
	root_line="$$( timeout 10s grep -nF '        self.sync_installation_root()?;' "$$low" | timeout 10s cut -d: -f1 )"; \
	final_line="$$( timeout 10s grep -nF '        let final_pre = capture_snapshot(installation, record)?;' "$$low" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$staging_line" -lt "$$root_line"; \
	timeout 10s test "$$root_line" -lt "$$final_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fn complete_parent_durability(' "$$bridge" )" = 2; \
	timeout 10s grep -Fq 'final_pre.fingerprint() != authenticated_pre.fingerprint()' "$$low"; \
	timeout 10s grep -Fq 'final_pre_projection.layout() != UsrExchangeLayout::Pre' "$$low"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate(' "$$low" )" = 1; \
	timeout 10s grep -Fq 'run_before_durable_revalidation_capture();' "$$low"; \
	if timeout 10s rg -n 'journal\.load\(\)' "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'renameat2|RENAME_EXCHANGE|exchange_retained_usr_once|attempt_usr_exchange_once|unlinkat|linkat|symlinkat|create_root_links|std::process|Command::new|\.advance[[:space:]]*\(|rollback_successor|forward_successor|state_db|Database' "$$low" "$$bridge"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|unsafe[[:space:]]*\{' "$$low" "$$bridge"; then exit 1; fi; \
	if timeout 10s rg -n 'File::open|OpenOptions|open_beneath|openat2|openat[[:space:]]*\(' "$$low" "$$bridge"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err)|match[[:space:]]+raw_report|matches!\([^\n]*raw_report|if[[:space:]]+let[^\n]*raw_report' "$$low" "$$bridge"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]' "$$low" "$$bridge"; then exit 1; fi; \
	for file in "$$low" "$$bridge" "$$tests" misc/make/startup-rollback-reverse-namespace-durability-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
