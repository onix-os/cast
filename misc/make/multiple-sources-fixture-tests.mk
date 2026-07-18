.PHONY: multiple-sources-contract-test \
	multiple-sources-supplemental-compiler-test \
	multiple-sources-fixture-test

MULTIPLE_SOURCES_TEST_PREFIX := planner::hermetic_tests::multiple_sources_

multiple-sources-contract-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '$(MULTIPLE_SOURCES_TEST_PREFIX)' <<<"$$listed" )" = 2; \
	for test in \
		planner::hermetic_tests::multiple_sources_declaration_and_lock_mutations_fail_closed \
		planner::hermetic_tests::multiple_sources_raw_and_git_fixture_tampering_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

# This deliberately does not claim Meson, container, Stone packaging, or
# delegated reproduction coverage. It is a fast host-side cross-compiler
# sanity check over the same exact offline source artifacts.
multiple-sources-supplemental-compiler-test:
	@timeout 180s bash "$(TOP_DIR)/misc/scripts/test-multiple-sources-compilers.sh"

multiple-sources-fixture-test: multiple-sources-contract-test \
	multiple-sources-supplemental-compiler-test
