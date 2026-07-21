BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-boot-publication-receipt-state-test

forge-boot-publication-receipt-state-test: forge-boot-publication-receipt-head-test forge-active-reblit-boot-publication-receipt-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipts::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 12; \
	for name in \
		api::first_stage_persists_one_exact_body_and_exact_retry_is_read_only \
		api::exact_committed_predecessor_and_pending_body_survive_reopen \
		api::committed_and_pending_conflicts_leave_the_exact_state_unchanged \
		api::head_update_failure_rolls_back_the_body_insert_in_the_same_transaction \
		corruption::committed_and_pending_head_references_must_have_canonical_bodies \
		corruption::tampered_noncanonical_and_hash_mismatched_bodies_fail_closed \
		corruption::mistyped_and_oversized_bodies_fail_before_typed_body_loading \
		corruption::row_head_transition_and_pending_predecessor_linkage_is_exact \
		corruption::a_conflicting_preexisting_body_cannot_be_adopted_or_stage_the_head \
		migration::receipt_table_migration_is_additive_and_initially_empty \
		migration::receipt_table_constraints_reject_invalid_storage_shapes \
		migration::transition_constraint_counts_bytes_after_embedded_nul; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	state="$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts.rs"; \
	head="$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipt_head.rs"; \
	state_mod="$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/mod.rs"; \
	schema="$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/schema.rs"; \
	migration="$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/migrations/2026-07-21-010000_boot_publication_receipts/up.sql"; \
	grep -Fq 'CREATE TABLE boot_publication_receipts (' "$$migration"; \
	grep -Fq 'receipt_sha256 BLOB NOT NULL PRIMARY KEY CHECK (' "$$migration"; \
	grep -Fq "typeof(receipt_sha256) = 'blob'" "$$migration"; \
	grep -Fq 'length(receipt_sha256) = 32' "$$migration"; \
	grep -Fq 'transition_id TEXT NOT NULL CHECK (' "$$migration"; \
	grep -Fq 'length(CAST(transition_id AS BLOB)) = 32' "$$migration"; \
	grep -Fq "transition_id NOT GLOB '*[^0-9a-f]*'" "$$migration"; \
	grep -Fq 'canonical_body BLOB NOT NULL CHECK (' "$$migration"; \
	grep -Fq "typeof(canonical_body) = 'blob'" "$$migration"; \
	grep -Fq 'length(canonical_body) > 0' "$$migration"; \
	grep -Fq 'length(canonical_body) <= 16777216' "$$migration"; \
	grep -Fq 'boot_publication_receipts (receipt_sha256) {' "$$schema"; \
	grep -Fq 'receipt_sha256 -> Binary,' "$$schema"; \
	grep -Fq 'transition_id -> Text,' "$$schema"; \
	grep -Fq 'canonical_body -> Binary,' "$$schema"; \
	grep -Fq 'mod boot_publication_receipts;' "$$state_mod"; \
	grep -Fq 'pub(crate) use boot_publication_receipts::{' "$$state_mod"; \
	test "$$( grep -Fc 'self.conn.exclusive_tx(|tx| stage_receipt(tx, receipt))' "$$state" )" = 1; \
	test "$$( grep -Fc 'self.conn.exclusive_tx' "$$state" )" = 1; \
	test "$$( grep -Fc 'diesel::insert_into(boot_publication_receipts::table)' "$$state" )" = 1; \
	test "$$( grep -Fc 'stage_pending_row(connection, transition_id, &pair)?' "$$state" )" = 1; \
	test "$$( grep -Fc 'fn stage_boot_publication_receipt_pair(' "$$head" )" = 1; \
	awk 'previous == "    #[cfg(test)]" && $$0 == "    pub(crate) fn stage_boot_publication_receipt_pair(" { found = 1 } { previous = $$0 } END { exit(found ? 0 : 1) }' "$$head"; \
	surface="$$( sed -nE 's/^[[:space:]]*pub\(crate\)[[:space:]]+(const[[:space:]]+)?fn[[:space:]]+([A-Za-z0-9_]+).*/\2/p' "$$state" )"; \
	expected_surface="$$( printf '%s\n' head committed pending receipt_pair_for boot_publication_receipt_state stage_boot_publication_receipt )"; \
	test "$$surface" = "$$expected_surface"; \
	if rg -n 'diesel::(update|delete)|pub\(crate\)[[:space:]]+fn[[:space:]]+(promote|commit|complete|replace|update|delete|remove|prune|gc|garbage_collect)' "$$state"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in \
		"$$state" \
		"$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/crates/forge/src/db/state/boot_publication_receipts/tests.rs" \
		"$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)"/crates/forge/src/db/state/boot_publication_receipts/tests/*.rs \
		"$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)"/crates/forge/src/db/state/migrations/2026-07-21-010000_boot_publication_receipts/*.sql \
		"$$schema" \
		"$$state_mod" \
		"$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/misc/make/boot-publication-receipt-state-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(BOOT_PUBLICATION_RECEIPT_STATE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
