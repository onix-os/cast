DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-leaf-assessment-test

forge-linux-descriptor-boot-leaf-assessment-test:
	@set -euo pipefail; \
	module="$(DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_leaf_assessment.rs"; \
	module_dir="$${module%.rs}"; \
	tests="$(DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_leaf_assessment.rs"; \
	listed="$$( $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list )"; \
	prefix='linux_fs::tests::descriptor_boot_leaf_assessment::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" <<<"$$listed" )" = 8; \
	for name in \
		missing_parent_is_absent_without_creating_or_synchronizing_it \
		missing_leaf_is_absent_but_binds_the_existing_retained_parent \
		exact_regular_mode_0644_single_link_binds_both_hashes_and_inode \
		stable_regular_content_mode_and_length_mismatches_are_different \
		symlink_nonregular_and_hardlinked_leaves_fail_closed \
		parent_symlink_non_directory_and_unsafe_policy_fail_closed \
		leaf_and_parent_substitution_windows_are_rejected \
		missing_parent_appearance_request_bounds_and_deadline_fail_closed; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	grep -Fq 'pub(crate) fn assess_boot_leaf_below_parent_until' "$$module_dir/parent_walk.rs"; \
	grep -Fq 'pub(crate) fn assess_boot_leaf_until' "$$module"; \
	grep -Fq 'nix::libc::O_PATH' "$$module" "$$module_dir/parent_walk.rs"; \
	grep -Fq 'nix::libc::O_NOFOLLOW' "$$module" "$$module_dir/parent_walk.rs"; \
	grep -Fq 'controlled_resolution()' "$$module" "$$module_dir/parent_walk.rs"; \
	grep -Fq 'Some(nix::libc::ENOENT) => OpenLeafError::Absent' "$$module"; \
	grep -Fq 'Some(nix::libc::ENOENT) => Ok(None)' "$$module_dir/parent_walk.rs"; \
	grep -Fq 'metadata.nlink() != 1' "$$module"; \
	grep -Fq 'opening.mode != REQUIRED_MODE || opening.length != request.expected_length()' "$$module"; \
	grep -Fq 'xxh3.digest128()' "$$module"; \
	grep -Fq 'sha256.finalize()' "$$module"; \
	grep -Fq 'effect::before_terminal_rebind();' "$$module" "$$module_dir/parent_walk.rs"; \
	if rg -n 'mkdirat|create_dir|O_CREAT|renameat|unlinkat|sync_all|sync_filesystem|remove_(file|dir)' "$$module" "$$module_dir"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$module_dir"/*.rs "$$tests" "$(DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR)/misc/make/linux-descriptor-boot-leaf-assessment-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_LEAF_ASSESSMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
