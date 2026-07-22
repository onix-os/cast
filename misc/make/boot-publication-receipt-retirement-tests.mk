BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-boot-publication-receipt-retirement-test

forge-boot-publication-receipt-retirement-test: forge-boot-publication-receipt-promotion-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipts::retirement::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 5; \
	for name in \
		exact_promoted_head_retires_once_and_preserves_immutable_chain \
		exact_retired_retry_is_read_only_and_reauthenticates_retained_chain \
		pending_foreign_identity_and_corrupt_body_fail_without_retirement \
		transaction_and_commit_report_seams_never_claim_false_success \
		body_and_head_drift_at_mutation_boundaries_roll_back_exactly; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	retirement="$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/retirement.rs"; \
	tests="$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/retirement/tests.rs"; \
	head="$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipt_head.rs"; \
	state="$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts.rs"; \
	state_mod="$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/crates/forge/src/db/state/mod.rs"; \
	grep -Fq 'pub(crate) fn retire_promoted_boot_publication_receipt_head(' "$$retirement"; \
	grep -Fq 'connection.transaction(|connection|' "$$retirement"; \
	grep -Fq 'self.conn.exclusive_tx(|connection|' "$$retirement"; \
	test "$$( grep -Fc 'let changed = retire_committed_row(connection, pair.pending)?;' "$$retirement" )" = 1; \
	grep -Fq 'BootPublicationReceiptRetirementOutcome::AlreadyRetired' "$$retirement"; \
	grep -Fq 'BootPublicationReceiptRetirementDurableState::Promoted' "$$retirement"; \
	grep -Fq 'BootPublicationReceiptRetirementDurableState::Retired' "$$retirement"; \
	grep -Fq 'ReceiptReference::Retired' "$$retirement"; \
	grep -Fq 'ReceiptReference::CommittedPredecessor' "$$retirement"; \
	grep -Fq 'load_required_receipt(connection, reference, pair.pending)?' "$$retirement"; \
	grep -Fq 'pub(super) fn retire_committed_row(' "$$head"; \
	helper="$$( sed -n '/^pub(super) fn retire_committed_row(/,/^#\[derive(Queryable)\]/p' "$$head" )"; \
	test "$$( grep -Fc 'diesel::update(' <<<"$$helper" )" = 1; \
	grep -Fq 'committed_receipt_sha256.is_null()' <<<"$$helper" || grep -Fq 'committed_receipt_sha256.eq(None::<&[u8]>)' <<<"$$helper"; \
	grep -Fq 'pending_transition_id.is_null()' <<<"$$helper"; \
	grep -Fq 'pending_receipt_sha256.is_null()' <<<"$$helper"; \
	grep -Fq '#[path = "boot_publication_receipts/retirement.rs"]' "$$state"; \
	grep -Fq 'BootPublicationReceiptRetirementOutcome,' "$$state_mod"; \
	grep -Fq 'pub(crate) fn arm_boot_publication_receipt_retirement_after_commit_error(' "$$retirement"; \
	if rg -n 'boot_publication_receipts::table|diesel::(insert_into|delete)|transition_journal|linux_fs|publish_immutable|renameat|unlinkat|remove_|delete_' "$$retirement"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	if rg -n 'boot_publication_receipts::table|diesel::(insert_into|delete)' <<<"$$helper"; then exit 1; else result="$$?"; test "$$result" = 1; fi; \
	for file in "$$retirement" "$$tests" "$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/misc/make/boot-publication-receipt-retirement-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_RETIREMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
