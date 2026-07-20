.PHONY: pgo-workload-contract-test pgo-workload-fixture-test

PGO_WORKLOAD_TEST_PREFIX := planner::hermetic_tests::pgo_workload_

pgo-workload-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(PGO_WORKLOAD_TEST_PREFIX)" -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -Fc "$(PGO_WORKLOAD_TEST_PREFIX)" <<<"$$listed")" -eq 1; \
	timeout 10s grep -Fqx \
		'planner::hermetic_tests::pgo_workload_declaration_and_training_source_fail_closed: test' \
		<<<"$$listed"; \
	timeout 300s $(CARGO) test -p mason --lib \
		planner::hermetic_tests::pgo_workload_declaration_and_training_source_fail_closed -- \
		--exact --nocapture --test-threads=1

pgo-workload-fixture-test: pgo-workload-contract-test
