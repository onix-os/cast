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

## Encoding decision: empty sequences

Lua cannot distinguish an empty array from an empty map — both are written `{}`
— and mlua resolves a bare empty table to a map. Read through `serde`'s
internally-tagged enum machinery (which buffers the whole value via
`deserialize_any` before dispatching on the tag), an empty `Vec<_>` field then
saw a map and failed with `invalid type: map, expected a sequence` — for example
a trigger handler with no arguments.

Because this encoding represents every domain map as a `Vec<{key, value}>` and
every struct/variant with explicitly named fields, a table with no entries is
unambiguously an empty *sequence*. The adapter therefore walks the validated
value tree and stamps mlua's array metatable onto every empty table, so
mlua's deserializer (`t.raw_len() > 0 || t.is_array()`) routes it to a sequence.
Authored fragments may use empty lists freely, including inside tagged variants.
This runs after the cycle check, so the walk terminates.

## Implemented so far

- Phase L0 engine spike (`crates/lua_engine_spike`) and this selection record.
- Phase L1 isolated `crates/lua_config` adapter (parser profile, capability
  allowlist, value-tree bounds, host-latched limits, tagged option encoding).
- Phase L2–L4 domain adapters with differential Gluon/Lua parity tests:
  `triggers::lua`, `mason::profile::lua`, `forge::repository::lua`.
- Phase L3 loader registration: `.lua` triggers dispatch by extension through
  the shared config layer alongside `.glu`.

## Domains needing neutral-conversion separation first

The config-style domains (triggers, profiles, repositories, system model,
build-policy layers) were already split so that DTO → domain conversion and
validation live in engine-neutral code (`decode_specs`, `into_domain`,
`spec::validate`), which both engines call. Adding the Lua adapter there is
purely a new DTO plus `From` impls.

Two shapes appear in the recipe domains:

- The build lock's domain types were *already* engine-neutral: the rich semantic
  `BuildLock::validate` is a method on the domain value, and the Gluon
  `TryFrom<GluonBuildLock>` only performs the structural `i64 → u32` conversion.
  So the Lua adapter needed no parallel spec — deriving `serde::Deserialize`
  (with the tagged encoding on the origin/role enums) on the domain types let
  Lua decode straight into `BuildLock` and reuse `validate`, leaving the Gluon
  path untouched. This is the cheapest case.

- Domains whose *validation* still lives inside a `TryFrom<GluonX>` (Gluon-typed
  input, no neutral method) genuinely need that logic extracted into an
  engine-neutral function or a domain method first.

### Package recipe and full build policy

Both reach their domain values through an infallible `From<GluonX>` (the package
does `PackageSpec::from(evaluation.value)`, the policy `evaluation.value.into()`),
so they are the *neutral* shape like the build lock — no validation extraction is
required. Two things still make them large, multi-day slices rather than quick
serde-derive additions:

1. **Scale.** `build_policy/mod.rs` alone defines ~44 spec types; the package
   recipe is comparable. Every transitively referenced type must derive
   `Deserialize` with a matching tagged encoding.
2. **Tuple variants throughout.** Serde's internally-tagged `#[serde(tag =
   "kind")]` encoding (used by every Lua enum adapted so far — build-lock's
   origins, the layer operations) does **not** support tuple/newtype variants.
   The build policy is full of them, in both the patch and the core spec:
   `ValuePatch<T>` = `Keep | Set(T)`, `ArrayPatch<T>` = `Keep | Replace(Vec<T>)`
   `| …`, `TextSpec` = `Literal(String) | Context(_) | Concat(Vec<Self>)`,
   `BuildToolSpec` = `Package(String) | Binary(String) | …`. None of these can
   gain the uniform tagged encoding from a derive. Each needs a dedicated Lua DTO
   (a struct-variant mirror such as `Set { value: T }` / `literal { value }`)
   with `From` conversions into the domain enum — a parallel DTO tree, not a
   derive. `lua_config::LuaPatch` covers the `ValuePatch` shape; the rest are new.

This is the crucial difference from the build lock, whose enums were all
struct/unit variants and so needed only derives. Because tuple variants pervade
the build policy (and, by the same `From`-based shape, likely the package
recipe), those two domains require a hand-written DTO tree with matching tagged
encodings — the multi-day work the estimate reflects, and the reason they were
not adaptable this session by the cheap serde-derive route.

The verification cost is also real: an all-`Keep` patch decodes without
exercising any nested type's encoding, so a meaningful parity test must author
substantial populated values (the authored policies are ~600 lines of `.glu`).
These are the reasons the recipe domains are estimated in engineer-weeks, not the
hours the config-style domains took.

## Done

Every declaration domain now decodes through Lua to the shared Rust value with a
differential parity test: triggers, repositories, profiles, system model,
build-policy layers, the full build policy (and its patch overlay), the build
lock, the package recipe, and both machine-local boot intents. Repositories,
profiles, and triggers dispatch `.lua` by extension through the real config
loader; the two boot intents are wired into their fixed retained `etc/cast/*`
slots with the same retention, double-revalidation, and strict fingerprint
contract as Gluon. The authored dialect, sandbox, import rules, encoding, file
authority, and provenance are documented in
[`../lua-configuration.md`](../lua-configuration.md).

## Still open

- the full 218-reference `.glu` corpus + 132 documentation examples paired with
  reviewed `.lua` forms (Phase L7);
- the crash-safe installed-state SQLite migration bridge and release-parity
  gates (Phase L8, `make lua-release-test` / `make lua-installed-state-test`);
- Endpoint finish — Gluon removal (L9A) or permanent dual support (L9B);
- musl-target release execution and dependency/license audit
  (`make lua-dependency-audit`).
