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

# Phase L8: release-built Lua execution parity. `make build` alone is not
# execution.
lua-release-test:
	@echo 'lua-release-test: pending phase L8 (release parity harness not built yet)' >&2
	@exit 1

# Phase L0/L9: audit the Lua runtime dependency tree and its licenses/notices.
lua-dependency-audit:
	@echo 'lua-dependency-audit: pending phase L0 engine selection (no mlua dependency selected yet)' >&2
	@exit 1

# Phase L8: installed-state migration bridge (catalog coverage, rollback
# selection, resume, interruption).
lua-installed-state-test:
	@echo 'lua-installed-state-test: pending phase L8 (installed-state bridge not implemented yet)' >&2
	@exit 1
