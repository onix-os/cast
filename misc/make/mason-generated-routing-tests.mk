.PHONY: mason-generated-routing-test

MASON_GENERATED_ROUTING_TESTS := \
	package::collect::publication::tests::generated_route_preflight_uses_projected_kind_and_regular_mode \
	package::collect::publication::tests::one_unrouted_generated_artifact_rejects_the_whole_batch_before_publication \
	package::collect::routing::tests::actual_and_projected_regular_and_symlink_routes_have_identical_semantics \
	package::collect::routing::tests::projected_routes_preserve_reverse_precedence_and_kind_rejection

mason-generated-routing-test:
	@set -eu; \
	listed="$$(timeout 300s $(CARGO) test -p mason --lib 'package::collect::' -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -Fcx -f <(printf '%s: test\n' $(MASON_GENERATED_ROUTING_TESTS)) <<<"$$listed")" -eq 4; \
	for test in $(MASON_GENERATED_ROUTING_TESTS); do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 300s $(CARGO) test -p mason --lib 'package::collect::' -- --test-threads=1
