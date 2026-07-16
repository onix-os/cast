.PHONY: stone-read-test forge-read-only-installation-test forge-installation-test \
	forge-mutable-startup-namespace-test forge-candidate-pre-journal-durability-test \
	forge-transition-journal-coordinator-test forge-transition-recovery-classifier-test \
	forge-startup-reconciliation-test \
	forge-linux-fs-test forge-cache-test forge-client-direct-test \
	forge-database-adapter-test forge-read-only-substrate-test \
	forge-read-only-client-test forge-transition-journal-contract-test \
	forge-transition-runtime-evidence-test forge-transition-journal-successor-test \
	forge-transition-journal-test \
	stone-recipe-derivation-provenance-test \
	stone-recipe-derivation-validation-test stone-recipe-build-lock-test \
	stone-recipe-package-validation-test \
	stone-recipe-build-policy-validation-test stone-recipe-build-policy-contract-test \
	stone-recipe-build-policy-patch-test tools-buildinfo-semantic-fingerprint-test \
	container-cgroup-test \
	container-process-runtime-test container-mount-boundary-test \
	container-root-host-safe-test mason-package-collect-test \
	mason-package-collect-transaction-test mason-analysis-handler-test mason-emit-test \
	mason-archive-test mason-package-publication-test \
	mason-git-materialization-test mason-paths-test mason-executor-test \
	mason-build-context-test mason-recipe-explanation-test \
	mason-upstream-git-cache-test mason-build-root-test mason-profile-test \
	mason-planner-bootstrap-test mason-policy-test \
	config-gluon-store-test gitwrap-repository-fs-test gitwrap-all-test \
	forge-repository-manager-test \
	forge-security-fixture-test

forge-read-only-installation-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		installation::snapshot::tests::two_readers_share_global_and_custom_cache_locks_until_the_last_reader_drops \
		installation::snapshot::tests::writable_root_opened_explicitly_read_only_never_becomes_mutable_or_frozen \
		installation::snapshot::tests::mutable_and_frozen_modes_do_not_expose_read_only_snapshot_authority \
		installation::snapshot::tests::explicit_snapshot_is_rejected_before_client_coordinator_or_database_mutation \
		installation::snapshot::tests::naturally_read_only_open_is_rejected_before_client_coordinator_or_database_mutation \
		installation::snapshot::tests::contended_shared_snapshot_lock_has_a_typed_zero_budget_timeout_without_mutation \
		installation::snapshot::tests::missing_cast_fails_without_creating_or_changing_any_entry \
		installation::snapshot::tests::missing_default_cache_fails_without_recreating_or_changing_any_entry \
		installation::snapshot::tests::missing_global_lock_fails_without_recreating_it \
		installation::snapshot::tests::missing_custom_cache_lock_fails_without_recreating_it \
		installation::snapshot::tests::missing_custom_cache_directory_fails_without_creating_or_changing_any_entry \
		installation::snapshot::tests::retained_snapshot_rejects_installation_root_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_cast_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_lockfile_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_default_cache_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_custom_cache_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_custom_cache_lockfile_substitution \
		installation::snapshot::tests::open_revalidate_clone_and_drop_leave_recursive_metadata_and_contents_unchanged; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-installation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^installation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 28; \
	timeout 900s $(CARGO) test -p forge --lib "installation::tests::" -- --test-threads=1

forge-mutable-startup-namespace-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^client::mutable_startup_namespace_tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 5; \
	for test in \
		client::mutable_startup_namespace_tests::every_system_database_open_is_anchored_and_replacement_directories_remain_untouched \
		client::mutable_startup_namespace_tests::namespace_revalidation_supersedes_a_simultaneous_sqlite_open_failure \
		client::mutable_startup_namespace_tests::startup_journal_uses_retained_cast_and_never_mutates_its_replacement \
		client::mutable_startup_namespace_tests::startup_namespace_substitution_supersedes_a_simultaneous_journal_open_failure \
		client::mutable_startup_namespace_tests::replaced_global_lockfile_is_rejected_without_touching_the_foreign_inode; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-candidate-pre-journal-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	prefix_count="$$( timeout 10s grep -Ec '^(transition_identity::prejournal_inventory::tests|client::tests)::candidate_pre_journal_.*: test$$' <<<"$$listed" )"; \
	core_count="$$( timeout 10s grep -c '^transition_identity::prejournal_inventory::tests::candidate_pre_journal_.*: test$$' <<<"$$listed" )"; \
	client_count="$$( timeout 10s grep -c '^client::tests::candidate_pre_journal_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$prefix_count" = 12; \
	timeout 10s test "$$core_count" = 10; \
	timeout 10s test "$$client_count" = 2; \
	for test in \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_nested_tree_seals_and_allows_sole_new_marker \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_existing_marker_is_adopted_without_delta \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_add_delete_replace_and_content_changes_fail \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_same_metadata_content_rewrite_fails \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_symlink_is_opaque_and_target_change_fails \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_special_mount_and_hardlink_entries_fail \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_entry_depth_and_name_bounds_are_inclusive \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_regular_byte_bound_is_inclusive \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_operation_and_deadline_bounds_are_inclusive \
		transition_identity::prejournal_inventory::tests::candidate_pre_journal_access_and_default_acls_fail \
		client::tests::candidate_pre_journal_namespace_substitution_preserves_detached_cast_and_replacement_before_markers \
		client::tests::candidate_pre_journal_legacy_hardlinked_archived_payload_fails_before_marker_or_exchange \
		client::tests::failed_first_install_can_retry_the_exact_marker_only_previous_baseline \
		client::tests::first_install_marker_retry_rejects_marker_plus_foreign_content_unchanged \
		client::tests::exact_parked_tree_marker_hardlink_is_reauthorized_after_reopen; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "candidate_pre_journal_" -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib \
		"client::tests::failed_first_install_can_retry_the_exact_marker_only_previous_baseline" \
		-- --exact --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib \
		"client::tests::first_install_marker_retry_rejects_marker_plus_foreign_content_unchanged" \
		-- --exact --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib \
		"client::tests::exact_parked_tree_marker_hardlink_is_reauthorized_after_reopen" \
		-- --exact --test-threads=1

