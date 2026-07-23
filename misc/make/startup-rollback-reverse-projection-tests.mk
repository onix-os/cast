.PHONY: forge-startup-usr-rollback-reverse-projection-test

forge-startup-usr-rollback-reverse-projection-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::masks::reverse_exchange_parent_mask_allows_only_mtime_and_ctime \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::masks::reverse_exchange_moved_usr_mask_allows_only_ctime \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::parent_rebind::reverse_exchange_parent_rebind_uses_only_value_identity \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::parent_rebind::reverse_exchange_parent_rebind_rejects_cross_device_pairs \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::parent_rebind::retained_reverse_exchange_parents_rebind_both_exact_public_names \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::projection::reverse_exchange_projection_accepts_only_exact_semantic_post_to_pre_movement \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::projection::reverse_exchange_projection_rejects_every_nonexchange_delta \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::projection::reverse_exchange_projection_requires_unique_tokens_and_exact_staging_shape \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::semantic_fields::reverse_exchange_projection_rejects_token_and_staging_substitution \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::semantic_fields::reverse_exchange_projection_rejects_tree_metadata_and_state_changes \
		client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::semantic_fields::reverse_exchange_projection_rejects_abi_wrapper_epoch_and_anchor_changes; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	capture_contract=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	projection=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange.rs; \
	test_root=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/reverse_exchange; \
	if timeout 10s rg -n '(^|[^[:alnum:]_])(rename|renameat|renameat2|unlink|unlinkat|link|linkat|symlink|symlinkat|syscall)[[:space:]]*[!(]|RENAME_EXCHANGE|exchange_retained_usr_once|exchange_live_and_staged|exchange_forward|exchange_reverse|std::process|Command::new' "$$projection"; then exit 1; fi; \
	if timeout 10s rg -n '(^|[^[:alnum:]_])(sync_all|sync_data|syncfs|fsync|fdatasync|sync_file_range)[[:space:]]*\(|\.sync[[:space:]]*\(' "$$projection"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|Deref|FnOnce|FnMut|unsafe[[:space:]]*\{|libc::|nix::' "$$projection"; then exit 1; fi; \
	if timeout 10s rg -U -n 'pub\(in crate::client::startup_reconciliation::activation_namespace\)[^{;]{0,512}(File|RawFd|BorrowedFd|OwnedFd)' "$$projection"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*pub(\([^)]*\))?[[:space:]]+[A-Za-z_][A-Za-z0-9_]*:[[:space:]]*(File|RawFd|BorrowedFd|OwnedFd)' "$$projection"; then exit 1; fi; \
	for file in \
		"$$capture_contract" \
		"$$projection" \
		"$$test_root/tests.rs" \
		"$$test_root/tests/masks.rs" \
		"$$test_root/tests/parent_rebind.rs" \
		"$$test_root/tests/projection.rs" \
		"$$test_root/tests/semantic_fields.rs"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::activation_namespace::capture::reverse_exchange::tests::' \
		-- --test-threads=1
