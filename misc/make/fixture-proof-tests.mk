.PHONY: fixture-proof-test fixture-proof-validator-test \
	mason-fixture-proof-evidence-test \
	mason-fixture-proof-cross-boundary-test

fixture-proof-validator-test:
	@timeout 180s "$(TOP_DIR)/misc/scripts/test-validate-fixtures-ci-proof.sh"

mason-fixture-proof-evidence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^planner::hermetic_tests::bootstrap::execution_evidence::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
	timeout 1200s $(CARGO) test -p mason --lib \
		"planner::hermetic_tests::bootstrap::execution_evidence::tests::" -- \
		--test-threads=1

mason-fixture-proof-cross-boundary-test:
	@timeout 1200s $(CARGO) test -p mason --lib \
		"planner::hermetic_tests::bootstrap::execution_evidence::tests::rust_published_proof_passes_the_exact_shell_validator" -- \
		--ignored --exact --test-threads=1

fixture-proof-test: fixture-proof-validator-test \
	mason-fixture-proof-evidence-test \
	mason-fixture-proof-cross-boundary-test
