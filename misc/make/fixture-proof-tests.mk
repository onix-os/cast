.PHONY: fixture-proof-test fixture-proof-validator-test \
	mason-fixture-proof-evidence-test \
	mason-fixture-proof-cross-boundary-test \
	mason-fixture-execution-session-test \
	mason-fixture-cleanup-witness-test \
	mason-fixture-cleanup-boundary-test

fixture-proof-validator-test:
	@timeout 180s "$(TOP_DIR)/misc/scripts/test-validate-fixtures-ci-proof.sh"

mason-fixture-proof-evidence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^planner::hermetic_tests::bootstrap::execution_evidence::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 14; \
	timeout 1200s $(CARGO) test -p mason --lib \
		"planner::hermetic_tests::bootstrap::execution_evidence::tests::" -- \
		--test-threads=1

mason-fixture-proof-cross-boundary-test:
	@timeout 1200s $(CARGO) test -p mason --lib \
		"planner::hermetic_tests::bootstrap::execution_evidence::tests::rust_published_proof_passes_the_exact_shell_validator" -- \
		--ignored --exact --test-threads=1

mason-fixture-execution-session-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( grep -c '^planner::hermetic_tests::execution_session::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 10; \
	$(CARGO) test -p mason --lib \
		"planner::hermetic_tests::execution_session::tests::" -- \
		--test-threads=1

mason-fixture-cleanup-witness-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( grep -c '^planner::hermetic_tests::execution_cleanup_witness_tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 3; \
	$(CARGO) test -p mason --lib \
		"planner::hermetic_tests::execution_cleanup_witness_tests::" -- \
		--test-threads=1

mason-fixture-cleanup-boundary-test:
	@set -eu; \
	mason_listed="$$( $(CARGO) test -p mason --lib -- --list )"; \
	for test_name in \
		container::tests::descriptor_child_open_rejects_mount_crossings \
		paths::tests::execution_lock_rejects_symlink_and_multiple_link_regular_file \
		paths::tests::frozen_scratch_is_atomically_replaced_and_bounded_cleanup_never_follows_links; do \
		grep -Fqx "$$test_name: test" <<<"$$mason_listed"; \
		$(CARGO) test -p mason --lib "$$test_name" -- --exact --test-threads=1; \
	done; \
	forge_listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test_name in \
		client::tests::frozen_discard_unlinks_symlinks_without_touching_external_targets \
		client::tests::frozen_discard_unlink_never_retries_against_a_foreign_replacement \
		client::tests::frozen_discard_rejects_destination_parent_replacement_without_touching_either_tree; do \
		grep -Fqx "$$test_name: test" <<<"$$forge_listed"; \
		$(CARGO) test -p forge --lib "$$test_name" -- --exact --test-threads=1; \
	done

fixture-proof-test: fixture-proof-validator-test \
	mason-fixture-proof-evidence-test \
	mason-fixture-proof-cross-boundary-test \
	mason-fixture-execution-session-test \
	mason-fixture-cleanup-witness-test \
	mason-fixture-cleanup-boundary-test
