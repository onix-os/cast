.PHONY: external-test-vectors-contract-test \
	external-test-vectors-supplemental-host-test \
	external-test-vectors-fixture-test

EXTERNAL_TEST_VECTORS_TEST_PREFIX := planner::hermetic_tests::external_test_vectors_

external-test-vectors-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(EXTERNAL_TEST_VECTORS_TEST_PREFIX)" -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -Fc "$(EXTERNAL_TEST_VECTORS_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::external_test_vectors_declaration_and_corpus_fail_closed \
		planner::hermetic_tests::external_test_vectors_tampered_sources_never_become_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

external-test-vectors-supplemental-host-test:
	@timeout 180s dash "$(TOP_DIR)/misc/scripts/test-external-test-vectors-host.sh"

external-test-vectors-fixture-test: external-test-vectors-contract-test \
	external-test-vectors-supplemental-host-test
