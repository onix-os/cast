# Plan: Language-Agnostic Authoring ABI (Gluon/Lua fully independent)

## Problem

The domain (`PackageSpec`) and the per-engine *decoders* are already
language-agnostic Rust — that is why Lua can already represent and load any
package (verified: `every_gluon_recipe_example_round_trips_through_lua`).

But the **authoring ABI is implemented as Gluon source**:

- `crates/stone_recipe/gluon/package.glu` (~725 lines): `mk_package`, `meta`,
  `source`, `dep`, `step`, `phase`, defaults, `when`, output constructors…
- `crates/stone_recipe/gluon/builders/{cmake,meson,cargo,autotools}.glu`:
  functions that expand `cmake.builder {flags}` into typed phases/steps.

These run **inside the Gluon VM**. Lua has the same Rust decoder but no
equivalent module content, so Lua can only author the fully-expanded data table,
not call the helpers. **Remove Gluon and authoring dies.** The languages are not
independent.

## Decision (user, 2026)

**Lift the authoring ABI into shared Rust.** Both languages author minimal,
direct data; Rust fills defaults and lowers builder requests into typed steps —
once, shared. Gluon and Lua become thin, interchangeable syntaxes over a
Rust-owned authoring layer. Removing either leaves full authoring intact.
Dropped: the "custom builder as a config-language module" path (the explicit
`custom`/`shell` escape hatch — authoring an explicit `BuilderSpec` as data —
stays). No per-language ABI duplication.

## Target architecture

```text
stone.glu  (minimal Gluon record)  ─┐
                                     ├─> AuthoredPackage (Rust DTO, decoded by the engine)
stone.lua  (minimal Lua table)     ─┘        │
                                             ▼
                              lower()  [shared Rust: defaults + builder→steps]
                                             │
                                             ▼
                                    PackageSpec  (unchanged frozen domain, typed steps)
```

- **Domain unchanged.** `PackageSpec`/`BuilderSpec`/`StepSpec` (typed
  `CMakeConfigure{flags}`, `CargoBuild{features}`, …) stay the frozen contract.
- **`AuthoredPackage` (new, Rust).** The minimal authoring surface: `meta`,
  `sources`, a `BuilderRequest`, dependency lists, and *optional* `outputs`,
  `options`, `profiles`, `tuning`, `architectures`, `hooks`, `emul32`, `mold`.
  Missing optionals default in Rust.
- **`BuilderRequest` (new, Rust).** `enum { Cmake(CmakeConfig) | Meson | Cargo |
  Autotools | Custom(BuilderSpec) }`; each config carries exactly what today's
  `.glu` builders take (flags / features / run_tests / binaries).
- **`lower(AuthoredPackage) -> PackageSpec` (new, shared Rust).** Fills defaults
  (root output from `pname`, default `options`, empty `hooks`, all-hooks
  support) and lowers each `BuilderRequest` into `BuilderSpec` (the tools +
  environment + typed step phases currently in the `.glu` builders). THIS is the
  shared ABI.
- **Engines.** `gluon_config` and `lua_config` each decode their native value
  into `AuthoredPackage` through the shared serde/domain decode, then call the
  same `lower`. Both share defaults + builder lowering.
- **Removed.** `gluon/package.glu` and `gluon/builders/*.glu`. The Gluon ABI
  modules no longer exist; Gluon authors the `AuthoredPackage` shape natively.

## Authored surface (both languages, minimal + identical shape)

```lua
-- stone.lua
return {
  meta = { pname = "hello", version = "1.0.0", release = 1,
           homepage = "https://example.invalid/hello", license = { "MIT" } },
  sources = { { kind = "archive", url = "…", sha256 = "…" } },
  builder = { kind = "cmake", flags = { "-DBUILD_TESTS=ON" } },
  native_build_inputs = { { kind = "pkgconfig", value = "openssl" } },
  build_inputs = { { kind = "package", value = "zlib" } },
  -- outputs / options / profiles / tuning default in Rust
}
```

