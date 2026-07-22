ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-terminal-promotion-test

forge-active-reblit-boot-terminal-promotion-test: forge-active-reblit-boot-immutable-publication-attempt-test forge-boot-publication-receipt-promotion-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/target/active-reblit-boot-terminal-promotion-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::active_reblit_boot_publication_preflight::immutable_attempt::tests::receipt_promotion::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 20; \
	for name in \
		admission::wrong_client_is_rejected_before_fresh_namespace_admission \
		admission::inherited_deadline_expiry_fails_without_receipt_promotion \
		admission::deadline_expiry_after_staged_revalidation_fails_without_receipt_promotion \
		admission::missing_terminal_leaf_fails_closed_before_receipt_promotion \
		database_reporting::ambiguous_commit_report_preserves_promoted_classification_without_success_authority \
		fail_stop::leaf_drift_between_terminal_checks_fails_before_database_promotion \
		fail_stop::leaf_drift_after_database_success_returns_outcome_but_no_success_authority \
		fail_stop::final_validation_catches_late_leaf_drift_after_promoted_revalidation \
		fail_stop::post_promotion_same_bytes_different_journal_inode_fails_stop \
		last_boundary::leaf_drift_after_last_revalidation_promotes_but_returns_no_authority \
		last_boundary::collision_drift_after_last_revalidation_is_detected_after_durable_promotion \
		last_boundary::target_attachment_identity_drift_after_last_revalidation_is_detected_after_promotion \
		last_boundary::journal_inode_substitution_after_last_revalidation_fails_stop_after_promotion \
		pre_promotion_integrity::cleared_pending_head_after_initial_admission_fails_before_promotion \
		pre_promotion_integrity::missing_pending_body_after_initial_admission_fails_before_promotion \
		pre_promotion_integrity::same_bytes_different_journal_inode_before_staged_revalidation_fails_before_promotion \
		success::all_already_exact_terminal_evidence_promotes_without_republishing \
		success::chained_predecessor_terminal_receipt_promotes_and_retains_the_chain \
		success::mixed_terminal_evidence_promotes_once_and_preserves_journal_and_counters \
		success::exact_already_promoted_receipt_is_adopted_without_journal_change; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	attempt="$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	bridge="$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion.rs"; \
	staging="$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging/promoted_receipt_validation.rs"; \
	promotion="$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/promotion.rs"; \
	forge_root="$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/crates/forge/src"; \
	grep -Fq 'pub(in crate::client) fn promote_terminal_receipt(' "$$bridge"; \
	grep -Fq '.promote_boot_publication_receipt(self.staged.receipt(), plan.input_deadline())' "$$bridge"; \
	test "$$( grep -Fc '.promote_boot_publication_receipt(' "$$bridge" )" = 1; \
	grep -Fq 'pub(crate) fn promote_boot_publication_receipt(' "$$promotion"; \
	promotion_mentions="$$( rg -n -o '\bpromote_boot_publication_receipt\b' "$$promotion" )"; \
	test "$$( grep -c . <<<"$$promotion_mentions" )" = 1; \
	production_mentions="$$( rg -n -o '\bpromote_boot_publication_receipt\b' "$$forge_root" \
		--glob '*.rs' \
		--glob '!**/tests/**' \
		--glob '!**/tests.rs' \
		--glob '!**/*_tests.rs' \
		--glob '!**/*_test.rs' \
		--glob '!**/test_support.rs' \
		--glob '!**/*_test_support.rs' \
		--glob '!**/fixtures/**' \
		--glob '!**/fixtures.rs' \
		--glob '!**/*_fixtures.rs' \
		--glob '!**/*_fixture.rs' \
		--glob '!**/fixture_support.rs' \
		--glob '!**/*_fixture_support.rs' )"; \
	test "$$( grep -c . <<<"$$production_mentions" )" = 2; \
	test "$$( grep -Fc "$$promotion:" <<<"$$production_mentions" )" = 1; \
	test "$$( grep -Fc "$$bridge:" <<<"$$production_mentions" )" = 1; \
	grep -Fq '#[must_use = "terminal boot-publication evidence must be promoted or deliberately discarded"]' "$$attempt"; \
	grep -Fq '#[must_use = "promoted boot-publication authority must be durably completed or deliberately discarded"]' "$$bridge"; \
	grep -Fq 'before_fresh_admission();' "$$bridge"; \
	grep -Fq 'before_immediate_pre_promotion_terminal_check();' "$$bridge"; \
	grep -Fq 'after_pre_promotion_revalidation();' "$$bridge"; \
	grep -Fq 'after_database_promotion();' "$$bridge"; \
	grep -Fq 'before_final_promoted_validation();' "$$bridge"; \
	grep -Fq '.revalidate_promoted_against(client)' "$$bridge"; \
	grep -Fq '.require_promoted_boot_publication_receipt(&self.receipt)' "$$staging"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:StagedExact|PromotedExact)ActiveReblitBootPublication|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:StagedExact|PromotedExact)ActiveReblitBootPublication' "$$attempt" "$$bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'Phase::BootSyncComplete|forward_successor|advance_record_binding|journal[.]advance|publish_preflighted_immutable_leaf|renameat|unlinkat|remove_(?:file|dir)|delete_|Command::new|nix::mount|libc::mount' "$$bridge" "$$staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in \
		"$$attempt" \
		"$$bridge" \
		"$$staging" \
		"$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests.rs \
		"$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests/*.rs \
		"$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/misc/make/active-reblit-boot-terminal-promotion-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_TERMINAL_PROMOTION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1 --include-ignored
