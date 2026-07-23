.PHONY: relation-policy-contract-test relation-policy-fixture-test

RELATION_POLICY_TEST := planner::hermetic_tests::relation_policy_source_less_declaration_is_exact_and_role_validated

relation-policy-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(RELATION_POLICY_TEST)" -- --list)"; \
	timeout 10s grep -Fqx '$(RELATION_POLICY_TEST): test' <<<"$$listed"; \
	timeout 300s $(CARGO) test -p mason --lib "$(RELATION_POLICY_TEST)" -- \
		--exact --nocapture --test-threads=1

relation-policy-fixture-test: relation-policy-contract-test