```gluon
-- stone.glu (no `import! cast.package.v3`)
{
  meta = { pname = "hello", version = "1.0.0", release = 1,
           homepage = "https://example.invalid/hello", license = ["MIT"] },
  sources = [ { kind = "archive", url = "…", sha256 = "…" } ],
  builder = { kind = "cmake", flags = ["-DBUILD_TESTS=ON"] },
  native_build_inputs = [ { kind = "pkgconfig", value = "openssl" } ],
  build_inputs = [ { kind = "package", value = "zlib" } ],
}
```

## Constraints (inherited from PLAN.md)

- Preserve evaluation fingerprints/provenance and the `DerivationPlan` identity.
  The recipe-source fingerprint stays; the *imported ABI module* fingerprints
  disappear because there are no imported ABI modules — record this ABI-version
  bump in provenance so old derivation IDs are not silently reused.
- Keep the repository buildable between slices; commit every few cohesive
  changes; tests beside behavior.
- Both languages must remain at full parity (the round-trip test keeps proving
  it) AND both must be independently sufficient for authoring.

## Slices

1. **Rust builder lowering + `BuilderRequest`.** Define `BuilderRequest` +
   configs; port `cmake/meson/cargo/autotools.glu` logic to a Rust
   `lower_builder(BuilderRequest) -> BuilderSpec`. Unit-test each builder lowers
   to exactly the steps the `.glu` module produced today (golden parity).
2. **`AuthoredPackage` DTO + `lower()`.** Define the minimal authored DTO and
   `lower(AuthoredPackage) -> PackageSpec` (defaults + builder lowering). Unit
   tests: minimal input → same `PackageSpec` a current recipe produces.
3. **Rewire the Gluon engine.** `GluonPackageEvaluator` decodes `AuthoredPackage`
   (native Gluon record) and calls `lower`, instead of decoding the pre-lowered
   value. Convert the Gluon example corpus to the minimal form. Delete
   `package.glu` + `builders/*.glu`. Keep the whole corpus green.
4. **Rewire the Lua engine.** `LuaPackageEvaluator` decodes `AuthoredPackage`
   (native Lua table) via the same DTO + `lower`. Regenerate the Lua corpus.
5. **Parity + independence proofs.** Keep the Gluon⇄Lua round-trip test; ADD an
   *authored* (hand-written, non-generated) Lua recipe test proving Lua authors
   a full package with no Gluon involvement, and the symmetric Gluon one.
6. **Provenance + docs.** Bump the package ABI version, drop imported-ABI-module
   fingerprints from provenance cleanly, update the authoring guide to the
   minimal-data surface for both languages.

**Exit:** either language can be deleted and the other still authors, checks,
evaluates, plans, and freezes every corpus package to identical `PackageSpec`
and derivation IDs. No authoring logic lives in a config language.

## Progress (2026-07)

- **Slice 1 — done** (`de24c664`). `BuilderRequest` + `lower_builder` in
  `package/builder_lowering.rs`; golden parity tests vs the `.glu` builders.
- **Slice 2 — done** (`677fb5bc`, `79a08ad5`). `AuthoredPackage` + `lower` +
  `default_output_set` in `package/authored.rs`; byte-parity test proves the
  shared Rust defaults reproduce `cast.package.v3` exactly.
- **Slice 3 — decode path + proof done** (`338e96ad`). `GluonAuthoredPackage` +
  `GluonBuilderRequest` DTOs + `From` + `GluonPackageEvaluator::evaluate_authored`
  + the `cast.authored.v1` types-only prelude. *Not yet:* making `evaluate_authored`
  the default engine path, rewriting the Gluon corpus to minimal form, deleting
  `package.glu` + `builders/*.glu`.
