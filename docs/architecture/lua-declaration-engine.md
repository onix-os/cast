# Lua declaration engine — selection and endpoint decision

**Status:** Phase L0 accepted (engine spike). No production `.lua` domain is
registered yet.
**Plan:** [`plans/lua.md`](../../plans/lua.md), Phase L0.
**Prerequisite:** [`plans/agnostic_config.md`](../../plans/agnostic_config.md) is
complete — the declaration core, Gluon-as-adapter boundary, engine-neutral
`EvaluationIdentity`, and the second-adapter seam are all in place.

## Selected engine

Cast's Lua declaration profile targets **standard Lua 5.4** through:

- **`mlua`** (vendored Lua 5.4) as the runtime. Vendoring pins the exact
  interpreter (`lua-src` 547 → Lua 5.4.7), builds with the workspace C toolchain,
  and keeps the musl/static release story simple. System Lua is never used —
  host-selected versions and search paths would break reproducibility.
- **`full_moon`** (feature `lua54`) as the grammar-aware parser. The plan
  forbids a regex/text scanner as a security boundary; `full_moon` parses the
  real Lua grammar, which is what validates the Cast profile and extracts
  literal imports.

This is the plan's starting preference precisely because Lua 5.4 is the requested
language and has the simpler musl release story.

## Rejected alternative

**Luau (`mlua` + Luau).** Rejected for the initial selection because Luau is
Lua 5.1-derived, not Lua 5.4, and carries C++17 build/link implications. Its
built-in sandbox and memory limiter are convenient, but its default sandbox is
still broader than Cast's empty-by-default policy, so it offers no decisive
safety advantage over a manually stripped Lua 5.4 environment. Luau remains the
documented fallback if Lua 5.4 ever cannot satisfy the shared evaluator policy
without fragile runtime patches.

## What the spike proved (`crates/lua_engine_spike`)

The throwaway spike (`make lua-engine-spike`, `make lua-engine-spike-release`)
establishes, in both debug and release profiles:

- **Empty-by-default sandbox.** `StdLib::NONE` still leaves the Lua base library
  (`load`, `dofile`, `setmetatable`, `print`, …) on the global table, so each
  authored chunk is evaluated in its own controlled `_ENV` table with no
  metatable and no path back to `_G`. No forbidden global is reachable.
- **Host-latched resource limits.** A monotonic-deadline debug hook interrupts an
  unbounded loop; `set_memory_limit` bounds allocation (verified: ~71 MiB RSS
  under a runaway table-growth loop); non-tail recursion faults as a caught error
  rather than a host stack overflow. The runtime remains usable after each.
- **Grammar-aware literal imports.** `full_moon` extracts `cast.import("...")`
  literals in source order and rejects computed, concatenated, or unparseable
  import arguments.
- **Determinism.** With no clock, randomness, or nondeterministic iteration
  exposed, repeated evaluation of the same source is byte-stable.

The Gluon 64-KiB stack setting has no assumed Lua equivalent; a selected-engine
call-depth bound (or an explicit engine-neutral call-depth policy version) is
required when the production `crates/lua_config` adapter is built, and must not
rely on an undocumented engine default.

## Endpoint decision

**Working decision: Endpoint A — Lua-only (the plan's recommendation).** After
every Lua domain, generated artifact, example, active configuration, and
archived state is proven through a dual-runtime migration window, the Gluon
adapters, dependencies, and `.glu` corpus are removed, leaving one user language
and one long-term runtime/security matrix.

This is revisitable before public `.lua` registration (Phase L3); it does not
change the Lua adapter architecture, which is identical for Endpoint A and B
through production parity. If dual support is chosen instead (Endpoint B), both
dependency trees, threat surfaces, ABI implementations, and test matrices become
permanent.

## Not yet done (stops here for review)

Per the plan's first implementation batch, work stops before creating the
production `crates/lua_config` adapter. Still open:

- register the Lua ABI/runtime source roots in semantic implementation
  fingerprints once the ABI tree exists;
- a documented selected-engine call-depth bound;
- musl-target release execution and dependency/license audit
  (`make lua-release-test`, `make lua-dependency-audit`).
