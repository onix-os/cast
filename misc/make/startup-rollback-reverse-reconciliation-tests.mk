.PHONY: forge-startup-usr-rollback-reverse-reconciliation-test

forge-startup-usr-rollback-reverse-reconciliation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	for test in \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_classifies_normal_success_from_fresh_pre_evidence \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_classifies_reported_error_from_fresh_pre_evidence \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_classifies_reported_success_only_from_exact_unchanged_post \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_classifies_reported_error_only_from_exact_unchanged_post \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_maps_fresh_capture_failure_to_ambiguous \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_maps_extra_namespace_delta_to_ambiguous \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_maps_foreign_mixed_layout_to_ambiguous \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_rejects_non_post_baseline_after_fresh_capture \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::reverse_exchange_reconciliation_rejects_projection_from_another_post_snapshot; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	effect=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/effect.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/effect/reconciliation.rs; \
	reconciliation_tests=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange/effect/reconciliation/tests.rs; \
	timeout 10s grep -Fqx 'mod reconciliation;' "$$effect"; \
	timeout 10s grep -Fqx 'pub(in crate::client::startup_reconciliation::activation_namespace) enum ReverseExchangeReconciliation {' "$$reconciliation"; \
	timeout 10s grep -Fqx '    Applied(AppliedReverseExchangeReconciliation),' "$$reconciliation"; \
	timeout 10s grep -Fqx '    NotApplied,' "$$reconciliation"; \
	timeout 10s grep -Fqx '    Ambiguous,' "$$reconciliation"; \
	timeout 10s grep -Fqx '    parents: RetainedReverseExchangeParents,' "$$reconciliation"; \
	timeout 10s grep -Fqx '    fresh_pre: NamespaceSnapshot,' "$$reconciliation"; \
	timeout 10s grep -Fqx '    fresh_pre_projection: ProjectedReverseNamespace,' "$$reconciliation"; \
	timeout 10s grep -Fqx '    raw_report: Result<(), Error>,' "$$reconciliation"; \
	timeout 10s grep -Fq 'fn reconcile(' "$$reconciliation"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        let fresh_capture = capture_snapshot(installation, record);' "$$reconciliation" )" = 1; \
	timeout 10s rg -U -q 'run_before_reverse_exchange_reconciliation_capture\(\);\n        let fresh_capture = capture_snapshot\(installation, record\);' "$$reconciliation"; \
	classification_line="$$( timeout 10s grep -nF '        let classification = classify_fresh_namespace(' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	match_line="$$( timeout 10s grep -nF '        match classification {' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	first_consumption_line="$$( timeout 10s grep -nF '                let Self {' "$$reconciliation" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$classification_line" -lt "$$match_line"; \
	timeout 10s test "$$match_line" -lt "$$first_consumption_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'revalidate_value_identity(installation).is_err()' "$$reconciliation" )" = 2; \
	timeout 10s grep -Fq 'if fresh.fingerprint() == authenticated_post_baseline.fingerprint() {' "$$reconciliation"; \
	timeout 10s grep -Fq 'baseline_projection.layout() != UsrExchangeLayout::Post' "$$reconciliation"; \
	timeout 10s grep -Fq '.require_post_to_pre(&fresh_projection)' "$$reconciliation"; \
	timeout 10s grep -Fq 'run_before_reverse_exchange_reconciliation_capture();' "$$reconciliation"; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err)|match[[:space:]]+raw_report|matches!\([^\n]*raw_report|if[[:space:]]+let[^\n]*raw_report' "$$reconciliation"; then exit 1; fi; \
	reconciliation_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$reconciliation" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$reconciliation_code"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat2|RENAME_EXCHANGE|exchange_retained_usr_once|unlinkat|linkat|symlinkat|create_root_links|std::process|Command::new' "$$reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'sync_all|sync_data|syncfs|fsync|fdatasync|sync_file_range|\.sync[[:space:]]*\(|\.advance[[:space:]]*\(|rollback_successor|forward_successor|TransitionJournal|state_db|Database' "$$reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|unsafe[[:space:]]*\{' "$$reconciliation"; then exit 1; fi; \
	for file in "$$effect" "$$reconciliation" "$$reconciliation_tests" misc/make/startup-rollback-reverse-reconciliation-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::activation_namespace::capture::reverse_exchange::effect::reconciliation::tests::' \
		-- --test-threads=1