forge-transition-journal-coordinator-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^transition_identity::journal_coordinator::tests::journal_coordinator_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 42; \
	for test in \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_reaches_candidate_prepared_through_exact_generations \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_previous_origins_and_options_are_exact \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_activation_reaches_candidate_prepared_without_allocation_phases \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reaches_candidate_prepared_without_allocation_phases \
		transition_identity::journal_coordinator::tests::journal_coordinator_creation_captures_exact_epoch_tokens_and_runtime_tree_witnesses \
		transition_identity::journal_coordinator::tests::journal_coordinator_quarantine_name_is_fixed_transition_token_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_state_authority_cannot_be_reinterpreted_between_operations \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_prejournal_authority_preserves_residue_and_name_substitution \
		transition_identity::journal_coordinator::tests::journal_coordinator_wrong_operation_or_phase_is_rejected_without_record_change \
		transition_identity::journal_coordinator::tests::journal_coordinator_fresh_allocation_effect_observes_durable_intent_before_database_commit \
		transition_identity::journal_coordinator::tests::journal_coordinator_allocation_finish_rejects_missing_cleared_foreign_and_wrong_state_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_database_commit_and_completion_share_exact_transition_correlation \
		transition_identity::journal_coordinator::tests::journal_coordinator_post_commit_journal_failure_preserves_matching_database_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_prepare_effect_order_and_failure_preserve_exact_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_state_id_publication_failures_preserve_started_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_state_id_appearance_before_prepare_intent_blocks_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_existing_candidate_database_removal_blocks_journal_creation \
		transition_identity::journal_coordinator::tests::journal_coordinator_distinct_previous_database_removal_blocks_journal_creation \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_triggers_complete_exact_new_state_and_active_reblit_generations \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_transaction_triggers_are_rejected_without_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_effect_error_runs_once_and_preserves_started \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_intent_faults_leave_old_or_successor_without_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_completion_faults_leave_started_or_complete_after_one_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_preflight_failure_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_failure_preserves_started \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_database_changes_are_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_previous_database_removal_is_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_global_database_audit_blocks_foreign_rows \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_state_id_and_public_name_substitution_are_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_failure_releases_journal_while_error_lives \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_proof_is_owned_for_every_operation_and_uses_exact_os_info \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_metadata_proof_rejects_independent_expectation_mismatch_without_mutation \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_prepare_rejects_same_byte_foreign_candidate_before_metadata_or_state_id \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_substitution_before_trigger_intent_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_substitution_during_trigger_effect_stops_before_completion \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_publication_failure_releases_authorities_while_error_lives \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_has_exact_phase_and_generation_for_every_operation \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_performs_no_exchange_or_root_link_publication \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_revalidates_all_retained_evidence_before_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_reseals_candidate_before_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_faults_leave_exact_predecessor_or_intent \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_failure_releases_journal_while_error_lives; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	trigger_contract="crates/forge/src/transition_identity/journal_coordinator/transaction_triggers.rs"; \
	usr_exchange_contract="crates/forge/src/transition_identity/journal_coordinator/usr_exchange_intent.rs"; \
	prepare_contract="crates/forge/src/transition_identity/journal_coordinator/candidate_preparation.rs"; \
	coordinator_contract="crates/forge/src/transition_identity/journal_coordinator/mod.rs"; \
	authority_contract="crates/forge/src/transition_identity/candidate_state_authority.rs"; \
	tree_lifecycle="crates/forge/src/transition_identity/tree_lifecycle.rs"; \
	timeout 10s grep -Fqx 'mod candidate_state_authority;' crates/forge/src/transition_identity.rs; \
	if timeout 10s grep -Fqx 'pub(crate) mod candidate_state_authority;' crates/forge/src/transition_identity.rs; then \
		timeout 10s printf '%s\n' 'candidate state authority module visibility widened' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	for variant in '    UnknownIdAbsent,' '    KnownIdAbsent(state::Id),' '    ExistingId(state_tree_metadata::RetainedTreeStateId),'; do \
		timeout 10s grep -Fqx "$$variant" "$$authority_contract"; \
	done; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_active_reblit_candidate(' "$$tree_lifecycle"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_retained_active_reblit_candidate(' "$$tree_lifecycle"; \
	if timeout 10s grep -nF 'candidate_state: Option<state::Id>' "$$tree_lifecycle"; then \
		timeout 10s printf '%s\n' 'candidate preparation collapsed three-way state authority back into Option' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'match parts.candidate_id' "$$coordinator_contract"; then \
		timeout 10s printf '%s\n' 'coordinator again treats logical candidate ID presence as filesystem publication' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'self.record.operation != Operation::NewState' "$$prepare_contract"; then \
		timeout 10s printf '%s\n' 'candidate proof again conflates ActiveReblit with existing archived state ID' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -RInF 'publish_new' crates/forge/src/transition_identity; then \
		timeout 10s printf '%s\n' 'state-ID publisher regained NewState-only semantics' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx '            Operation::NewState | Operation::ActiveReblit => {' "$$prepare_contract"; \
	timeout 10s grep -Fq 'RetainedTreeStateId::publish_absent' "$$prepare_contract"; \
	if timeout 10s grep -RInE 'prepare_(retained_)?active_reblit_candidate' crates/forge/src/client; then \
		timeout 10s printf '%s\n' 'known-ID/absent candidate authority gained a live callsite before startup recovery exists' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx "pub(super) struct StatefulTransactionTriggerAuthority<'authority> {" "$$trigger_contract"; \
	timeout 10s grep -Fqx 'pub(super) enum StatefulTransactionTriggerFailure<E>' "$$trigger_contract"; \
	timeout 10s grep -Fqx 'pub(crate) enum PreparedStatefulTransitionCoordinator {' "$$prepare_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct PreparedTransactionTriggerCoordinator {' "$$prepare_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct PreparedArchivedTransitionCoordinator {' "$$prepare_contract"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) metadata: CandidateMetadataProof,' "$$prepare_contract" )" = 3; \
	timeout 10s grep -Fqx 'impl PreparedTransactionTriggerCoordinator {' "$$trigger_contract"; \
	timeout 10s grep -Fqx '    pub(super) fn run_transaction_triggers<E, F>(' "$$trigger_contract"; \
	if timeout 10s grep -nF 'Option<CandidateMetadataProof>' "$$prepare_contract" "$$trigger_contract"; then \
		timeout 10s printf '%s\n' 'proof-bearing coordinator authority became optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	runner_signature="$$( timeout 10s sed -n '/pub(super) fn run_transaction_triggers/,/) -> Result/p' "$$trigger_contract" )"; \
	if timeout 10s grep -Fq 'CandidateMetadataProof' <<<"$$runner_signature"; then \
		timeout 10s printf '%s\n' 'transaction-trigger runner accepts a caller-supplied metadata proof' >&2; exit 1; \
	fi; \
	if timeout 10s grep -nF 'PreparedArchivedTransitionCoordinator' "$$trigger_contract"; then \
		timeout 10s printf '%s\n' 'archived activation acquired transaction-trigger authority' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if widened="$$( timeout 10s grep -nE \
		'pub\(crate\).*(StatefulTransactionTriggerAuthority|StatefulTransactionTriggerFailure|run_transaction_triggers)' \
		crates/forge/src/transition_identity/journal_coordinator/mod.rs "$$prepare_contract" "$$trigger_contract" )"; then \
		timeout 10s printf '%s\n' 'unwired transaction-trigger authority was widened before metadata-aware live integration:' "$$widened" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx 'mod usr_exchange_intent;' "$$coordinator_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct UsrExchangeIntentCoordinator {' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx 'pub(super) enum UsrExchangeIntentFailure {' "$$usr_exchange_contract"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) fn begin_usr_exchange_intent(' "$$usr_exchange_contract" )" = 2; \
	timeout 10s grep -Fqx '    coordinator: StatefulTransitionCoordinator,' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx '    metadata: CandidateMetadataProof,' "$$usr_exchange_contract"; \
	if timeout 10s grep -nF 'Option<CandidateMetadataProof>' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' '/usr exchange-intent authority made its metadata proof optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nE 'renameat2|exchange_forward|create_root_links|symlinkat|unlinkat' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' 'intent-only /usr exchange boundary acquired a namespace mutation primitive' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'pub(crate) fn begin_usr_exchange_intent' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' '/usr exchange-intent transition widened before live recovery exists' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if callsites="$$( timeout 10s grep -RInE \
		'begin_transition|begin_fresh_allocation|transition_id_for_allocation|finish_fresh_allocation|begin_candidate_prepare|finish_candidate_prepare|run_transaction_triggers|begin_usr_exchange_intent' \
		--include='*.rs' --exclude-dir=journal_coordinator crates/forge/src )"; then \
		timeout 10s printf '%s\n' 'journal coordinator has a live callsite outside its contract module:' "$$callsites" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" = 1; \
	fi; \
	timeout 900s $(CARGO) test -p forge --lib \
		"transition_identity::journal_coordinator::tests::journal_coordinator_" \
		-- --test-threads=1

forge-transition-recovery-classifier-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^transition_journal::recovery::tests::recovery_classifier_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 3; \
	timeout 300s $(CARGO) test -p forge --lib \
		"transition_journal::recovery::tests::recovery_classifier_" \
		-- --test-threads=1

forge-startup-reconciliation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::tests::startup_reconciliation_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 8; \
	for test in \
		client::startup_reconciliation::tests::startup_reconciliation_database_phase_matrix_is_exact \
		client::startup_reconciliation::tests::startup_reconciliation_matching_allocation_behind_journal_is_retained \
		client::startup_reconciliation::tests::startup_reconciliation_inconsistent_database_audit_is_blocked \
		client::startup_reconciliation::tests::startup_reconciliation_current_and_historical_runtime_epochs_are_distinguished \
		client::startup_reconciliation::tests::startup_reconciliation_two_link_tree_marker_remains_unresolved \
		client::startup_reconciliation::tests::startup_reconciliation_final_tree_name_substitution_is_not_retained \
		client::startup_reconciliation::tests::startup_reconciliation_retains_exact_database_instance \
		client::startup_reconciliation::tests::startup_reconciliation_pending_error_releases_journal_before_retry; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib \
		"client::startup_reconciliation::tests::startup_reconciliation_" \
		-- --test-threads=1

forge-linux-fs-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^linux_fs::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 17; \
	for test in \
		linux_fs::tests::interrupted_retry_limit_accepts_n_and_rejects_n_plus_one \
		linux_fs::tests::expired_retry_deadline_fails_before_another_syscall \
		linux_fs::tests::expired_rename_deadline_preserves_both_namespaces \
		linux_fs::tests::expired_sync_filesystem_deadline_fails_before_syncfs \
		linux_fs::tests::procfs_authentication_rejects_an_ordinary_filesystem \
		linux_fs::tests::authenticated_procfs_descriptor_child_path_binds_the_retained_directory \
		linux_fs::tests::proc_pid_parser_accepts_only_bounded_canonical_decimal \
		linux_fs::tests::thread_self_parser_requires_exact_current_process_and_thread \
		linux_fs::tests::chmod_revalidates_the_exact_opath_inode_and_mode \
		linux_fs::tests::descriptor_times_update_the_retained_regular_inode_not_its_replacement \
		linux_fs::tests::descriptor_read_uses_the_retained_inode_and_preserves_atime \
		linux_fs::tests::descriptor_read_rejects_non_regular_capabilities \
		linux_fs::tests::descriptor_times_support_a_mode_zero_directory \
		linux_fs::tests::descriptor_times_update_a_symlink_without_touching_its_target \
		linux_fs::tests::authenticated_procfs_links_an_unnamed_inode_without_privilege \
		linux_fs::tests::new_directory_normalization_retains_identity_and_rejects_name_substitution \
		linux_fs::tests::chmod_uses_the_calling_tasks_private_descriptor_table; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-cache-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::cache::download_limit_tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::cache::download_limit_tests::declared_package_size_can_only_tighten_the_global_ceiling \
		client::cache::download_limit_tests::cached_package_symlink_is_rejected_without_reading_target \
		client::cache::download_limit_tests::cached_package_fifo_is_rejected_without_blocking \
		client::cache::download_limit_tests::cached_package_requires_the_exact_declared_size_at_n_and_n_plus_one \
		client::cache::download_limit_tests::retained_download_descriptor_defeats_path_substitution_before_unpack \
		client::cache::download_limit_tests::asset_publication_replaces_fifo_and_symlink_without_blocking_or_touching_target \
		client::cache::download_limit_tests::asset_authentication_rejects_truncated_and_n_plus_one_entries \
		client::cache::download_limit_tests::competing_asset_publishers_reuse_one_verified_winner \
		client::cache::download_limit_tests::competing_download_publishers_reuse_one_verified_winner \
		client::cache::download_limit_tests::armed_publication_cleanup_removes_only_the_exact_moved_inode \
		client::cache::download_limit_tests::random_stages_clean_failure_without_truncating_legacy_part_file; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-client-direct-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 213; \
	timeout 1200s $(CARGO) test -p forge --lib "client::tests::" -- --test-threads=1

forge-database-adapter-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	layout_count="$$( timeout 10s grep -c '^db::layout::test::.*: test$$' <<<"$$listed" )"; \
	meta_count="$$( timeout 10s grep -c '^db::meta::test::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$layout_count" = 11; \
	timeout 10s test "$$meta_count" = 10; \
	timeout 900s $(CARGO) test -p forge --lib "db::layout::test::" -- --test-threads=1; \
	timeout 900s $(CARGO) test -p forge --lib "db::meta::test::" -- --test-threads=1

forge-read-only-substrate-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		db::read_only::tests::deserialized_adapters_query_exact_state_meta_and_selected_layout_without_mutation \
		db::read_only::tests::authorizer_denies_writes_and_functions_and_connection_remains_clean \
		db::read_only::tests::opcode_and_deadline_interruptions_are_deterministic_and_handlers_are_cleared \
		db::read_only::tests::connection_mutex_wait_uses_the_same_finite_query_deadline \
		db::read_only::tests::temp_store_is_memory_only_and_ordered_scan_leaves_source_unchanged \
		db::read_only::tests::sidecar_inode_kinds_fail_closed_and_are_preserved \
		db::read_only::tests::oversized_database_image_fails_before_allocation_without_mutation \
		db::read_only::tests::corrupt_database_image_fails_typed_without_mutation \
		db::read_only::tests::metadata_reconstructed_id_and_i32_release_corruption_fail_typed \
		db::read_only::tests::missing_unknown_and_extra_diesel_migrations_fail_typed \
		db::read_only::tests::absent_migration_table_is_version_set_validation_failure_not_migration \
		transition_journal::read_only::tests::absent_and_preexisting_clean_journals_are_retained_without_provisioning \
		transition_journal::read_only::tests::valid_canonical_transition_fails_closed_and_is_preserved \
		transition_journal::read_only::tests::corrupt_canonical_and_interrupted_temporary_fail_closed_unchanged; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-read-only-client-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^client::read_only::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	timeout 600s $(CARGO) test -p forge --lib "client::read_only::tests::" -- --test-threads=1

forge-transition-journal-contract-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		transition_journal::tests::canonical_round_trip_covers_every_phase \
		transition_journal::tests::canonical_v1_full_frame_and_json_order_are_locked_by_golden_bytes \
		transition_journal::tests::exact_record_limit_and_n_plus_one_are_distinguished \
		transition_journal::tests::checksum_covers_header_fields_and_payload \
		transition_journal::tests::unknown_frame_and_payload_versions_are_rejected \
		transition_journal::tests::unknown_phase_field_and_duplicate_field_are_rejected \
		transition_journal::tests::reboot_identity_schema_is_required_strict_and_has_no_v1_aliases \
		transition_journal::tests::record_trailing_bytes_and_noncanonical_json_are_rejected \
		transition_journal::tests::bounded_identifiers_and_obvious_semantic_mismatches_fail_closed \
		transition_journal::tests::generated_tree_tokens_are_canonical_and_distinct \
		transition_journal::tests::preparing_constructor_derives_wire_fields_and_rejects_invalid_operation_layouts \
		transition_journal::tests::preparing_pins_epoch_tokens_runtime_witnesses_and_operation_relationships_fail_closed \
		transition_journal::tests::disabled_forward_phases_and_rollback_plan_placement_fail_closed \
		transition_journal::tests::all_operations_and_forward_option_paths_have_exact_successors \
		transition_journal::tests::rollback_is_available_until_commit_except_after_verified_boot_sync \
		transition_journal::tests::conditional_advance_rejects_generation_transition_phase_and_layout_changes \
		transition_journal::tests::rollback_plan_requirements_are_derived_from_source_and_observation \
		transition_journal::tests::rollback_candidate_disposition_and_external_effects_are_derived \
		transition_journal::tests::rollback_recovery_order_and_status_updates_are_exact \
		transition_journal::tests::ambiguous_boot_repair_is_terminal_unverified_and_nondeletable \
		transition_journal::tests::shared_transition_id_is_the_only_journal_correlation_encoding; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-runtime-evidence-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		transition_journal::tests::runtime_epoch_capture_is_canonical_stable_and_current \
		transition_journal::tests::runtime_tree_identity_capture_binds_the_exact_directory_and_mount \
		transition_journal::tests::runtime_tree_identity_rejects_a_non_directory_descriptor \
		transition_journal::tests::boot_id_and_mount_namespace_parsers_reject_untrusted_or_noncanonical_inputs \
		transition_journal::tests::fdinfo_mount_id_parser_is_bounded_canonical_and_unique; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-journal-successor-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		transition_journal::tests::production_forward_successor_inserts_a_state_id_only_at_allocation_completion \
		transition_journal::tests::production_rollback_decision_derives_requirements_from_exact_observations \
		transition_journal::tests::production_rollback_successor_requires_one_exact_action_outcome_and_persists_unverified_boot \
		transition_journal::tests::production_rollback_successor_executes_every_pending_effect_in_fixed_order; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-journal-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^transition_journal::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 60; \
	timeout 900s $(CARGO) test -p forge --lib "transition_journal::tests::" -- --test-threads=1

stone-read-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone --all-features --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^read::test::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 20; \
	for test in \
		read::test::read_header \
		read::test::read_bash_completion \
		read::test::payload_count_limit_accepts_n_and_rejects_n_plus_one \
		read::test::record_count_limit_accepts_n_and_rejects_n_plus_one \
		read::test::record_byte_limit_accepts_n_and_rejects_n_plus_one_before_allocation \
		read::test::stored_and_plain_payload_limits_accept_n_and_reject_n_plus_one \
		read::test::aggregate_stored_plain_record_and_count_limits_are_enforced_at_n_plus_one \
		read::test::zstd_plain_size_must_match_exact_expansion \
		read::test::malformed_metadata_and_layout_lengths_are_rejected_without_panics \
		read::test::malformed_or_out_of_bounds_content_indices_are_rejected \
		read::test::huge_declared_attribute_length_fails_before_allocation \
		read::test::exact_length_strings_reject_truncation \
		read::test::record_payload_trailing_bytes_are_rejected_without_panicking \
		read::test::declared_payload_header_is_never_silently_truncated \
		read::test::multiple_content_payloads_are_rejected \
		read::test::unknown_payloads_are_skipped_with_exact_checksum_validation \
		read::test::trailing_bytes_and_truncated_payload_are_rejected \
		read::test::huge_sparse_archive_and_limit_arithmetic_fail_before_offset_seeks \
		read::test::content_output_never_exceeds_declared_plain_size \
		read::test::ffi_content_stream_is_bounded_and_validates_checksum_before_eof; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p stone --all-features --lib "$$test" -- --exact --test-threads=1; \
	done

stone-recipe-derivation-provenance-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		derivation::tests::identical_plans_have_identical_bytes_and_ids \
		derivation::tests::complete_evaluation_fingerprint_is_part_of_canonical_identity \
		derivation::tests::nested_provenance_shape_and_order_are_part_of_canonical_identity \
		derivation::tests::v2_provenance_aggregate_helpers_preserve_nested_semantics \
		derivation::tests::validation_rejects_invalid_nested_evaluation_fingerprints_at_the_exact_field \
		derivation::tests::validation_rejects_ambient_or_non_normalized_provenance_names \
		derivation::tests::validation_binds_recipe_and_profiles_to_their_locked_inputs \
		derivation::tests::validation_binds_policy_name_root_and_composition_to_the_build_lock \
		derivation::tests::validation_rejects_non_normalized_policy_origins \
		derivation::tests::validation_replays_policy_transition_state; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p stone_recipe --lib "$$test" -- --exact --test-threads=1; \
	done

stone-recipe-derivation-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^derivation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 66; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "derivation::tests::" -- --test-threads=1

stone-recipe-build-lock-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^derivation::build_lock::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "derivation::build_lock::tests::" -- --test-threads=1

stone-recipe-package-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 25; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "package::tests::" -- --test-threads=1

stone-recipe-build-policy-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^build_policy::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "build_policy::tests::" -- --test-threads=1

stone-recipe-build-policy-contract-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --test build_policy -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 32; \
	timeout 900s $(CARGO) test -p stone_recipe --test build_policy -- --test-threads=1

stone-recipe-build-policy-patch-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --test build_policy_patch -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 7; \
	timeout 900s $(CARGO) test -p stone_recipe --test build_policy_patch -- --test-threads=1

tools-buildinfo-semantic-fingerprint-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p tools_buildinfo --test semantic_fingerprint -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 29; \
	timeout 900s $(CARGO) test -p tools_buildinfo --test semantic_fingerprint -- --test-threads=1

container-cgroup-test:
	@set -eu; \
	listed="$$( timeout 120s $(CARGO) test -p container --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^cgroup::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 41; \
	timeout 300s $(CARGO) test -p container --lib "cgroup::tests::" -- --test-threads=1

# Socket-diagnostic tests remain in the complete `make test` lane. The local
# sandbox denies their `send(MSG_NOSIGNAL | MSG_DONTWAIT)` syscall with EPERM;
# this focused lane covers the two moved helpers plus every host-safe pidfd,
# signal-mask, signal-action, and lifecycle test without misreporting a skip.
container-process-runtime-test:
	@set -eu; \
	listed="$$( timeout 120s $(CARGO) test -p container --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		process_runtime::launch_support::tests::clone_stack_has_a_non_accessible_guard_and_read_write_usable_mapping \
		process_runtime::launch_support::tests::error_transport_format_is_bounded_even_for_cyclic_and_huge_sources \
		tests::pidfd_wait_and_signal_preserve_exact_terminal_statuses \
		tests::valid_pidfd_cleanup_kills_and_reaps_without_numeric_wait \
		tests::pidfd_reap_deadline_is_finite_and_leaves_authority_recoverable \
		tests::successful_cgroup_drain_retry_reaps_by_pidfd_and_restores_primary_failure \
		tests::already_reaped_pidfd_cleanup_accepts_only_the_authoritative_terminal_pair \
		tests::dropping_unrecovered_pidfd_authority_aborts_an_isolated_process \
		tests::invalid_pidfd_cleanup_never_falls_back_and_retains_authority \
		tests::signal_override_restores_the_exact_previous_action \
		tests::blocked_clone_signal_mask_restores_the_exact_previous_mask \
		tests::raw_clone_child_guard_can_retain_blocked_mask_until_exit \
		tests::signal_overrides_are_serialized_across_concurrent_runs; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 120s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
	done

container-mount-boundary-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		tests::anchored_constructor_owns_a_cloexec_opath_directory_duplicate \
		tests::anchored_constructor_rejects_every_non_opath_or_non_directory_descriptor \
		tests::anchored_bind_source_is_pinned_before_clone_and_survives_path_substitution \
		tests::anchored_mount_targets_must_preexist_and_reject_symlink_traversal \
		tests::anchored_mount_target_normalization_rejects_escape_and_root_aliases \
		tests::anchored_mount_topology_rejects_duplicate_and_nested_targets \
		tests::anchored_execution_rejects_pathname_and_special_file_bind_sources_before_clone \
		tests::anchored_bind_apis_require_absolute_source_and_guest_paths \
		tests::sealed_resolver_file_has_exact_metadata_seals_and_cleanup \
		tests::resolver_stability_witness_detects_content_metadata_change \
		tests::anchored_resolver_target_uses_the_descriptor_not_the_replaced_label \
		tests::anchored_resolver_rejects_fifo_and_device_targets_without_opening_data \
		tests::default_policy_preserves_historical_mounts \
		tests::bounded_tmpfs_limits_reject_each_zero_ceiling \
		tests::bounded_tmpfs_emits_exact_mount_and_fsconfig_values \
		tests::bounded_tmpfs_verification_reports_fstatfs_failure \
		tests::bounded_tmpfs_readback_rejects_wrong_filesystem_magic \
		tests::bounded_tmpfs_readback_rejects_representation_and_multiplication_overflow \
		tests::bounded_tmpfs_readback_reports_size_and_inode_normalization_exactly \
		tests::pseudo_mount_targets_are_prepared_before_a_root_can_be_sealed \
		tests::disabled_policy_produces_no_mount_decisions \
		tests::policy_maps_to_ordered_mount_decisions \
		tests::deterministic_loopback_policy_is_explicit \
		tests::read_only_root_reopens_only_explicit_read_write_binds \
		tests::bounded_tmpfs_on_a_read_only_root_enforces_exact_byte_and_inode_ceilings \
		tests::anchored_bounded_tmpfs_enforces_the_same_exact_ceilings \
		tests::non_page_aligned_bounded_tmpfs_is_rejected_on_path_activation \
		tests::non_page_aligned_bounded_tmpfs_is_rejected_on_anchored_activation \
		tests::minimal_dev_is_read_only_and_exact_on_the_path_activation \
		tests::minimal_dev_is_read_only_and_exact_on_anchored_activation \
		tests::read_only_root_is_enforced_by_the_live_kernel_mount_and_capability_paths \
		tests::anchored_root_path_substitution_cannot_redirect_payload \
		tests::anchored_root_relative_install_is_exact_writable_exception_after_label_substitution \
		tests::anchored_payload_error_transport_is_bounded_and_completes \
		tests::anchored_root_clone_excludes_undeclared_nested_mounts \
		tests::anchored_directory_bind_excludes_undeclared_nested_mounts \
		tests::minimal_dev_has_an_exact_non_entropy_device_set \
		tests::minimal_dev_accepts_only_exact_linux_character_device_identities \
		tests::special_file_bind_gets_a_file_mountpoint; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
	done

# The complete lane intentionally retains five socket-diagnostic tests which
# this local sandbox denies with EPERM. Run every other direct root test here,
# one-by-one, so the extraction is still covered without converting a denial
# into a false skip or weakening the tests themselves.
container-root-host-safe-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 64; \
	ran=0; \
	for test in $$( timeout 10s grep '^tests::.*: test$$' <<<"$$listed" | timeout 10s sed 's/: test$$//' ); do \
		case "$$test" in \
			tests::child_error_read_does_not_wait_for_a_leaked_descendant_socket|\
			tests::raw_clone_child_panic_is_contained_and_reported|\
			tests::synchronization_socket_blocks_child_until_one_atomic_release|\
			tests::synchronization_socket_is_close_on_exec_blocking_and_nosignal|\
			tests::synchronization_socket_preserves_the_maximum_diagnostic_packet) continue ;; \
		esac; \
		timeout 180s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
		ran=$$((ran + 1)); \
	done; \
	timeout 10s test "$$ran" = 59

mason-package-collect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::collect::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 27; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::tests::" -- --test-threads=1

mason-package-collect-transaction-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	mutation_count="$$( timeout 10s grep -c '^package::collect::mutation::tests::.*: test$$' <<<"$$listed" )"; \
	publication_count="$$( timeout 10s grep -c '^package::collect::publication::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$mutation_count" = 11; \
	timeout 10s test "$$publication_count" = 11; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::mutation::tests::" -- --test-threads=1; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::publication::tests::" -- --test-threads=1

mason-analysis-handler-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::analysis::handler::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 30; \
	timeout 900s $(CARGO) test -p mason --lib "package::analysis::handler::tests::" -- --test-threads=1

mason-emit-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^package::emit::verification_tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 18; \
	for test in \
		package::emit::verification_tests::artifact_sink_publishes_only_the_exact_read_only_set \
		package::emit::verification_tests::real_contentful_stone_emission_survives_transactional_staging \
		package::emit::verification_tests::bounded_artifact_failure_removes_every_owned_name \
		package::emit::verification_tests::bounded_artifact_seek_accepts_exact_limit_and_rejects_limit_plus_one \
		package::emit::verification_tests::publication_collision_after_one_rename_rolls_back_owned_final \
		package::emit::verification_tests::staged_same_size_mutation_immediately_before_rename_is_rejected \
		package::emit::verification_tests::final_inode_swap_is_detected_without_deleting_the_replacement \
		package::emit::verification_tests::same_inode_truncation_after_publication_is_detected_and_removed \
		package::emit::verification_tests::same_inode_same_size_overwrite_after_publication_is_detected_and_removed \
		package::emit::verification_tests::replaced_public_root_is_rejected_and_only_the_pinned_root_is_cleaned \
		package::emit::verification_tests::preexisting_artifact_root_entries_are_never_reused_or_removed \
		package::emit::verification_tests::emitter_rejects_a_path_replaced_after_collection \
		package::emit::verification_tests::duplicate_normalized_layout_targets_are_rejected_before_emission \
		package::emit::verification_tests::reserved_system_metadata_target_is_rejected_before_artifact_sink_creation \
		package::emit::verification_tests::near_system_metadata_names_remain_legal_for_mason_layouts \
		package::emit::verification_tests::non_directory_normalized_ancestor_is_rejected_before_emission \
		package::emit::verification_tests::directory_normalized_ancestor_may_own_descendants \
		package::emit::verification_tests::content_emission_preserves_the_primary_writer_error; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

mason-archive-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^archive::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 24; \
	timeout 900s $(CARGO) test -p mason --lib "archive::tests::" -- --test-threads=1

mason-package-publication-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^package::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 42; \
	timeout 900s $(CARGO) test -p mason --lib "package::tests::" -- --test-threads=1

mason-git-materialization-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^upstream::git::materialization::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 20; \
	timeout 900s $(CARGO) test -p mason --lib "upstream::git::materialization::tests::" -- --test-threads=1

mason-paths-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^paths::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 25; \
	timeout 900s $(CARGO) test -p mason --lib "paths::tests::" -- --test-threads=1

mason-executor-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^executor::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 25; \
	timeout 900s $(CARGO) test -p mason --lib "executor::tests::" -- --test-threads=1

mason-build-context-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^build::context::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 19; \
	timeout 900s $(CARGO) test -p mason --lib "build::context::tests::" -- --test-threads=1

mason-recipe-explanation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^cli::recipe::explanation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 2; \
	timeout 900s $(CARGO) test -p mason --lib "cli::recipe::explanation::tests::" -- --test-threads=1

mason-upstream-git-cache-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^upstream::git::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
	timeout 900s $(CARGO) test -p mason --lib "upstream::git::tests::" -- --test-threads=1

mason-build-root-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^build::root::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	timeout 900s $(CARGO) test -p mason --lib "build::root::tests::" -- --test-threads=1

mason-profile-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^profile::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	timeout 900s $(CARGO) test -p mason --lib "profile::tests::" -- --test-threads=1

mason-planner-bootstrap-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^planner::hermetic_tests::bootstrap::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	timeout 1200s $(CARGO) test -p mason --lib "planner::hermetic_tests::bootstrap::" -- --test-threads=1

mason-policy-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^policy::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	timeout 900s $(CARGO) test -p mason --lib "policy::tests::" -- --test-threads=1

config-gluon-store-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p config --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^gluon::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 28; \
	timeout 900s $(CARGO) test -p config --lib "gluon::tests::" -- --test-threads=1

gitwrap-repository-fs-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p gitwrap --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		tests::repository_limits_accept_exact_n_and_reject_n_plus_one \
		tests::strict_entry_quota_rejects_n_plus_one_without_sampling_slack \
		tests::live_scan_may_retry_a_vanished_name_but_strict_scan_fails_closed \
		tests::live_scan_allows_initial_absence_without_building_a_strict_inventory \
		tests::strict_relative_path_allocation_is_prechecked_against_snapshot_budget \
		tests::strict_two_snapshot_verification_rejects_same_name_inode_replacement \
		tests::descriptor_rooted_quota_scan_never_follows_nested_or_root_symlinks \
		tests::quota_scan_rejects_nesting_before_exhausting_parent_descriptors \
		tests::quota_scan_rejects_a_budget_too_small_for_one_cursor \
		tests::quota_scanner_reserves_descriptors_already_open_in_the_parent \
		tests::repository_rejects_a_replaced_public_path_while_root_is_pinned \
		tests::quota_scan_uses_the_subprocess_absolute_deadline \
		tests::oversized_cached_mirror_is_rejected_before_git_is_started \
		tests::remote_url_mutation_is_rejected_when_it_crosses_repository_quota \
		tests::failed_public_fetch_never_deletes_a_caller_owned_repository \
		tests::incremental_quota_scan_does_not_starve_a_full_stdout_pipe \
		tests::oversized_clone_is_rejected_without_final_or_staging_state \
		tests::published_mirror_and_credential_config_are_owner_private \
		tests::private_mirror_strips_hostile_local_config_before_open_and_fetch \
		tests::private_mirror_origin_is_checked_before_config_is_rewritten \
		tests::sha256_object_format_commit_ids_are_accepted; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p gitwrap --lib "$$test" -- --exact --test-threads=1; \
	done

gitwrap-all-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p gitwrap --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 32; \
	timeout 900s $(CARGO) test -p gitwrap --lib "tests::" -- --test-threads=1

forge-repository-manager-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^repository::manager::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 19; \
	timeout 900s $(CARGO) test -p forge --lib "repository::manager::tests::" -- --test-threads=1

forge-security-fixture-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		cli::repo::tests::authored_system_intent_rejects_imperative_repository_changes \
		client::tests::state_creation_records_and_exports_the_generated_snapshot \
		installation::tests::both_open_modes_defer_invalid_system_intent_but_frozen_skips_active_state \
		tree_marker::tests::canonical_marker_rejects_links_wrong_kinds_and_mutable_modes; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done
