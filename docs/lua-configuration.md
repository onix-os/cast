<!-- SPDX-License-Identifier: MPL-2.0 -->

# Lua configuration

Lua is the second declaration engine. It sits behind the same engine-neutral
declaration core as Gluon: authored `.lua` sources are decoded into the exact
same shared Rust domain values, validated by the same neutral logic, and
committed through the same file authorities. Only the *engine* differs — and it
differs deliberately, so an authored `.lua` file and an equivalent authored
`.glu` file normalize to identical domain values while carrying distinct
evaluation identities.

This guide describes the actually-selected dialect, sandbox, import rules, value
encoding, file authority, and provenance. It is the Lua counterpart to
[`gluon-configuration.md`](./gluon-configuration.md); the engine-selection
rationale lives in
[`architecture/lua-declaration-engine.md`](./architecture/lua-declaration-engine.md).

## Selected dialect and runtime

- **Language:** Lua 5.4, via the `mlua` crate with a vendored interpreter
  (features `lua54`, `vendored`, `serialize`). No LuaJIT, no Luau.
- **Parser/linter:** `full_moon` (feature `lua54`) enforces the declaration
  profile before evaluation.
- **Selection:** a `LanguageSpec` with language id `lua`, engine id `lua`
  version `5.4.7`, extension `lua`, and source profile `declaration-v1`.

The engine descriptor is part of every evaluation identity, so a Lua identity is
never equal to a Gluon identity even for byte-identical domain output.

## Restricted evaluator (the declaration profile)

A declaration is a *value*, not a program. The profile rejects dangerous or
non-deterministic language constructs at parse time, before the sandbox ever
runs a chunk. The `full_moon` visitor rejects:

- **loops** — `while`, `repeat`, numeric `for`, and generic `for`;
- **post-construction mutation** — assignment / reassignment of a binding;
- and the profile is intentionally minimal: a later profile version may relax it
  as translated corpus needs are proven, but authored declarations must not
  depend on that today.

The sandbox itself is an empty controlled `_ENV`: the base library globals
(`load`, `setmetatable`, `require`, `os`, `io`, …) are unreachable from an
authored root. The only visible binding is `cast.import`. Memory is bounded with
`set_memory_limit`; wall-clock is bounded by an instruction-count hook driven
from the caller-owned evaluation deadline, so a runaway chunk is interrupted
rather than hanging the host.

### Imports

- The single import primitive is `cast.import("<module>")`.
- Every import is resolved and bounded by the shared module graph *before* the
  root chunk runs; an unresolved name is an internal invariant break, not a
  runtime lookup.
- Import cycles are rejected. There is **no cross-language import**: a `.lua`
  source cannot import a `.glu` module or vice versa.
- Machine-local closed declarations (boot topology, root filesystem) import
  nothing at all — their evaluation contract requires an empty module set.

### Default resource limits

The neutral `Limits` bound source bytes, explicit-input bytes, imported-file
bytes, import count, import-graph bytes, and evaluation timeout. Adapters that
run under a caller-owned budget (the machine-local boot intents) derive these
from the budget policy and the budget's absolute deadline, so the typed
evaluation boundary cannot substitute a fresh relative timeout.

## Value encoding

Because Lua has one aggregate type (the table), the encoding is explicit and
uniform so every domain agrees on it:

- **Records** are tables with named string keys: `{ name = "…", release = 1 }`.
- **Sequences** are array tables: `{ "a", "b" }`. An empty sequence is written
  `{}`; the adapter stamps mlua's array metatable onto empty tables so a bare
  `{}` decodes as an empty *sequence*, never an empty map.
- **Closed variants** are internally tagged: `{ kind = "…", … }`. For example a
  dependency is `{ kind = "binary", value = "cc" }` and a build step is
  `{ kind = "cmake_build" }`. Tuple/newtype domain variants are encoded as
  struct variants (`{ kind = "literal", value = "…" }`) so the uniform tag
  applies.
- **Options** use the tagged encoding `{ kind = "none" }` /
  `{ kind = "some", value = … }` (`LuaOption`), kept distinct from an absent
  field so an intentional value is never confused with a default.
- **Patches** (build-policy overlays) use `{ kind = "keep" }` /
  `{ kind = "set", value = … }` for a total value patch and
  `{ kind = "keep" | "replace" | "prepend" | "append", values = { … } }` for an
  ordered-array patch.
- **Domain maps** are encoded as sequences of `{ key = …, value = … }` records,
  never native Lua maps — which is what makes the empty-table-as-sequence rule
  unambiguous.

An authored root ends by returning its value: `return { … }`.

## Registered domains

Each declaration domain reaches the same shared Rust value through both engines.
The following domains have a Lua adapter with a differential parity test proving
the Lua and Gluon forms normalize to equal values:

| Domain | Shape | `.lua` real-loader wiring |
| --- | --- | --- |
| Triggers (`tx`/`sys`) | read-only, extension dispatch | yes |
| Repositories | fragment map | yes |
| Profiles | fragment map | yes |
| System model | semantic snapshot | evaluator |
| Build-policy layers | ordered manifest, explicit inputs | evaluator |
| Build policy (+ patch) | large recipe policy | evaluator |
| Build lock | canonical lock | evaluator |
| Package recipe | full recipe | evaluator |
| Boot topology | machine-local closed intent | fixed-path slot |
| Root filesystem | machine-local closed intent | fixed-path slot |

Repositories, profiles, and triggers are selected by file extension through the
shared config loader: a `.lua` fragment is dispatched to the Lua adapter and a
`.glu` fragment to the Gluon adapter, with no content sniffing, fallback, or
cross-language import. The two machine-local boot intents are discovered in
their fixed retained `etc/cast/*` slot by extension and evaluated under the same
retention, double-revalidation, and byte-exact source contract as Gluon; the
slot keeps one canonical logical name regardless of engine.

## File authority

- A generated `.lua` file carries the ownership marker
  `-- @generated by cast. DO NOT EDIT.` as its first line.
- Extension dispatch is exact: only the registered language set (`glu`, `lua`)
  is accepted. Unknown extensions in a fixed slot are simply *not discovered*
  (a hard missing-source error, never a fallback). Non-Lua serialized
  configuration formats — YAML, KDL, JSON as an authored surface — remain
  explicitly unsupported; the loader admits only registered declaration
  languages.
- Generated declaration slots switch language authority transactionally; a
  domain never holds two live authorities at once.

## Fingerprints and provenance

Every evaluation produces an engine-neutral evaluation identity binding the
language, source profile, engine, configuration ABI, evaluator policy, resource
policy, root logical name and source hash, the imported module graph, explicit
inputs, and the final hash — under the domain
`os-tools-declaration-evaluation\0`. The engine descriptor is part of the
identity, so equivalent Lua and Gluon sources are guaranteed to produce
different identities by design. Explicit inputs are hashed into the identity, so
two evaluations that admit different external inputs commit to different
identities even for identical decoded values.

## Endpoint

The migration endpoint — whether Lua ultimately becomes the *sole* declaration
language (Gluon removed after a dual-runtime bridge window) or both engines are
kept permanently — is recorded before `.lua` registration becomes public. It
does not change the Lua adapter architecture, which is identical for either
endpoint through production parity. See the engine-selection record for the
current decision and the rejected alternatives.
