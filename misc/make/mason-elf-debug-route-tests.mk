.PHONY: mason-elf-debug-route-test

MASON_ELF_DEBUG_ROUTE_TESTS := \
	package::analysis::elf_debug_route::tests::missing_elf_debug_route_fails_before_chain_without_objcopy_or_tree_mutation \
	package::analysis::elf_debug_route::tests::missing_elf_debug_route_leaves_every_path_verified_and_inventory_sealable \
	package::analysis::elf_debug_route::tests::elf_debug_preflight_uses_witnessed_bytes_instead_of_a_replaced_path \
	package::analysis::elf_debug_route::tests::elf32_and_elf64_build_ids_have_class_specific_debug_destinations \
	package::analysis::elf_debug_route::tests::elf_debug_route_uses_non_executable_mode_and_reverse_rule_precedence \
	package::analysis::elf_debug_route::tests::elf_debug_preflight_respects_switch_reachability_and_non_elf_inputs \
	package::analysis::elf_debug_route::tests::existing_debug_destination_accepts_regular_and_rejects_nonregular_before_effects \
	package::analysis::elf_debug_route::tests::routed_debug_output_runs_objcopy_only_after_global_preflight

mason-elf-debug-route-test:
	@set -eu; \
	listed="$$(timeout 300s $(CARGO) test -p mason --lib 'package::analysis::elf_debug_route::tests::' -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -c '^package::analysis::elf_debug_route::tests::.*: test$$' <<<"$$listed")" -eq 8; \
	for test in $(MASON_ELF_DEBUG_ROUTE_TESTS); do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	package_source="$(TOP_DIR)/crates/mason/src/package.rs"; \
	preflight_source="$(TOP_DIR)/crates/mason/src/package/analysis/elf_debug_route.rs"; \
	elf_source="$(TOP_DIR)/crates/mason/src/package/analysis/handler/elf.rs"; \
	elf_input_source="$(TOP_DIR)/crates/mason/src/package/analysis/handler/elf_input.rs"; \
	timeout 10s awk '/\.enumerate_paths\(/ { enumerate = NR } /preflight_elf_debug_routes\(/ { preflight = NR } /analysis::Chain::new\(/ { chain = NR } /analysis\.process\(paths\)/ { process = NR } END { exit !(enumerate && preflight && chain && process && enumerate < preflight && preflight < chain && chain < process) }' "$$package_source"; \
	timeout 10s test "$$(timeout 10s grep -Fc 'VerifiedElf::from_path_info(info)?' "$$preflight_source")" -eq 1; \
	timeout 10s test "$$(timeout 10s grep -Fc 'VerifiedElf::from_path_info(info)?' "$$elf_source")" -eq 1; \
	timeout 10s grep -Fq 'VerifiedAnalyzerInput::from_path_info(info, info.size)?' "$$elf_input_source"; \
	timeout 10s grep -Fq 'ElfStream::open_stream(input.try_clone()?)' "$$elf_input_source"; \
	timeout 10s test "$$(timeout 10s grep -Fc 'ElfStream::open_stream' "$$elf_source")" -eq 1; \
	timeout 10s awk '/#\[cfg\(test\)\]/ { test_module = NR } /ElfStream::open_stream/ { stream_open = NR } END { exit !(test_module && stream_open && test_module < stream_open) }' "$$elf_source"; \
	if timeout 10s rg -n 'parse_elf\([^)]*info\.path|File::open\([^)]*info\.path' "$$preflight_source" "$$elf_source" "$$elf_input_source"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 900s $(CARGO) test -p mason --lib 'package::analysis::elf_debug_route::tests::' -- --test-threads=1; \
	handler_listed="$$(timeout 300s $(CARGO) test -p mason --lib 'package::analysis::handler::elf::tests::' -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -c '^package::analysis::handler::elf::tests::.*: test$$' <<<"$$handler_listed")" -eq 10; \
	timeout 900s $(CARGO) test -p mason --lib 'package::analysis::handler::elf::tests::' -- --test-threads=1
