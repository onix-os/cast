MOUNTINFO_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mountinfo-parser-test

forge-linux-mountinfo-parser-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNTINFO_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNTINFO_TOP_DIR)/target/linux-mountinfo-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNTINFO_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::mountinfo_'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 23; \
	for name in \
		grammar::complete_records_preserve_order_fields_and_exact_decoded_path_bytes \
		grammar::all_four_kernel_path_escapes_decode_without_interpreting_utf8 \
		grammar::mangle_fields_decode_hash_escape_while_paths_keep_four_escape_grammar \
		grammar::malformed_or_unknown_path_escapes_are_never_reinterpreted \
		grammar::canonical_numeric_fields_reject_zero_ids_leading_zeroes_signs_and_overflow \
		grammar::duplicate_mount_ids_are_rejected_without_reordering_records \
		grammar::separator_and_space_grammar_rejects_ambiguous_or_incomplete_lines \
		grammar::opaque_options_keep_filesystem_specific_escapes_and_duplicates \
		grammar::empty_missing_newline_and_blank_records_are_truncation_or_invalid_data \
		bounds::byte_ceiling_admits_n_and_rejects_n_plus_one_for_slices_and_readers \
		bounds::line_ceiling_admits_n_and_rejects_n_plus_one \
		bounds::field_and_option_item_ceiling_admits_n_and_rejects_n_plus_one \
		bounds::total_field_ceiling_rejects_before_copying_the_first_over_budget_option_item \
		bounds::field_byte_ceiling_admits_n_and_rejects_n_plus_one \
		bounds::work_ceiling_admits_full_parse_n_and_rejects_n_minus_one \
		bounds::bounded_reader_never_retries_eintr_without_limit \
		bounds::bounded_reader_uses_one_truncation_sentinel_byte \
		bounds::deadline_snapshot_retains_exact_bytes_and_the_complete_parse \
		bounds::expired_snapshot_deadline_reads_zero_bytes \
		bounds::expired_parser_deadline_fails_before_parsing_complete_bytes \
		bounds::production_limits_are_finite_and_internally_consistent \
		compatibility::synthetic_kernel_format_snapshot_is_parser_compatible_without_host_topology \
		compatibility::observed_nsfs_mount_root_is_preserved_as_an_opaque_field; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/mountinfo.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn parse_mountinfo_bytes(bytes: &[u8]) -> io::Result<MountInfo>' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) fn read_mountinfo_bounded(reader: &mut impl io::Read) -> io::Result<MountInfo>' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) fn read_mountinfo_snapshot_bounded_until(' "$$module"; \
	timeout 10s grep -Fq 'MAX_MOUNTINFO_WORK' "$$module"; \
	if timeout 10s rg -n 'from_utf8|to_string_lossy|PathBuf|read_to_end[[:space:]]*\(' "$$module"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	compatibility="$(MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/mountinfo_compatibility.rs"; \
	if timeout 10s rg -n '/proc|File::open|OpenOptions|std::fs::read|thread-self|self/mountinfo' "$$compatibility"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$module" \
		"$(MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/mountinfo_grammar.rs" \
		"$(MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/mountinfo_bounds.rs" \
		"$$compatibility" \
		"$(MOUNTINFO_TOP_DIR)/misc/make/linux-mountinfo-parser-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNTINFO_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
