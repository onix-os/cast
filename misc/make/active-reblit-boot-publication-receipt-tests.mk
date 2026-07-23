ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: forge-active-reblit-boot-publication-receipt-test

forge-active-reblit-boot-publication-receipt-test: host-storage-safety-test forge-active-reblit-desired-publication-test forge-active-reblit-mounted-boot-topology-capture-test forge-boot-publication-receipt-head-test
	@set -euo pipefail; \
	listed="$$( $(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	body_prefix='boot_publication::receipt_body::tests::'; \
	test "$$( grep -Ec "^$$body_prefix.*: test$$" <<<"$$listed" )" = 7; \
	for name in \
		complete_body_retains_every_authority_free_provenance_claim \
		destination_shape_is_exact_and_distinct_targets_must_not_alias \
		historical_destination_device_must_match_its_partition_identity \
		distinct_destinations_require_distinct_partition_devices_and_equal_disk_sequences \
		output_mode_is_restricted_to_canonical_active_reblit_mode \
		empty_oversized_and_unsafely_pathed_inventories_fail_closed \
		output_order_duplicate_and_fat_alias_collisions_are_rejected; do \
		grep -Fqx "$$body_prefix$$name: test" <<<"$$listed"; \
	done; \
	codec_prefix='boot_publication::receipt_codec::tests::'; \
	test "$$( grep -Ec "^$$codec_prefix.*: test$$" <<<"$$listed" )" = 4; \
	for name in \
		canonical_receipt_round_trips_exact_body_bytes_and_identity \
		canonical_fixture_has_pinned_bytes_and_domain_separated_fingerprint \
		fingerprint_changes_when_any_receipt_identity_domain_changes \
		malformed_noncanonical_and_oversized_bodies_fail_closed; do \
		grep -Fqx "$$codec_prefix$$name: test" <<<"$$listed"; \
	done; \
	client_prefix='client::active_reblit_boot_publication_receipt::tests::'; \
	test "$$( grep -Ec "^$$client_prefix.*: test$$" <<<"$$listed" )" = 8; \
	for name in \
		contracts::only_the_exact_boot_sync_started_predecessor_is_admitted \
		contracts::provenance_claims_must_cover_the_exact_canonical_inventory \
		contracts::provenance_claim_bindings_reject_a_same_length_permutation \
		contracts::mapper_rejects_a_substituted_deadline_and_checks_expiry_at_entry \
		topology::distinct_topology_maps_stable_partition_identity_and_historical_witnesses \
		topology::topology_shape_must_match_the_desired_destination_layout \
		integration::real_bound_alias_plan_maps_to_one_complete_authority_free_receipt \
		integration::committed_predecessor_and_claim_data_are_fingerprint_significant; do \
		grep -Fqx "$$client_prefix$$name: test" <<<"$$listed"; \
	done; \
	shared="$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/boot_publication.rs"; \
	body="$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/boot_publication/receipt_body.rs"; \
	codec="$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/boot_publication/receipt_codec.rs"; \
	mapper="$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_receipt.rs"; \
	error="$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_receipt/error.rs"; \
	grep -Fq 'b"os-tools/forge/boot-publication-receipt-body/v1\0"' "$$codec"; \
	grep -Fq '#[serde(deny_unknown_fields)]' "$$body"; \
	grep -Fq 'pub(in crate::client) fn prepare_complete_boot_publication_receipt(' "$$mapper"; \
	grep -Fq '.boot_sync_started_successor(BootPublicationReceiptPair {' "$$mapper"; \
	grep -Fq 'if target.partuuid != target.partition_uuid.as_str() {' "$$mapper"; \
	grep -Fq 'if target.destination.raw_device() != target.boot_filesystem.destination_device()' "$$mapper"; \
	if rg -n 'std::fs|fs_err|OpenOptions|File::(?:open|create)|create_dir|remove_(?:file|dir)|rename\(|unlink(?:at)?\b|linkat\b|mount\(|umount\(|std::process|process::Command|Command::new' "$$shared" "$$body" "$$codec" "$$mapper" "$$error"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'BorrowedFd|OwnedFd|RawFd|AsFd|AsRawFd|FromRawFd|IntoRawFd|FileExt|read_at|write_at|openat2|fstat|statx' "$$shared" "$$body" "$$codec" "$$mapper" "$$error"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'crate::db|super::db|\bdb::|rusqlite|Connection|Transaction|query_row|execute\(|boot_publication_receipts' "$$shared" "$$body" "$$codec" "$$mapper" "$$error"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in \
		"$$shared" \
		"$$body" \
		"$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/boot_publication/receipt_body_tests.rs" \
		"$$codec" \
		"$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/boot_publication/receipt_codec_tests.rs" \
		"$$mapper" \
		"$$error" \
		"$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_receipt_tests.rs" \
		"$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_receipt_tests/*.rs \
		"$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/misc/make/active-reblit-boot-publication-receipt-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$body_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$codec_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_RECEIPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$client_prefix" -- --test-threads=1
