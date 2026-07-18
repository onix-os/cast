SHELL := /bin/bash

SYSFS_BLOCK_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-sysfs-block-parser-test

forge-linux-sysfs-block-parser-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(SYSFS_BLOCK_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(SYSFS_BLOCK_TOP_DIR)/target/linux-sysfs-block-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(SYSFS_BLOCK_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::sysfs_block_'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 24; \
	for name in \
		identity::partition_identity_cross_checks_all_required_attributes_and_retains_event \
		identity::partition_identity_rejects_cross_file_disagreement \
		identity::partition_identity_requires_canonical_known_fields \
		identity::disk_identity_requires_disk_type_and_matching_device_number \
		identity::optional_disk_sequence_must_be_absent_on_both_or_equal_on_both \
		links::dev_block_target_normalizes_relative_to_the_fixed_base_as_raw_bytes \
		links::dev_block_target_rejects_escape_dot_empty_and_non_devices_forms \
		links::dev_block_target_bounds_accept_n_and_reject_n_plus_one \
		links::dev_block_target_work_bound_accepts_exact_consumption_and_rejects_one_less \
		links::subsystem_target_extracts_only_a_validated_final_basename \
		links::subsystem_target_rejects_ambiguous_or_noncanonical_basenames \
		links::subsystem_bounds_and_work_are_exact \
		links::link_parsers_reject_zero_or_incoherent_limits \
		numeric::dev_attribute_accepts_exact_canonical_u32_pairs \
		numeric::dev_attribute_rejects_noncanonical_or_out_of_range_numbers \
		numeric::dev_attribute_enforces_the_exact_maximum_length_boundary \
		numeric::partition_attribute_accepts_only_positive_canonical_u32 \
		numeric::partition_attribute_enforces_the_exact_maximum_length_boundary \
		uevent::uevent_retains_order_unknown_keys_empty_values_and_opaque_value_bytes \
		uevent::uevent_rejects_duplicate_keys_and_noncanonical_line_grammar \
		uevent::uevent_byte_and_line_bounds_accept_n_and_reject_n_plus_one \
		uevent::uevent_key_bound_is_exactly_sixty_four_bytes \
		uevent::uevent_work_bound_accepts_exact_consumption_and_rejects_one_less \
		uevent::uevent_rejects_zero_or_incoherent_configured_limits; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn parse_sysfs_dev(bytes: &[u8]) -> io::Result<SysfsDeviceNumber>' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/numeric.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn parse_sysfs_uevent(bytes: &[u8]) -> io::Result<SysfsUevent>' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/uevent.rs"; \
	timeout 10s grep -Fq 'const MAX_UEVENT_KEY_BYTES: usize = 64;' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/uevent.rs"; \
	timeout 10s grep -Fq 'const MAX_UEVENT_WORK: usize = 2 * 1024 * 1024;' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/uevent.rs"; \
	timeout 10s grep -Fq 'const MAX_LINK_COMPONENTS: usize = 128;' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/links.rs"; \
	timeout 10s grep -Fq 'components.first().map(Vec::as_slice) != Some(b"devices")' "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block/links.rs"; \
	if timeout 10s rg -n 'canonicalize|read_link|read_dir|PathBuf|std::process|Command|/dev/disk|blkid|udev|DEVNAME|PARTNAME|(^|[^[:alnum:]_])(mount|umount)[[:space:]]*\(' "$$root" "$(SYSFS_BLOCK_TOP_DIR)/crates/forge/src/linux_fs/sysfs_block"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$root" \
		"$(SYSFS_BLOCK_TOP_DIR)"/crates/forge/src/linux_fs/sysfs_block/*.rs \
		"$(SYSFS_BLOCK_TOP_DIR)"/crates/forge/src/linux_fs/tests/sysfs_block_*.rs \
		"$(SYSFS_BLOCK_TOP_DIR)/misc/make/linux-sysfs-block-parser-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(SYSFS_BLOCK_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
