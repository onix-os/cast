ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-immutable-publication-attempt-test

forge-active-reblit-boot-immutable-publication-attempt-test: host-storage-safety-test forge-active-reblit-boot-publication-preflight-test forge-active-reblit-installed-boot-publication-delta-test forge-active-reblit-boot-owned-replacement-bridge-test forge-boot-publication-receipt-state-test forge-linux-descriptor-boot-publication-parent-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/target/active-reblit-boot-immutable-attempt-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::active_reblit_boot_publication_preflight::immutable_attempt::tests::'; \
	restart_prefix='client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_cleanup::restart::tests::'; \
	test "$$( grep -Ec "^$$prefix(routing|integration|durable_state|failures|installed_replacement)::.*: test$$" "$$listed" )" = 9; \
	test "$$( grep -Ec "^$$restart_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		routing::alias_and_distinct_routes_preserve_global_plan_order \
		integration::staged_alias_attempt_publishes_in_phase_order_and_terminally_observes_exact \
		durable_state::pre_effect_journal_identity_drift_fails_before_any_publication \
		durable_state::sealed_classification_drift_fails_before_effect_authority_exists \
		durable_state::immediate_post_schedule_journal_drift_fails_before_any_effect \
		installed_replacement::authentic_installed_delta_executes_desired_actions_and_promoted_cleanup \
		installed_replacement::missing_owned_replacement_sidecar_blocks_receipt_promotion \
		installed_replacement::same_bytes_different_owned_replacement_sidecar_inode_blocks_receipt_promotion \
		failures::leaf_failure_stops_before_later_outputs_and_retains_pending_started; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	for name in \
		exact_receipt_plan_live_alias_targets_and_startup_seal_drive_restart_cleanup \
		receipt_replacement_reconstructs_fresh_authority_then_is_already_clean \
		receipt_stale_cleanup_reconciles_canonical_detached_and_already_clean \
		restart_admission_refuses_receipt_drift_noop_and_unowned_preserve \
		component_process_kill::owned_cleanup_components_process_kills_recover_exactly; do \
		grep -Fqx "$$restart_prefix$$name: test" "$$listed"; \
	done; \
	staging="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging/immutable_publication_attempt.rs"; \
	attempt="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	execution="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/execution_schedule.rs"; \
	evidence="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/effect_evidence.rs"; \
	leaf="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/immutable_leaf.rs"; \
	replacement="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/owned_replacement.rs"; \
	promoted_validation="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging/promoted_receipt_validation.rs"; \
	cleanup="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/promoted_cleanup.rs"; \
	cleanup_bridge="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/owned_cleanup.rs"; \
	cleanup_fixture="$${cleanup_bridge%.rs}/fixture.rs"; \
	restart_cleanup="$${cleanup_bridge%.rs}/restart.rs"; \
	restart_cleanup_tests="$${cleanup_bridge%.rs}/restart_tests.rs"; \
	owned_cleanup_process_kill="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_owned_cleanup_component_process_kill_tests.rs"; \
	mount_namespace="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace.rs"; \
	attachment="$${mount_namespace%.rs}/attachment.rs"; \
	replacement_module="$${attachment%.rs}/boot_file_replacement.rs"; \
	replacement_effect="$${replacement_module%.rs}/effect.rs"; \
	tests="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/tests"; \
	installed_replacement="$$tests/installed_replacement.rs"; \
	forge_root="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src"; \
	grep -Fq 'pub(in crate::client) fn attempt_immutable_boot_publication(' "$$staging"; \
	grep -Fq '.prepare_effect_schedule(retained_plan)' "$$attempt"; \
	grep -Fq 'let mut evidence = prepare_execution_schedule(&preflight, &schedule)?;' "$$attempt"; \
	grep -Fq 'after_pre_effect_schedule_validation();' "$$attempt"; \
	grep -Fq '.publish_preflighted_immutable_leaf(' "$$attempt"; \
	grep -Fq '.replace_preflighted_owned_leaf(' "$$attempt"; \
	grep -Fq 'let terminal_states = terminal_namespace_assessment(&preflight)?;' "$$attempt"; \
	grep -Fq '.revalidate_publication_targets()' "$$attempt"; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired' "$$execution"; \
	grep -Fq 'ReplacedOwned {' "$$evidence"; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired' "$$installed_replacement"; \
	grep -Fq 'terminal.replaced_count()' "$$installed_replacement"; \
	grep -Fq 'terminal.promoted_cleanup_required()' "$$installed_replacement"; \
	grep -Fq 'evidence.owner_matches_receipt(pending_fingerprint)' "$$installed_replacement"; \
	grep -Fq 'authority.sidecar_leaf()' "$$installed_replacement"; \
	grep -Fq 'let promoted = match promoted.try_into_cleaned()' "$$installed_replacement"; \
	grep -Fq 'rejected cleanup conversion retains replacement authority' "$$installed_replacement"; \
	grep -Fq 'PromotionPairCase::MissingRollbackSidecar' "$$installed_replacement"; \
	grep -Fq 'PromotionPairCase::SameBytesDifferentSidecarInode' "$$installed_replacement"; \
	grep -Fq 'checkpoint: "initial terminal admission"' "$$installed_replacement"; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale' "$$installed_replacement"; \
	grep -Fq 'assert!(!owned_stale.exists())' "$$installed_replacement"; \
	grep -Fq 'fs::metadata(&borrowed_stale).unwrap().ino()' "$$installed_replacement"; \
	grep -Fq 'ActiveReblitBootImmutableLeafPublicationError::DestinationStateChanged' "$$leaf"; \
	grep -Fq '.replace_exact_boot_file_until(' "$$replacement"; \
	grep -Fq 'pub(in crate::client) const fn classified_delta(' "$$promoted_validation"; \
	rg --pcre2 -U -q 'pub\(in crate::client\) fn cleanup_promoted_outputs\(\s*mut self,\s*client: &Client' "$$cleanup"; \
	grep -Fq '.revalidate_promoted_against(client)' "$$cleanup"; \
	grep -Fq '.revalidate_publication_targets()' "$$cleanup"; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion' "$$cleanup"; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale' "$$cleanup"; \
	grep -Fq '.reconcile_and_cleanup_promoted_owned_replacement(' "$$cleanup"; \
	grep -Fq '.reconcile_and_cleanup_promoted_owned_stale(' "$$cleanup"; \
	grep -Fq 'self.terminal.promoted_cleanup_required = false;' "$$cleanup"; \
	grep -Fq '.reconcile_replaced_boot_file_sidecar_cleanup_until(' "$$cleanup_bridge"; \
	grep -Fq 'if &recovered != historical {' "$$cleanup_bridge"; \
	grep -Fq '.cleanup_replaced_boot_file_sidecar_until(recovered, deadline)' "$$cleanup_bridge"; \
	grep -Fq '.reconcile_stale_boot_file_cleanup_until(' "$$cleanup_bridge"; \
	grep -Fq '.cleanup_authenticated_stale_boot_file_until(recovered, deadline)' "$$cleanup_bridge"; \
	grep -Fq 'impl ReceiptValidatedActiveReblitBootPublicationTargets' "$$restart_cleanup"; \
	grep -Fq 'fn reconcile_and_cleanup_restart_receipt_entry(' "$$restart_cleanup"; \
	grep -Fq 'cleanup_seal: &ActiveReblitBootSyncStartedCleanupSeal' "$$restart_cleanup"; \
	grep -Fq 'cleanup_seal.promoted_receipt()' "$$restart_cleanup"; \
	grep -Fq 'plan.promoted_receipt()' "$$restart_cleanup"; \
	grep -Fq 'validated.aliases_esp' "$$restart_cleanup"; \
	grep -Fq 'BootPublicationRoot::Esp | BootPublicationRoot::Boot' "$$restart_cleanup"; \
	grep -Fq 'BootTargetRole::Xbootldr' "$$restart_cleanup"; \
	for disposition in ReplaceOwned DeleteOwnedStale NoOp PreserveUnownedStale; do \
		grep -Fq "ActiveReblitPromotedBootCleanupDisposition::$$disposition" "$$restart_cleanup"; \
	done; \
	grep -Fq 'output.length()' "$$restart_cleanup"; \
	grep -Fq 'output.xxh3().as_u128()' "$$restart_cleanup"; \
	grep -Fq '*output.content_sha256().as_bytes()' "$$restart_cleanup"; \
	grep -Fq '.reconcile_replaced_boot_file_sidecar_cleanup_until(' "$$restart_cleanup"; \
	grep -Fq 'RetainedBootFileAppliedSidecarCleanupState::Pending(recovered)' "$$restart_cleanup"; \
	grep -Fq '.cleanup_replaced_boot_file_sidecar_until(recovered, deadline)' "$$restart_cleanup"; \
	grep -Fq '.reconcile_stale_boot_file_cleanup_until(' "$$restart_cleanup"; \
	grep -Fq 'RetainedBootFileStaleCleanupState::Detached(recovered)' "$$restart_cleanup"; \
	grep -Fq '.cleanup_authenticated_stale_boot_file_until(recovered, deadline)' "$$restart_cleanup"; \
	grep -Fq '#[path = "../../../../active_reblit_owned_cleanup_component_process_kill_tests.rs"]' "$$restart_cleanup_tests"; \
	grep -Fq 'low-level owned-cleanup components' "$$owned_cleanup_process_kill"; \
	grep -Fq 'not exercise receipt validation or a production startup entry point.' "$$owned_cleanup_process_kill"; \
	grep -Fq 'const ALL: [Self; 3]' "$$owned_cleanup_process_kill"; \
	grep -Fq 'assert_eq!(cases, 3, "owned-cleanup SIGKILL matrix must remain exact")' "$$owned_cleanup_process_kill"; \
	grep -Fq 'Command::new(env::current_exe().unwrap())' "$$owned_cleanup_process_kill"; \
	grep -Fq 'Some(nix::libc::SIGKILL)' "$$owned_cleanup_process_kill"; \
	grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$owned_cleanup_process_kill"; \
	grep -Fq 'reconcile_restart_replacement_with_attachment(' "$$owned_cleanup_process_kill"; \
	grep -Fq 'reconcile_restart_stale_with_attachment(' "$$owned_cleanup_process_kill"; \
	grep -Fq 'ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale' "$$owned_cleanup_process_kill"; \
	grep -Fq 'ActiveReblitBootOwnedCleanupOutcome::AlreadyClean' "$$owned_cleanup_process_kill"; \
	grep -Fq 'root.starts_with(&target)' "$$owned_cleanup_process_kill"; \
	grep -Fq 'const NONCE_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_NONCE";' "$$owned_cleanup_process_kill"; \
	grep -Fq '.rand_bytes(CONTROL_NONCE_LENGTH)' "$$owned_cleanup_process_kill"; \
	grep -Fq '.env(NONCE_ENV, fixture.control.nonce())' "$$owned_cleanup_process_kill"; \
	grep -Fq 'for name in [ROLE_ENV, CASE_ENV, ROOT_ENV, RESIDUE_ENV, NONCE_ENV]' "$$owned_cleanup_process_kill"; \
	grep -Fq 'struct ParentControl {' "$$owned_cleanup_process_kill"; \
	grep -Fq 'file: tempfile::NamedTempFile' "$$owned_cleanup_process_kill"; \
	grep -Fq 'assert_boot_root_scaffold(&case.root, &control_leaf);' "$$owned_cleanup_process_kill"; \
	grep -Fq 'fixture.cleanup();' "$$owned_cleanup_process_kill"; \
	rg --pcre2 -U -q 'fn run_child\(case: ChildCase\) \{\s*verify_child_fixture\(&case\);\s*let anchor = PreparedMountNamespaceAnchor::prepare\(\)' "$$owned_cleanup_process_kill"; \
	grep -Fq 'arm_after_stale_boot_file_detach_callback' "$$replacement_effect"; \
	grep -Fq 'detached stale inode plus its parent and' "$$replacement_effect"; \
	grep -Fq 'filesystem have been synchronized' "$$replacement_effect"; \
	grep -Fq 'arm_after_boot_file_sidecar_unlink_callback' "$$replacement_effect"; \
	grep -Fq 'effect::after_stale_detach();' "$$replacement_module"; \
	test "$$( grep -Fc 'effect::after_sidecar_unlink();' "$$replacement_module" )" = 2; \
	for exported in arm_after_stale_boot_file_detach_callback arm_after_boot_file_sidecar_unlink_callback; do \
		grep -Fq "$$exported" "$$replacement_module"; \
		grep -Fq "$$exported" "$$attachment"; \
		grep -Fq "$$exported" "$$mount_namespace"; \
	done; \
	if rg -n 'nix::mount|libc::mount|setns|unshare|/dev/' "$$owned_cleanup_process_kill"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	seal_mentions="$$( rg -n -o 'ActiveReblitBootPromotedCleanupSeal::new\(' "$$forge_root" --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_test.rs' --glob '!**/test_support.rs' --glob '!**/*_test_support.rs' --glob '!**/fixtures/**' --glob '!**/fixtures.rs' --glob '!**/*_fixtures.rs' --glob '!**/*_fixture.rs' --glob '!**/fixture_support.rs' --glob '!**/*_fixture_support.rs' )"; \
	test "$$( grep -c . <<<"$$seal_mentions" )" = 1; \
	grep -Fq "$$cleanup:" <<<"$$seal_mentions"; \
	clean_marks="$$( rg -n -o 'promoted_cleanup_required = false' "$$forge_root" --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_test.rs' --glob '!**/test_support.rs' --glob '!**/*_test_support.rs' --glob '!**/fixtures/**' --glob '!**/fixtures.rs' --glob '!**/*_fixtures.rs' --glob '!**/*_fixture.rs' --glob '!**/fixture_support.rs' --glob '!**/*_fixture_support.rs' )"; \
	test "$$( grep -c . <<<"$$clean_marks" )" = 1; \
	grep -Fq "$$cleanup:" <<<"$$clean_marks"; \
	if rg -n 'Phase::BootSyncComplete|forward_successor|advance_record_binding|promote_boot_publication|clear_boot_publication|delete_boot_publication' "$$staging" "$$attempt" "$$execution" "$$evidence" "$$leaf"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'Phase::BootSyncComplete|boot_sync_complete_successor|advance_record_binding|promote_boot_publication_receipt|clear_boot_publication|delete_boot_publication|Command::new|nix::mount|libc::mount|[.]remove_file|[.]rename|unlinkat|renameat' "$$cleanup" "$$cleanup_bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'historical_runtime_witness|ValidatedRetainedBootFileReplacement|ActiveReblitBootPromotedCleanupSeal|diesel::|SqliteConnection|TransitionJournal|std::fs|fs_err|Command::new|nix::mount|libc::mount' "$$restart_cleanup"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct StagedExactActiveReblitBootPublication|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+StagedExactActiveReblitBootPublication' "$$attempt"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$staging" "$$attempt" "$$execution" "$$evidence" "$$leaf" "$$replacement" "$$promoted_validation" "$$cleanup" "$$cleanup_bridge" "$$cleanup_fixture" "$$restart_cleanup" "$$restart_cleanup_tests" "$$owned_cleanup_process_kill" "$$mount_namespace" "$$attachment" "$$replacement_module" "$$replacement_effect" "$$tests.rs" "$$tests"/*.rs "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/misc/make/active-reblit-boot-immutable-publication-attempt-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$restart_prefix" -- --test-threads=1
