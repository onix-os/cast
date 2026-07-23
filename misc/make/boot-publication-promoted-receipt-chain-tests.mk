BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-boot-publication-promoted-receipt-chain-test

forge-boot-publication-promoted-receipt-chain-test: forge-boot-publication-receipt-promotion-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipts::exact_promoted_receipt_chain_tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 9; \
	for name in \
		current_chain_admits_only_a_strictly_empty_database \
		current_chain_derives_the_installed_identity_and_predecessor_from_storage \
		current_chain_rejects_pending_state_without_caller_correlation \
		current_chain_rejects_dangling_and_mismatched_immutable_bodies \
		exact_chain_loads_a_promoted_installed_receipt_without_a_predecessor \
		exact_chain_returns_both_immutable_canonical_receipts \
		exact_chain_rejects_pending_and_mismatched_compact_correlations \
		exact_chain_requires_the_named_predecessor_body_without_mutation \
		current_chain_loader_is_read_only_and_uses_no_exclusive_lock; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	state="$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts.rs"; \
	promotion="$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/promotion.rs"; \
	chain="$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/exact_promoted_receipt_chain.rs"; \
	tests="$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/exact_promoted_receipt_chain_tests.rs"; \
	grep -Fq '#[path = "boot_publication_receipts/exact_promoted_receipt_chain.rs"]' "$$state"; \
	grep -Fq 'pub(crate) struct ExactPromotedBootPublicationReceiptChain' "$$chain"; \
	grep -Fq 'pub(crate) enum CurrentExactPromotedBootPublicationReceiptChain' "$$chain"; \
	grep -Fq 'pub(crate) fn load_current_exact_promoted_boot_publication_receipt_chain(' "$$chain"; \
	grep -Fq 'let transition_id = installed.body().transition_id().clone();' "$$chain"; \
	grep -Fq 'pending: installed.fingerprint(),' "$$chain"; \
	grep -Fq 'boot_publication_receipts::table' "$$chain"; \
	grep -Fq 'pub(crate) fn load_exact_promoted_boot_publication_receipt_chain(' "$$chain"; \
	grep -Fq 'load_exact_promoted_state_with_predecessor(' "$$chain"; \
	grep -Fq 'pub(super) fn load_exact_promoted_state_with_predecessor(' "$$promotion"; \
	test "$$( grep -Fc 'connection.transaction(|connection|' "$$chain" )" = 2; \
	if rg -n 'exclusive_tx|promote_pending_row|stage_pending_row|diesel::(insert_into|update|delete)|\b(INSERT|UPDATE|DELETE)\b|transition_journal|linux_fs' "$$chain"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	for file in "$$chain" "$$tests" "$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/misc/make/boot-publication-promoted-receipt-chain-tests.mk"; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test --manifest-path "$(BOOT_PUBLICATION_PROMOTED_CHAIN_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1 --include-ignored
