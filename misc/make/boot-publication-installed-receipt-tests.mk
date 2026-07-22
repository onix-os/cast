BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-boot-publication-installed-receipt-test

forge-boot-publication-installed-receipt-test: forge-boot-publication-receipt-promotion-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipts::installed_receipt_tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 2; \
	for name in \
		exact_promoted_head_is_the_durable_installed_receipt \
		installed_receipt_a_becomes_b_predecessor_and_both_bodies_remain_immutable; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	state="$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts.rs"; \
	head="$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipt_head.rs"; \
	tests="$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/installed_receipt_tests.rs"; \
	grep -Fq 'pub(crate) fn load_exact_promoted_boot_publication_receipt_state(' "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/promotion.rs"; \
	grep -Fq 'let committed_predecessor = admitted_state.head().committed();' "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging.rs"; \
	if rg -n 'retire_committed_row|BootPublicationReceiptRetirement|ReceiptReference::Retired|boot_publication_receipts/retirement.rs' "$$state" "$$head" "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/crates/forge/src/db/state/mod.rs"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	for file in "$$tests" "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/misc/make/boot-publication-installed-receipt-tests.mk"; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test --manifest-path "$(BOOT_PUBLICATION_INSTALLED_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1 --include-ignored
