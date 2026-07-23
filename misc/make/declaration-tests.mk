.PHONY: declarative-config-test gluon-adapter-test declaration-regression-test

# Test-only proof of the engine-neutral, typed declaration boundary.
declarative-config-test:
	@$(CARGO) test -p config --test declaration_adapter_contract -- --test-threads=1

# Characterization gate for the current Gluon parser, loader, evaluator, and identity.
gluon-adapter-test:
	@$(CARGO) test -p gluon_config -- --test-threads=1

# Existing read-only and writable Gluon consumers stay green while the boundary moves.
declaration-regression-test: declarative-config-test gluon-adapter-test
	@$(CARGO) test -p config --lib -- --test-threads=1
	@$(CARGO) test -p triggers --test gluon -- --test-threads=1
