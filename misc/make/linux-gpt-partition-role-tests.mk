GPT_PARTITION_ROLE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-gpt-partition-role-test

forge-linux-gpt-partition-role-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(GPT_PARTITION_ROLE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(GPT_PARTITION_ROLE_TOP_DIR)/target/linux-gpt-partition-role-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(GPT_PARTITION_ROLE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::gpt_partition_role::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 34; \
	for name in \
		stable::esp_guid_constant_uses_uefi_mixed_endian_disk_bytes \
		stable::xbootldr_guid_constant_uses_uefi_mixed_endian_disk_bytes \
		stable::stable_512_byte_esp_image_returns_selected_semantics_and_table_identity \
		stable::stable_4096_byte_xbootldr_image_returns_selected_semantics_and_table_identity \
		stable::logical_block_size_endpoints_are_both_admitted \
		stable::unselected_used_entries_are_validated_without_changing_the_selected_result \
		snapshot_stability::stable_512_and_4096_tables_retain_deterministic_nonzero_fingerprints \
		snapshot_stability::stable_512_esp_table_fingerprint_v1_is_pinned \
		snapshot_stability::a_valid_unselected_entry_change_is_rejected_between_passes \
		snapshot_stability::a_valid_disk_guid_change_is_rejected_between_4096_byte_passes \
		snapshot_stability::one_exact_table_has_one_fingerprint_independent_of_selected_role \
		snapshot_bounds::two_pass_snapshot_authentication_shares_one_absolute_deadline \
		snapshot_bounds::deadline_expiry_during_the_second_source_fails_before_returning_evidence \
		snapshot_bounds::interpass_revalidation_runs_exactly_once_before_second_pass_reads \
		snapshot_bounds::interpass_revalidation_failure_prevents_second_pass_and_result \
		snapshot_bounds::deadline_expiry_after_interpass_revalidation_prevents_second_pass \
		snapshot_bounds::fixture_limits_cannot_raise_any_hard_production_ceiling \
		snapshot_bounds::complete_stable_ledger_accepts_exact_n_and_rejects_every_n_minus_one \
		malformed::logical_block_size_and_image_length_are_strict \
		malformed::protective_mbr_must_be_exact_and_non_hybrid \
		malformed::both_headers_require_exact_profile_fields_and_crc \
		malformed::header_locations_and_redundant_semantics_must_match \
		malformed::entry_count_size_and_metadata_layout_are_strict \
		malformed::both_entry_arrays_require_crc_and_byte_equality \
		malformed::selected_entry_requires_exact_number_partuuid_and_role \
		malformed::used_entries_require_nonzero_unique_guids_and_usable_ranges \
		malformed::used_entry_ranges_must_not_overlap \
		malformed::unused_entries_must_be_completely_zero \
		malformed::expected_partuuid_text_is_canonical_lowercase_and_nonzero \
		bounds::expired_deadline_and_zero_limits_fail_before_image_parsing \
		bounds::read_byte_and_call_limits_fail_closed_at_exact_boundaries \
		bounds::work_and_allocation_limits_reject_underprovisioned_operations \
		bounds::maximum_entry_count_and_array_size_are_admitted_within_global_bounds \
		bounds::image_truncation_and_too_small_images_fail_without_fallback; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs/gpt_partition_role.rs"; \
	module_dir="$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs/gpt_partition_role"; \
	tests="$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs/tests/gpt_partition_role.rs"; \
	tests_dir="$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs/tests/gpt_partition_role"; \
	timeout 10s test -f "$$module"; \
	timeout 10s test -f "$$tests"; \
	for part in constants fingerprint guid parser reader snapshot stable; do timeout 10s test -f "$$module_dir/$$part.rs"; done; \
	for part in bounds malformed snapshot_bounds snapshot_stability stable support; do timeout 10s test -f "$$tests_dir/$$part.rs"; done; \
	timeout 10s grep -Fq 'pub(crate) mod gpt_partition_role;' "$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs.rs"; \
	timeout 10s grep -Fqx 'mod gpt_partition_role;' "$(GPT_PARTITION_ROLE_TOP_DIR)/crates/forge/src/linux_fs/tests.rs"; \
	for part in constants fingerprint guid parser reader snapshot stable; do timeout 10s grep -Fqx "mod $$part;" "$$module"; done; \
	for part in bounds malformed snapshot_bounds snapshot_stability stable support; do timeout 10s grep -Fqx "mod $$part;" "$$tests"; done; \
	timeout 10s grep -Fq 'pub(crate) enum GptPartitionRole {' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedGptPartitionRole {' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) fn authenticate_gpt_partition_role_image_until(' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) fn authenticate_gpt_partition_role_sources_until(' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) fn authenticate_gpt_partition_role_sources_with_interpass_until(' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) use reader::Image as GptPartitionRoleImage;' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) const fn table_sha256(&self) -> &[u8; 32]' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) const fn partition_number(&self) -> u32' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) const fn logical_block_size(&self) -> u32' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) const fn image_bytes(&self) -> u64' "$$module"; \
	timeout 10s grep -Fq 'image: &[u8],' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) trait Image {' "$$module_dir/reader.rs"; \
	timeout 10s grep -Fq 'pub(super) struct SliceImage<' "$$module_dir/reader.rs"; \
	timeout 10s grep -Fq 'pub(super) const MIN_LOGICAL_BLOCK_SIZE: u32 = 512;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const MAX_LOGICAL_BLOCK_SIZE: u32 = 65_536;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const GPT_SIGNATURE: [u8; 8] = *b"EFI PART";' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const GPT_REVISION_1_0: u32 = 0x0001_0000;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const GPT_HEADER_BYTES: usize = 92;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const GPT_ENTRY_BYTES: u32 = 128;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const MIN_GPT_ENTRIES: u32 = 128;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const MAX_GPT_ENTRIES: u32 = 4_096;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'pub(super) const MAX_ENTRY_ARRAY_BYTES: usize = 512 * 1024;' "$$module_dir/constants.rs"; \
	timeout 10s grep -Fq 'let primary = parse_header(&primary_bytes' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'let backup = parse_header(&backup_bytes' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'if primary_array != backup_array {' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'if type_guid != expected_role.type_guid() {' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'pub(super) struct AuthenticatedGptTableSnapshot {' "$$module_dir/snapshot.rs"; \
	timeout 10s grep -Fq 'first.require_exact_match(&second, &mut operation)?;' "$$module_dir/stable.rs"; \
	timeout 10s grep -Fq 'const TABLE_FINGERPRINT_DOMAIN:' "$$module_dir/fingerprint.rs"; \
	shopt -s nullglob; \
	sources=( "$$module" "$$module_dir"/*.rs "$$tests" "$$tests_dir"/*.rs ); \
	timeout 10s test "$${#sources[@]}" = 15; \
	witness_decl="$$( timeout 10s sed -n '/pub(crate) struct ValidatedGptPartitionRole {/,/^}/p' "$$module" )"; \
	timeout 10s test -n "$$witness_decl"; \
	if timeout 10s rg -n '\b(Image|SliceImage|File|Path|OwnedFd|RawFd|BorrowedFd)\b|image:|descriptor:|fd:' <<<"$$witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|std::os::(?:fd|unix::io)|std::io::(?:Read|Write)|(?:^|[^[:alnum:]_])(File|Path|PathBuf|OpenOptions|OwnedFd|RawFd|BorrowedFd|AsFd|AsRawFd)(?:[^[:alnum:]_]|$$)|/(?:dev|proc|sys)(?:/|"|`)|std::process|process::Command|Command::new|std::env|nix::|libc::|unsafe|ioctl|BLK[A-Z_]+|read_dir|canonicalize|tempfile|write_all|write_at|pwrite|set_len|create_new|remove_(file|dir)|rename\(|mount\(|umount' "$${sources[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '^pub[[:space:]]+(struct|enum|fn|const|static|type|trait|mod)[[:space:]]' "$${sources[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$${sources[@]}" "$(GPT_PARTITION_ROLE_TOP_DIR)/misc/make/linux-gpt-partition-role-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(GPT_PARTITION_ROLE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
