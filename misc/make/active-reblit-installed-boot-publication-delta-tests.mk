ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-installed-boot-publication-delta-test

forge-active-reblit-installed-boot-publication-delta-test: host-storage-safety-test
	@set -euo pipefail; \
	root="$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/crates/forge/src/client/boot/active_reblit_installed_boot_publication_delta.rs"; \
	tests="$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/crates/forge/src/client/boot/active_reblit_installed_boot_publication_delta_tests.rs"; \
	listed="$$(mktemp "$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/target/active-reblit-installed-boot-delta-list.XXXXXXXXXXXX")"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	prefix='client::active_reblit_installed_boot_publication_delta::tests::'; \
	test "$$(grep -Ec "^$$prefix.*: test$$" "$$listed")" = 10; \
	for name in \
		desired_absent_exact_and_owned_different_have_closed_actions \
		stale_owned_is_post_promotion_deletion_and_stale_unowned_is_preserved \
		different_desired_without_exact_authenticated_owned_predecessor_fails_closed \
		observation_shape_and_same_expected_conflicts_fail_closed \
		alias_and_distinct_keys_use_fat_folding_without_cross_root_confusion \
		only_strict_empty_or_promoted_database_state_can_form_installed_input \
		authenticated_claim_mapping_keeps_first_adoption_borrowed \
		receipt_claim_bridge_uses_inventory_order_and_ignores_stale_entries \
		receipt_claim_bridge_rejects_missing_duplicate_and_stale_desired_keys \
		receipt_claim_bridge_rejects_a_desired_action_with_no_desired_key; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	grep -Fq 'from_exact_promoted_chain(' "$$root"; \
	grep -Fq 'from_strict_empty_state(' "$$root"; \
	grep -Fq 'prepare_installed_boot_publication_delta(' "$$root"; \
	grep -Fq "derive_receipt_provenance_claims<'inventory>(" "$$root"; \
	awk 'previous_previous ~ /cfg[(]test[)]/ && previous ~ /derive[(]Clone, Copy, Debug, Eq, PartialEq[)]/ && $$0 == "struct ActiveReblitBootPublicationDeltaObservation {" { found = 1 } { previous_previous = previous; previous = $$0 } END { exit found ? 0 : 1 }' "$$root"; \
	awk 'previous ~ /cfg[(]test[)]/ && $$0 == "    fn classify(" { found = 1 } { previous = $$0 } END { exit found ? 0 : 1 }' "$$root"; \
	for action in PublishDesired RetainOwnedDesired PreserveBorrowedDesired ReplaceOwnedDesired DeleteOwnedStaleAfterPromotion PreserveUnownedStale; do \
		grep -Fq "$$action" "$$root"; \
	done; \
	if rg -n 'std::fs|fs_err|OpenOptions|File::(?:open|create)|BorrowedFd|OwnedFd|RawFd|AsFd|create_dir|remove_(?:file|dir)|rename\(|std::process|process::Command|Command::new|promote_boot_publication_receipt\(|stage_boot_publication_receipt\(' "$$root"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$root" "$$tests" "$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/misc/make/active-reblit-installed-boot-publication-delta-tests.mk"; do \
		test "$$(wc -l < "$$file")" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_INSTALLED_BOOT_DELTA_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
