ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-owned-replacement-bridge-test

forge-active-reblit-boot-owned-replacement-bridge-test:
	@set -euo pipefail; \
	module="$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/owned_replacement.rs"; \
	tests="$${module%.rs}/tests.rs"; \
	listed="$$( $(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_replacement::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 3; \
	grep -Fq '_effect_seal: &ActiveReblitBootPublicationEffectSeal' "$$module"; \
	test "$$( grep -Fc 'ActiveReblitBootPublicationDeltaExpected' "$$module" )" -ge 5; \
	grep -Fq 'pending_receipt: BootPublicationReceiptFingerprint' "$$module"; \
	grep -Fq 'RetainedBootFileMutationFingerprint::new(*pending_receipt.as_bytes())' "$$module"; \
	grep -Fq '.assess_boot_leaf_below_parent_until(' "$$module"; \
	grep -Fq '.retain_existing_boot_publication_parent_until(' "$$module"; \
	if grep -Fq '.retain_boot_publication_parent_until(' "$$module"; then exit 1; fi; \
	grep -Fq 'DestinationNotDifferent' "$$module"; \
	grep -Fq '.replace_exact_boot_file_until(' "$$module"; \
	grep -Fq ') -> Result<ValidatedRetainedBootFileReplacement, ActiveReblitBootOwnedLeafReplacementError>' "$$module"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(crate\) struct ValidatedRetainedBootFileReplacement|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ValidatedRetainedBootFileReplacement' "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_replacement/model.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$tests" "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/misc/make/active-reblit-boot-owned-replacement-bridge-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_OWNED_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
