.PHONY: forge-cli-state-request-test

forge-cli-state-request-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	prefix='cli::state::request::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )" = 7; \
	for name in \
		state_request_accepts_canonical_positive_i32_boundaries \
		state_request_rejects_noncanonical_out_of_range_and_aliasing_ids \
		state_request_accepts_exact_bounded_removal_range \
		state_request_rejects_descending_and_oversized_ranges_before_expansion \
		state_request_rejects_aggregate_n_plus_one_before_materialization \
		state_command_parser_rejects_invalid_ids_for_every_state_subcommand \
		state_remove_aggregate_rejection_precedes_client_database_creation; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	dispatch_test='cli::tests::state_remove_aggregate_rejection_precedes_context_root_open'; \
	timeout 10s grep -Fqx "$$dispatch_test: test" <<<"$$listed"; \
	cli=crates/forge/src/cli/mod.rs; \
	state=crates/forge/src/cli/state.rs; \
	request=crates/forge/src/cli/state/request.rs; \
	timeout 10s grep -Fqx 'pub use state::StateRequestError;' "$$cli"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'state::preflight(args).map_err(Error::State)?;' "$$cli" )" = 1; \
	timeout 10s awk 'index($$0, "state::preflight(args).map_err(Error::State)?;") { preflight = NR } index($$0, "let installation = open_installation(context)?;") && !opening { opening = NR } END { exit !(preflight && opening && preflight < opening) }' "$$cli"; \
	timeout 10s grep -Fqx 'mod request;' "$$state"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ValueParser::new(request::parse_state_id)' "$$state" )" = 2; \
	timeout 10s grep -Fq '#[arg(value_parser = request::parse_state_id)]' "$$state"; \
	timeout 10s grep -Fq 'ValueParser::new(request::parse_removal_token)' "$$state"; \
	timeout 10s grep -Fq 'MAX_ARCHIVED_STATE_PRUNE_BATCH' "$$request"; \
	timeout 10s grep -Fq 'request::collect_removal_ids(tokens)' "$$state"; \
	timeout 10s grep -Fq 'Client::for_cli(environment::NAME, installation, verbose)?;' "$$state"; \
	if timeout 10s rg -n 'as i32|parse_id_or_range|\(start\.\.=end\)\.collect' "$$state" "$$request"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$cli" "$$state" "$$request" misc/make/state-request-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 600s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1; \
	timeout 600s $(CARGO) test -p forge --lib "$$dispatch_test" -- --test-threads=1
