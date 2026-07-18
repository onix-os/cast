.PHONY: mason-draft-hardening-test

mason-draft-hardening-test: mason-archive-test
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	for test in \
		draft::upstream::tests::same_named_files_from_multiple_inputs_remain_isolated_and_ordered \
		draft::upstream::tests::manifest_file_list_never_follows_an_extracted_ancestor_symlink \
		draft::upstream::tests::unsupported_archive_type_and_digest_mismatch_publish_no_tree \
		draft::upstream::tests::source_count_and_transport_are_rejected_by_preflight \
		draft::test::draft_manifest_limit_accepts_n_and_rejects_n_plus_one_before_analysis \
		draft::test::untyped_python_ruby_and_perl_drafts_fail_closed \
		draft::test::missing_build_metadata_never_defaults_to_invented_autotools_semantics \
		draft::metadata::tests::canonical_metadata_uri_never_rebinds_a_hash_to_different_bytes \
		draft::build::tests::analysis_text_limit_accepts_n_and_rejects_n_plus_one \
		draft::build::tests::expired_analysis_deadline_stops_before_running_analyzers \
		draft::build::tests::metadata_count_and_aggregate_byte_budgets_accept_n_and_reject_n_plus_one \
		draft::build::tests::preflight_counts_each_hardlink_path_against_declared_bytes \
		draft::build::tests::unique_dependency_budget_accepts_n_duplicate_and_rejects_new_n_plus_one \
		draft::build::tests::lower_confidence_build_system_dependencies_never_leak_into_winner \
		draft::build::tests::equal_highest_build_system_confidence_fails_closed \
		draft::build::tests::nested_only_build_markers_produce_no_candidate \
		draft::licenses::tests::bounded_reader_accepts_n_and_rejects_n_plus_one \
		draft::licenses::tests::normalized_text_and_comparison_work_accept_n_and_reject_n_plus_one \
		cli::recipe::tests::failed_draft_leaves_an_absent_output_directory_absent \
		cli::recipe::tests::unsupported_detected_builder_publishes_no_recipe_or_output_directory \
		cli::recipe::tests::undetected_builder_publishes_no_recipe_or_output_directory \
		cli::recipe::tests::existing_recipe_is_untouched_and_drafting_never_starts \
		cli::recipe::tests::recipe_created_during_drafting_wins_and_is_never_replaced \
		cli::recipe::tests::successful_generated_recipe_is_published_with_exact_mode \
		upstream::plain::tests::downloaded_admission_copies_bytes_without_aliasing_the_download_inode \
		upstream::plain::tests::concurrent_identical_downloads_adopt_one_exact_no_clobber_cache_entry \
		upstream::plain::tests::production_store_never_refetches_over_mismatched_cache_state; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p mason --lib 'draft::' -- --test-threads=1; \
	timeout 900s $(CARGO) test -p mason --lib 'cli::recipe::tests::' -- --test-threads=1; \
	timeout 900s $(CARGO) test -p mason --lib 'upstream::plain::tests::' -- --test-threads=1; \
	if timeout 10s rg -n 'bsdtar|tokio::process::Command|buffer_unordered|util::enumerate_files' \
		crates/mason/src/draft.rs crates/mason/src/draft/upstream.rs; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if timeout 10s rg -n 'bsdtar' crates/mason/src/upstream.rs; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if timeout 10s rg -n 'fetched_upstream_cache_path|async_hardlink_or_copy' \
		crates/mason/src/draft/upstream.rs crates/mason/src/cli/recipe.rs; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if timeout 10s rg -n 'WalkDir|read_to_string' \
		crates/mason/src/draft/licenses.rs crates/mason/src/draft/build/autotools.rs \
		crates/mason/src/draft/build/cmake.rs crates/mason/src/draft/build/meson.rs; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if timeout 10s rg -n '%pyproject_|%python_|%gem_|%perl_|%make|%configure|b\.builder\.shell' \
		crates/mason/src/draft.rs crates/mason/src/draft/build; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	timeout 10s rg -q 'ArchiveSessionBudget' crates/mason/src/draft/upstream.rs; \
	timeout 10s rg -q 'extract_draft_tar' crates/mason/src/draft/upstream.rs; \
	timeout 10s rg -q 'persist_noclobber' crates/mason/src/upstream/plain.rs; \
	if timeout 10s rg -n 'fetch\(self\.url\.clone\(\), &path' crates/mason/src/upstream/plain.rs; \
		then exit 1; else status=$$?; test "$$status" = 1; fi
