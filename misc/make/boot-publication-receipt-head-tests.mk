.PHONY: forge-boot-publication-receipt-head-test

forge-boot-publication-receipt-head-test:
	@set -euo pipefail; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	prefix='db::state::boot_publication_receipt_head::tests::'; \
	test "$$( grep -c "^$$prefix.*: test$$" <<<"$$listed" )" = 15; \
	for name in \
		api::fresh_database_has_one_empty_exact_receipt_head \
		api::first_stage_writes_the_exact_transition_and_pair_once \
		api::exact_stage_retry_is_distinguished_and_does_not_change_the_head \
		api::committed_mismatch_fails_before_pending_conflict_or_mutation \
		api::every_nonexact_existing_pending_pair_is_a_hard_conflict \
		api::typed_test_replacement_and_clear_preserve_schema_invariants \
		api::staged_receipt_head_survives_database_reopen_exactly \
		corruption::malformed_committed_and_pending_fingerprint_lengths_fail_closed \
		corruption::partial_pending_rows_and_noncanonical_transition_ids_fail_closed \
		corruption::missing_wrong_and_duplicate_singletons_are_rejected_by_bounded_inspection \
		corruption::dynamically_mistyped_storage_is_a_typed_error_not_adopted_evidence \
		migration::prior_state_schema_upgrades_without_losing_state_or_provenance \
		migration::migration_rejects_non_singleton_rows_and_invalid_fingerprint_storage \
		migration::transition_constraint_measures_text_bytes_past_embedded_nul \
		migration::migration_accepts_only_the_complete_canonical_storage_shapes; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	for file in \
		crates/forge/src/db/state/boot_publication_receipt_head.rs \
		crates/forge/src/db/state/boot_publication_receipt_head/tests.rs \
		crates/forge/src/db/state/boot_publication_receipt_head/tests/*.rs \
		misc/make/boot-publication-receipt-head-tests.mk; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
