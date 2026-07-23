BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-boot-publication-receipt-promotion-test

forge-boot-publication-receipt-promotion-test: forge-boot-publication-receipt-state-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipts::promotion::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 20; \
	for name in \
		deadline::deadline_expiring_after_exclusive_admission_blocks_head_mutation \
		deadline::deadline_equality_at_exclusive_mutation_boundary_allows_promotion \
		api::first_pending_receipt_promotes_atomically_and_retains_its_body \
		api::chained_promotion_preserves_predecessor_and_successor_bodies \
		api::exact_promoted_retry_is_read_only_even_when_updates_are_rejected \
		api::exact_promoted_retry_uses_no_exclusive_lock_against_an_independent_reader \
		api::stale_replay_cannot_displace_a_pending_or_promoted_successor \
		api::promoted_head_and_body_survive_a_real_database_reopen \
		failures::missing_foreign_and_conflicting_preimages_fail_without_mutation \
		failures::conditional_and_terminal_races_roll_back_to_the_exact_pending_state \
		failures::well_formed_nonterminal_revalidation_rolls_back_instead_of_committing \
		failures::genuine_commit_failure_reconciles_the_rolled_back_pending_state \
		failures::storage_failure_rolls_back_and_ambiguous_success_is_exactly_classified \
		corruption::dangling_and_tampered_pending_bodies_fail_before_head_mutation \
		corruption::dangling_and_tampered_committed_predecessors_block_promotion \
		corruption::terminal_validation_and_exact_retry_require_the_committed_predecessor_body \
		promoted_validation::read_only_validator_rejects_pending_then_accepts_exact_promoted_without_writes_or_exclusive_lock \
		promoted_validation::read_only_validator_requires_the_retained_committed_predecessor_body \
		promoted_validation::exact_startup_query_rejects_empty_pending_and_wrong_identity_without_mutation \
		promoted_validation::exact_startup_query_rejects_a_corrupt_retained_predecessor_without_mutation; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	promotion="$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/promotion.rs"; \
	head="$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipt_head.rs"; \
	state="$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts.rs"; \
	state_mod="$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/crates/forge/src/db/state/mod.rs"; \
	validator="$$( sed -n '/pub(crate) fn require_promoted_boot_publication_receipt(/,/    \/\/\/ Atomically make one exact pending/p' "$$promotion" )"; \
	grep -Fq 'pub(crate) fn promote_boot_publication_receipt(' "$$promotion"; \
	grep -Fq '        deadline: Instant,' "$$promotion"; \
	grep -Fq 'pub(crate) fn require_promoted_boot_publication_receipt(' "$$promotion"; \
	grep -Fq 'connection.transaction(|connection| inspect_exact_state(connection, receipt))' "$$promotion"; \
	grep -Fq 'self.conn.exclusive_tx(|connection|' "$$promotion"; \
	grep -Fq 'let outcome = promote_receipt(connection, receipt, deadline)?;' "$$promotion"; \
	grep -Fq 'let state = load_receipt_state(connection)?;' "$$promotion"; \
	grep -Fq 'let after = load_receipt_state(connection)?;' "$$promotion"; \
	grep -Fq 'pub(super) fn promote_pending_row(' "$$head"; \
	grep -Fq 'promote_pending_row(connection, receipt.body().transition_id(), &pair)?' "$$promotion"; \
	boundary_line="$$( grep -n '^    before_head_update(connection);$$' "$$promotion" | cut -d: -f1 )"; \
	deadline_line="$$( grep -n '^    require_promotion_deadline(deadline)?;$$' "$$promotion" | cut -d: -f1 )"; \
	mutation_line="$$( grep -n '^    let changed = promote_pending_row(connection, receipt.body().transition_id(), &pair)?;$$' "$$promotion" | cut -d: -f1 )"; \
	test "$$((boundary_line + 1))" = "$$deadline_line"; \
	test "$$((deadline_line + 1))" = "$$mutation_line"; \
	grep -Fq 'DeadlineExceeded { deadline: Instant }' "$$promotion"; \
	grep -Fq 'BootPublicationReceiptPromotionOutcome::AlreadyPromoted' "$$promotion"; \
	grep -Fq 'BootPublicationReceiptPromotionDurableState::Pending' "$$promotion"; \
	grep -Fq 'BootPublicationReceiptPromotionDurableState::Promoted' "$$promotion"; \
	grep -Fq 'connection.transaction(|connection| inspect_exact_state(connection, receipt))' <<<"$$validator"; \
	grep -Fq 'BootPublicationReceiptPromotionDurableState::Pending' <<<"$$validator"; \
	grep -Fq '#[path = "boot_publication_receipts/promotion.rs"]' "$$state"; \
	grep -Fq 'BootPublicationReceiptPromotionOutcome,' "$$state_mod"; \
	grep -Fq 'pub(crate) fn arm_boot_publication_receipt_promotion_after_commit_error(' "$$promotion"; \
	grep -Fq 'pub(crate) use promotion::arm_boot_publication_receipt_promotion_after_commit_error;' "$$state"; \
	grep -Fq 'pub(crate) use boot_publication_receipts::arm_boot_publication_receipt_promotion_after_commit_error;' "$$state_mod"; \
	helper="$$( sed -n '/^pub(super) fn promote_pending_row(/,/^#\[derive(Queryable)\]/p' "$$head" )"; \
	if rg -n 'boot_publication_receipts::table|diesel::(insert_into|delete)|transition_journal|linux_fs|BootSyncComplete|publish_immutable|renameat|unlinkat|remove_|delete_' "$$promotion"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	if rg -n 'exclusive_tx|promote_receipt|promote_pending_row|diesel::(insert_into|update|delete)|\b(INSERT|UPDATE|DELETE)\b' <<<"$$validator"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	if rg -n 'boot_publication_receipts::table|diesel::(insert_into|delete)|transition_journal|linux_fs|BootSyncComplete|publish_immutable|renameat|unlinkat|remove_|delete_' <<<"$$helper"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	for file in \
		"$$promotion" \
		"$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)"/crates/forge/src/db/state/boot_publication_receipts/promotion/tests.rs \
		"$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)"/crates/forge/src/db/state/boot_publication_receipts/promotion/tests/*.rs \
		"$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/misc/make/boot-publication-receipt-promotion-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_PROMOTION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1 --include-ignored
