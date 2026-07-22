ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-owned-replacement-bridge-test

forge-active-reblit-boot-owned-replacement-bridge-test: forge-linux-descriptor-boot-file-replacement-test forge-linux-descriptor-boot-publication-parent-test
	@set -euo pipefail; \
	module="$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/owned_replacement.rs"; \
	module_dir="$${module%.rs}"; \
	tests="$$module_dir/tests.rs"; \
	validation="$$module_dir/validation.rs"; \
	fixture="$$module_dir/fixture.rs"; \
	listed="$$( $(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_replacement::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 2; \
	for name in \
		canonical_path_split_preserves_exact_parent_order_and_leaf \
		malformed_root_only_and_overdeep_paths_fail_before_effects; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	grep -Fq '_effect_seal: &ActiveReblitBootPublicationEffectSeal' "$$module"; \
	test "$$( grep -Fc 'ActiveReblitBootPublicationDeltaExpected' "$$module" )" -ge 5; \
	grep -Fq 'mutation_owner(_effect_seal)' "$$module"; \
	grep -Fq 'RetainedBootFileMutationFingerprint::new(*effect_seal.pending_receipt().as_bytes())' "$$module"; \
	if rg -n 'pending_receipt:\s*BootPublicationReceiptFingerprint' "$$module"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq '.assess_boot_leaf_below_parent_until(' "$$module"; \
	grep -Fq '.retain_existing_boot_publication_parent_until(' "$$module"; \
	if grep -Fq '.retain_boot_publication_parent_until(' "$$module"; then exit 1; fi; \
	grep -Fq 'DestinationNotDifferent' "$$module"; \
	grep -Fq '.replace_exact_boot_file_until(' "$$module"; \
	grep -Fq ') -> Result<ValidatedRetainedBootFileReplacement, ActiveReblitBootOwnedLeafReplacementError>' "$$module"; \
	grep -Fq 'validate_applied_owned_leaf_replacement' "$$validation"; \
	grep -Fq '.retain_existing_boot_publication_parent_until(' "$$validation"; \
	grep -Fq '.validate_applied_boot_file_replacement_until(evidence, self.deadline)' "$$validation"; \
	grep -Fq 'require_parent_root_identity(self, &parent)?' "$$validation"; \
	grep -Fq 'validate_applied_boot_file_replacement_until(evidence, deadline)' "$$fixture"; \
	grep -Fq 'VALIDATION_ROOTS' "$$fixture"; \
	if rg -n '\.(replace_exact_boot_file_until|restore_exact_boot_file_replacement_until|cleanup_replaced_boot_file_sidecar_until|cleanup_restored_boot_file_sidecar_until)\(' "$$validation"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(crate\) struct ValidatedRetainedBootFileReplacement|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ValidatedRetainedBootFileReplacement' "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_replacement/model.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$module_dir"/*.rs "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/misc/make/active-reblit-boot-owned-replacement-bridge-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
