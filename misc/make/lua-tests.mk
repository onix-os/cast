CARGO ?= cargo

.PHONY: lua-engine-spike lua-engine-spike-release lua-config-test \
	lua-domain-parity-test lua-release-test lua-dependency-audit \
	lua-installed-state-test

# Phase L0 engine spike: prove the selected Lua dialect/runtime satisfies the
# shared evaluator policy (capabilities, limits, deadline, imports,
# determinism) in a debug build before any production adapter exists.
lua-engine-spike:
	@$(CARGO) test -p lua_engine_spike

# Same spike executed against the release profile, which is where the musl /
# static-linkage and interrupt behavior must also hold.
lua-engine-spike-release:
	@$(CARGO) test -p lua_engine_spike --release

# Phase L1: the isolated LuaEngine declaration-adapter contract (parser
# profile, capability allowlist, value-tree bounds, host-latched limits).
lua-config-test:
	@if $(CARGO) metadata --no-deps --format-version 1 2>/dev/null | grep -q '"name":"lua_config"'; then \
		$(CARGO) test -p lua_config; \
	else \
		echo 'lua-config-test: pending phase L1 (crates/lua_config not created yet)' >&2; \
		exit 1; \
	fi

# Phase L2+: differential Gluon/Lua domain parity — equal normalized Rust
# values, intentionally distinct v2 identities. Grows one domain at a time.
lua-domain-parity-test:
	@$(CARGO) test -p triggers --lib "lua::" -- --test-threads=1
	@$(CARGO) test -p mason --lib "profile::lua::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "repository::lua::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "system_model::lua::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --lib "build_policy::layers::lua::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --lib "build_policy::lua::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --lib "package::lua::" -- --test-threads=1
	@$(CARGO) test -p stone_recipe --lib "derivation::build_lock::lua::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "active_reblit_boot_topology_intent::lua::" -- --test-threads=1
	@$(CARGO) test -p forge --lib "active_reblit_root_filesystem_intent::lua::" -- --test-threads=1

# Phase L8: release-built Lua execution parity. `make build` alone is not
# execution — these run release-built tests so the interrupt/limit behavior is
# exercised on the optimized profile as well as debug.
lua-release-test:
	@$(CARGO) test -p lua_engine_spike --release
	@$(CARGO) test -p lua_config --release
	@$(CARGO) test -p triggers --release --lib "lua::" -- --test-threads=1
	@$(CARGO) test -p forge --release --lib "declaration_migration::" -- --test-threads=1

# Phase L0/L9: audit the Lua runtime dependency subtrees (mlua/full_moon) so a
# new transitive dependency or license is visible before it ships.
lua-dependency-audit:
	@$(CARGO) tree -p mlua --edges normal
	@$(CARGO) tree -p full_moon --edges normal

# Phase L8: installed-state migration bridge — durable catalog authority,
# content-addressed blobs, crash-order/prune/GC invariants, and coverage.
lua-installed-state-test:
	@$(CARGO) test -p forge --lib "db::state::declaration_migrations" -- --test-threads=1
	@$(CARGO) test -p forge --lib "declaration_migration::" -- --test-threads=1
