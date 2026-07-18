.PHONY: gitwrap-fixture-bundle-test mason-upstream-git-fixture-import-test \
	mason-upstream-git-share-test

gitwrap-fixture-bundle-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p gitwrap --lib -- --list )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'tests::fixture_bundle::' <<<"$$listed" )" = 3; \
	for test in \
		tests::fixture_bundle::direct_bundle_clone_is_canonical_reopenable_and_exact \
		tests::fixture_bundle::unsafe_or_invalid_bundle_inputs_never_publish_a_destination \
		tests::fixture_bundle::bundle_byte_ceiling_origin_policy_and_existing_destination_fail_closed; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p gitwrap --lib "$$test" -- --exact --test-threads=1; \
	done

mason-upstream-git-fixture-import-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'upstream::git::fixture_import_tests::' <<<"$$listed" )" = 4; \
	for test in \
		upstream::git::fixture_import_tests::exact_import_reopens_and_syncs_through_the_production_cache_path \
		upstream::git::fixture_import_tests::wrong_variant_commit_and_materialization_identities_fail_before_publication \
		upstream::git::fixture_import_tests::unsafe_bundle_files_and_corrupt_bytes_never_publish_a_cache \
		upstream::git::fixture_import_tests::existing_cache_marker_and_post_publication_failure_are_never_adopted; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

mason-upstream-git-share-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'upstream::git::share_tests::retained_share_root_' <<<"$$listed" )" = 2; \
	for test in \
		upstream::git::share_tests::retained_share_root_publishes_normalized_git_without_administration_state \
		upstream::git::share_tests::retained_share_root_never_populates_a_replacement_public_parent; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done
