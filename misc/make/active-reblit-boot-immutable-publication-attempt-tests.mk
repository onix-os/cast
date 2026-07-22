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
	test "$$( grep -Ec "^$$prefix(routing|integration|durable_state|failures|installed_replacement)::.*: test$$" "$$listed" )" = 9; \
	for name in \
		routing::alias_and_distinct_routes_preserve_global_plan_order \
		integration::staged_alias_attempt_publishes_in_phase_order_and_terminally_observes_exact \
		durable_state::pre_effect_journal_identity_drift_fails_before_any_publication \
		durable_state::sealed_classification_drift_fails_before_effect_authority_exists \
		durable_state::immediate_post_schedule_journal_drift_fails_before_any_effect \
		installed_replacement::authentic_installed_delta_executes_all_desired_actions_and_defers_cleanup \
		installed_replacement::missing_owned_replacement_sidecar_blocks_receipt_promotion \
		installed_replacement::same_bytes_different_owned_replacement_sidecar_inode_blocks_receipt_promotion \
		failures::leaf_failure_stops_before_later_outputs_and_retains_pending_started; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	staging="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging/immutable_publication_attempt.rs"; \
	attempt="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	execution="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/execution_schedule.rs"; \
	evidence="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/effect_evidence.rs"; \
	leaf="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/immutable_leaf.rs"; \
	replacement="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/owned_replacement.rs"; \
	tests="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/tests"; \
	installed_replacement="$$tests/installed_replacement.rs"; \
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
	grep -Fq 'ActiveReblitBootImmutableLeafPublicationError::DestinationStateChanged' "$$leaf"; \
	grep -Fq '.replace_exact_boot_file_until(' "$$replacement"; \
	if rg -n 'Phase::BootSyncComplete|forward_successor|advance_record_binding|promote_boot_publication|clear_boot_publication|delete_boot_publication' "$$staging" "$$attempt" "$$execution" "$$evidence" "$$leaf"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct StagedExactActiveReblitBootPublication|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+StagedExactActiveReblitBootPublication' "$$attempt"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$staging" "$$attempt" "$$execution" "$$evidence" "$$leaf" "$$replacement" "$$tests.rs" "$$tests"/*.rs "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/misc/make/active-reblit-boot-immutable-publication-attempt-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
