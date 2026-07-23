.PHONY: config-declaration-manager-test config-rooted-declaration-loader-test config-fixed-root-declaration-loader-test config-declaration-storage-test declarative-config-test gluon-adapter-test trigger-declaration-test declaration-regression-test

# Language-neutral typed manager dispatch, precedence, and persistence.
config-declaration-manager-test:
	@$(CARGO) test -p config --test language_neutral_manager -- --test-threads=1

# Language-neutral descriptor-rooted dispatch, bounds, and revalidation.
config-rooted-declaration-loader-test:
	@$(CARGO) test -p config --test rooted_language_neutral_loader -- --test-threads=1

# Language-neutral typed loading for one fixed declaration slot.
config-fixed-root-declaration-loader-test:
	@$(CARGO) test -p config --test fixed_root_language_neutral_loader -- --test-threads=1

# Language-neutral fixed/generated declaration-slot contracts and persistence.
config-declaration-storage-test:
	@$(CARGO) test -p config --lib "declaration::" -- --test-threads=1

# Shared declaration-core tests plus the typed adapter-boundary proof.
declarative-config-test:
	@$(CARGO) test -p declarative_config -- --test-threads=1
	@$(CARGO) test -p config --test declaration_adapter_contract -- --test-threads=1

# Characterization gate for the current Gluon parser, loader, evaluator, and identity.
gluon-adapter-test:
	@$(CARGO) test -p gluon_config -- --test-threads=1

# Typed read-only trigger adapter and restricted Gluon ABI behavior.
trigger-declaration-test:
	@$(CARGO) test -p triggers --test gluon -- --test-threads=1

# Existing storage plus all twelve Gluon declaration roots stay green while the
# boundary moves. The boot-related filters run evaluator tests backed only by
# synthetic temporary trees; they do not mount, publish, or mutate host disks.
declaration-regression-test: declarative-config-test gluon-adapter-test trigger-declaration-test config-declaration-manager-test config-rooted-declaration-loader-test config-fixed-root-declaration-loader-test
	@$(CARGO) test -p config --lib -- --test-threads=1
	@$(CARGO) test -p stone_recipe --test package_v3 -- --test-threads=1
	@$(CARGO) test -p mason --lib "recipe::tests::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --test build_policy -- --test-threads=1
	@$(CARGO) test -p stone_recipe --test build_policy_patch -- --test-threads=1
	@$(CARGO) test -p stone_recipe --test build_policy_layers -- --test-threads=1
	@$(CARGO) test -p mason --lib "source_lock::tests::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --lib "derivation::build_lock::tests::" -- --test-threads=1
	@$(CARGO) test -p mason --lib "build_lock::tests::" -- --test-threads=1
	@$(CARGO) test -p mason --lib "profile::tests::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "repository::gluon::tests::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "system_model::gluon::tests::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "client::active_reblit_boot_topology_intent::tests::evaluation::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "client::active_reblit_root_filesystem_intent::tests::evaluation::" -- --test-threads=1
