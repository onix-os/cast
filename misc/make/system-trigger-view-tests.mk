.PHONY: forge-system-trigger-view-test

forge-system-trigger-view-test:
	@set -euo pipefail; \
	postblit="$(TOP_DIR)/crates/forge/src/client/postblit.rs"; \
	container="$(TOP_DIR)/crates/forge/src/client/postblit/system_trigger_container.rs"; \
	ephemeral="$(TOP_DIR)/crates/forge/src/client/postblit/retained_ephemeral.rs"; \
	stateful="$(TOP_DIR)/crates/forge/src/client/core/stateful_transition.rs"; \
	planning="$(TOP_DIR)/crates/forge/src/client/core/state_planning.rs"; \
	local_etc="$(TOP_DIR)/crates/forge/src/client/transaction_root.rs"; \
	if timeout 10s rg -n 'trigger_scope_may_execute_directly|revalidate_usr_name|Ok\(execute_trigger_directly' "$$postblit" "$$container" "$$ephemeral"; then \
		timeout 10s printf '%s\n' 'system triggers retained a direct or pathname-authorized execution route' >&2; \
		exit 1; \
	else \
		status="$$?"; \
		timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s rg -n 'named_installation_directory|Container::new\(' "$$container"; then \
		timeout 10s printf '%s\n' 'stateful system container retained pathname-based authority' >&2; \
		exit 1; \
	else \
		status="$$?"; \
		timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fq 'isolation_root: &' "$$postblit"; \
	timeout 10s grep -Fq 'local_etc: &' "$$postblit"; \
	timeout 10s grep -Fq 'system_trigger_container::after_payload();' "$$postblit"; \
	timeout 10s grep -Fq 'Error::SystemTriggerOperationAndRevalidation' "$$postblit"; \
	timeout 10s grep -Fq 'isolation_root,' "$$stateful"; \
	timeout 10s grep -Fq 'local_etc,' "$$stateful"; \
	timeout 10s grep -Fq 'let isolation_root = create_root_links(&self.installation.isolation_dir())?;' "$$planning"; \
	timeout 10s grep -Fq 'revalidate_mutable' "$$local_etc"; \
	timeout 10s grep -Fq 'const MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];' "$$container"; \
	timeout 10s grep -Fq 'const SYSTEM_MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];' "$$ephemeral"; \
	timeout 10s grep -Fq '.root_filesystem(TRANSACTION_ROOT_FILESYSTEM)' "$$container"; \
	timeout 10s grep -Fq '.pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS)' "$$container"; \
	timeout 10s grep -Fq '.root_filesystem(TRANSACTION_ROOT_FILESYSTEM)' "$$ephemeral"; \
	timeout 10s grep -Fq '.pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS)' "$$ephemeral"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.bind_rw_pinned(' "$$container" )" = 2; \
	for source in "$$postblit" "$$container" "$$ephemeral" "$$stateful" "$$planning" "$$local_etc"; do \
		lines="$$( timeout 10s wc -l < "$$source" )"; \
		timeout 10s test "$$lines" -le 1000; \
	done; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/system-trigger-view-tests.XXXXXX" )"; \
	trap 'timeout 10s rm -f -- "$$listed"' EXIT HUP INT TERM; \
	timeout 300s $(CARGO) test -p forge --lib -- --list > "$$listed"; \
	timeout 10s test -s "$$listed"; \
	for test in \
		client::postblit::retained_trigger_discovery::tests::stateful_system_scope_compiles_intent_from_the_retained_live_usr \
		client::postblit::system_trigger_container::tests::stateful_system_policy_is_read_only_and_exposes_only_bounded_kernel_views \
		client::postblit::system_trigger_container::tests::system_container_denies_root_tree_writes_and_exposes_only_declared_views \
		client::postblit::system_trigger_container::tests::retained_system_capabilities_reject_preconstruction_substitutions \
		client::postblit::system_trigger_container::tests::anchored_activation_rejects_substitutions_before_payload_mutation \
		client::postblit::system_trigger_container::tests::post_payload_revalidation_rejects_all_public_identity_swaps \
		client::postblit::retained_ephemeral::tests::retained_ephemeral_phase_policies_keep_transaction_etc_read_only \
		client::postblit::retained_ephemeral::tests::system_container_uses_writable_candidate_binds_inside_a_read_only_minimal_root \
		client::transaction_root::tests::mutable_revalidation_allows_content_changes_but_rejects_final_name_replacement \
		client::tests::archived_activation_archive_failure_reverses_usr_and_rearchives_the_candidate \
		client::tests::first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr \
		client::tests::fresh_identity_can_archive_after_a_complete_compensating_recovery; do \
		timeout 10s grep -Fqx "$$test: test" "$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done
