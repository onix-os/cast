DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-file-replacement-test

forge-linux-descriptor-boot-file-replacement-test:
	@set -euo pipefail; \
	module="$(DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_replacement.rs"; \
	module_dir="$${module%.rs}"; \
	tests="$(DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_file_replacement.rs"; \
	listed="$$( $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='linux_fs::tests::descriptor_boot_file_replacement::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 8; \
	grep -Fq 'renameat2_exchange_once(parent, canonical_name, parent, sidecar_name)' "$$module_dir/effect.rs"; \
	test "$$( grep -Fc 'effect::exchange_once(&parent, &names.canonical, &names.sidecar)' "$$module" )" = 2; \
	grep -Fq 'authenticate_applied_boot_file_replacement_until' "$$module"; \
	grep -Fq 'validate_applied_boot_file_replacement_until' "$$module"; \
	grep -Fq 'restore_exact_boot_file_replacement_until' "$$module"; \
	grep -Fq 'cleanup_replaced_boot_file_sidecar_until' "$$module"; \
	grep -Fq 'cleanup_restored_boot_file_sidecar_until' "$$module"; \
	grep -Fq 'authenticate_stale_boot_file_cleanup_until' "$$module"; \
	grep -Fq 'cleanup_authenticated_stale_boot_file_until' "$$module"; \
	grep -Fq 'authenticate_restored_boot_file_replacement_until' "$$module_dir/recovery.rs"; \
	grep -Fq 'reconcile_stale_boot_file_cleanup_until' "$$module_dir/recovery.rs"; \
	test "$$( grep -Fc 'effect::detach_once(&parent, &canonical, &private)' "$$module" )" = 1; \
	grep -Fq 'require_absent(&parent, &names.sidecar, deadline)?' "$$module"; \
	if rg -n 'std::fs::rename|rename\(' "$$module" "$$module_dir"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$module_dir"/*.rs "$$tests" "$(DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR)/misc/make/linux-descriptor-boot-file-replacement-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_REPLACEMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