- **Slice 4 — decode path + proof done** (`b26e026d`). `LuaAuthoredPackage` +
  `LuaBuilderRequest` (serde `#[serde(default)]` → true minimal authoring) +
  `LuaPackageEvaluator::evaluate_authored`. *Not yet:* default-path switch +
  corpus regen.
- **Slice 5 — done.** Both independence proofs pass: a hand-authored *minimal*
  recipe in each language decodes through the shared `lower` to the exact
  `PackageSpec` of the equivalent `AuthoredPackage`
  (`a_minimal_authored_lua_recipe_lowers_through_shared_rust`,
  `a_minimal_authored_gluon_recipe_lowers_through_shared_rust`). Neither recipe
  imports `cast.package.v3`; authoring logic is entirely in shared Rust.
- **Slice 6 — not started.** ABI-version/provenance bump + authoring-guide docs.

## Activation progress (2026-07, in flight)

- **Full `cast.authored.v1` prelude** shipped (`gluon/authored.glu`): the
  type+constructor layer lifted from `package.glu` with NO logic; `Custom` builder
  restored to `GluonBuilderRequest`. Rich + minimal authored proofs pass for both
  languages.
- **Entire 64-package Gluon corpus migrated to authored form** — 62 via a parallel
  workflow (verified against a per-package `PackageSpec`-equality oracle) + 2 hand
  exemplars. Every package produces the byte-identical `PackageSpec` as its legacy
  recipe.
- **Corpus is the default path.** `GluonPackageEvaluator` now dispatches
  try-authored-first, fall-back-to-legacy (`evaluate_dispatched`). Migrated corpus
  runs through the shared authored layer; the legacy `cast.package.v3` path remains
  only as a bridge for not-yet-migrated recipes. Workspace green (modulo 3
  pre-existing flaky `mason` git-fixture tests, flaky on baseline too).

## COMPLETE (2026-07-25) — legacy ABI deleted

- **All legacy recipes migrated** (each verified against a `PackageSpec` oracle):
  64 docs package recipes, **33 `tests/fixtures/gluon` recipes**,
  `stone_recipe/tests/{package_v3,builders_v2}.rs`, `composed-stone`+`package_policy`,
  `authored-source.glu`, and **10 `mason` inline-recipe files** (identity/module-list
  assertions shifted `cast.package.v3` → `cast.authored.v1`, the intended provenance
  change).
- **`mason/src/draft.rs`** production emitter rewritten to authored form; its
  `drafted-stone.glu` golden regenerated.
- **`package.glu` (725 lines) + `builders/*.glu` DELETED**, plus the `gluon.rs`
  legacy decoder: `GluonPackageSpec` + its `From`, `evaluate_package`, the five
  embedded-module registrations, and the dispatch bridge (trait methods now call
  `evaluate_authored_package` directly). Shared `Gluon*` sub-DTOs kept — reused by
  `GluonAuthoredPackage`. `PACKAGE_ABI_VERSION` + `GLUON_*_ABI` consts deleted (unused).
- Added along the way: `outputs.with_root` overlay lifted to shared Rust
  (`default_output_set_with_root`), a typed `output_with` for policy composition,
  and a real dispatch-correctness fix (authored `Conversion`/validation errors were
  being masked by the legacy fallback).
- **Kept as stable identifiers** (NOT renamed, to avoid needless derivation-ID
  churn): the builder-kind labels `"cast.builders.cmake.v2"` etc. in
  `mason/src/planner/identity.rs`; `package_v3.rs`’s retired-alias rejection list.
- `@generated` Lua corpus unchanged (identical `PackageSpec` → identical emitted Lua).

**Exit criteria met:** no authoring logic lives in a config language; either Gluon
or Lua can author, check, evaluate, plan, and freeze every package to identical
`PackageSpec`. `stone_recipe` + `mason` green (modulo 3 pre-existing flaky `mason`
git-fixture tests + the offline-fixture test, all flaky on baseline).
