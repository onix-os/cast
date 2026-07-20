# Replacing Gluon with embedded Lua — Feasibility & Architecture Report

Based on a four-track audit (the `gluon_config` embedding layer, every downstream consumer plus the
`.glu` corpus, a quantitative footprint/dependency analysis, and external research on the 2026
embedded-Lua ecosystem), 2026-07-20. No code was changed as part of this analysis.

## 1. Executive summary

- **Yes, it is feasible — and the corpus makes it easier than it looks.** Cast uses Gluon as a
  *typed record-composition DSL*, not a general functional language: across all 202 `.glu` files,
  `let` + records + record-update (`.. base`) dominate; pattern matching appears in **7 files**,
  recursion in **1**, arithmetic in **0**. Everything in actual use maps onto Lua tables plus a
  small host-provided combinator library.
- **The security/determinism machinery is mostly language-agnostic and survives.** The hardened
  source loader (openat2/TOCTOU, 604 lines), limits, deadline watchdog, fingerprint provenance, and
  fragment/layering system in `config` do not depend on Gluon and carry over nearly unchanged.
- **The one genuine loss is sound static typing.** Gluon's HM type system makes missing/unknown
  config fields *compile errors* — documented as load-bearing. A Lua stack replaces this with
  two weaker layers: Luau `--!strict` analysis (advisory, CI-side) + Rust-side serde validation
  with `deny_unknown_fields` (runtime, but at evaluation time — which for config *is* load time).
  This recovers ~90% of the practical value, not the soundness.
- **The strategic case is supply-chain health.** Gluon is a bus-factor-1 project (releases
  2021-10 → 2023-09 → 2026-07-08) with a history of rustc breakage (ICE issue #951), ~79k all-time
  downloads, and it drags **52 of 514 lockfile crates** including lalrpop (the workspace's
  compile-time cliff), syn 1.x, futures 0.1, and an unmaintained salsa fork. mlua is at 0.12.0
  (2026-07-05), ~5.6M downloads, actively maintained, with Luau as a first-class sandboxed target.
- **Recommended stack if migrating:** `mlua 0.12` + **Luau** + `vendored` + `serde`, with
  `Lua::sandbox(true)`, `set_memory_limit`, interrupt-based deadlines, OS stdlib stubbed out, and
  serde-typed extraction. Luau is sandboxed by design: no `io`, no `package`, no `__gc`,
  `os` reduced to four functions, readonly builtin globals.
- **Verdict: a good idea, but not an urgent one.** Gluon 0.18.3 (July 2026) works on current Rust
  today. The migration is ~4.5–7 engineer-months touching ~11k dedicated Rust lines and ~12.6k
  `.glu` lines. Do it for the long-term health, contributor familiarity, and build-time wins —
  not because anything is on fire. If determinism outranks Lua familiarity, Starlark deserves one
  honest look first (§6).

## 2. What exists today — the surface a replacement must cover

### 2.1 The embedding layer (`crates/gluon_config`, 3,031 lines)

All VM construction is in one function (`evaluator.rs:260-279`), deliberately minimal:

- **Empty VM** (`RootedThread::new()`, not `gluon::new_vm()`) — no stdlib, no prelude, no IO
  (`set_implicit_prelude(false)`, `set_use_standard_lib(false)`, `set_run_io(false)`).
- **Import control**: custom macro with a `RestrictedImporter`; filesystem search paths cleared;
  13 forbidden namespaces (`std.fs/io/process/env/random/http/thread/channel/reference/effect/
  debug/path/st.reference`) checked at graph build *and* compile time; only explicitly embedded
  in-memory ABI modules plus quoted relative paths under an explicit `SourceRoot`; `..`/absolute/
  NUL rejected; `GLUON_PATH` ignored (test-pinned).
- **Only two optional primitives** ever registered: `std.array.prim`, `std.string.prim` — and even
  those are opt-in per `ImportPolicy`.
- **Resource limits** (`limits.rs`): source 1 MiB, imported file 256 KiB, 64 imports, 2 MiB import
  graph, 32 MiB VM memory, 64 Ki stack, **2 s total wall-clock deadline** spanning
  load/parse/import/fingerprint/run, enforced by a watchdog thread calling `vm.interrupt()`;
  `catch_unwind` contains panics.
- **Hardened source loading** (`source.rs`, 604 lines): raw `openat2` with
  `RESOLVE_BENEATH|NO_MAGICLINKS|NO_SYMLINKS[|NO_XDEV]`, O_PATH root retention with dev/inode
  identity checks, 11-field metadata "sandwich" around every read, retained intermediate-directory
  witnesses, FIFO-safe non-blocking opens. **Entirely language-agnostic — carries over untouched.**
- **Provenance**: `EvaluationFingerprint` (SHA-256 over source, logical name, explicit inputs, and
  every imported module) — consumers pin contracts on it (e.g. triggers *require* the
  `cast.trigger.v1` import to appear; boot topology requires exactly one import and empty inputs).
- **Error taxonomy**: `Diagnostic` with 7 categories (Parse/Type/Import/Io/Limit/Runtime/Internal),
  8 limit kinds, byte-span extraction from codespan-rendered errors.
- **Evaluation model — the great simplifier**: strict evaluate-once-to-value. One pure expression
  per call, immediately converted via `Getable` into an owned Rust DTO; the VM is dropped. No
  retained threads, no callbacks in either direction, **zero `Pushable`/`Userdata` anywhere** —
  Rust never pushes values into the VM. Rust→script flows as generated canonical source text.

### 2.2 Consumers and domains

| Domain | Runtime path | Author | Entry point |
|---|---|---|---|
| Package recipe | `stone.glu` (+ generated `sources.lock.glu`, `build.lock.glu`) | packager | `stone_recipe::package::evaluate_gluon*` |
| Build policy | `policy.glu` + layer manifests; vendor data in `mason/data/policy/` | vendor/admin | `stone_recipe::build_policy[::layers]` |
| Profiles | `usr/share/cast/profile.d/` → `etc/` → XDG | vendor+admin+user (only writable domain) | `mason::profile` via `config::Manager` |
| Repositories | `usr/share/cast/repo.d/` → `etc/cast/repo.d/` | vendor+admin+generated | `forge::repository::gluon` |
| Transaction/system triggers | `/usr/share/cast/triggers/{tx.d,sys.d}/*.glu` | packagers only (encode is a hard error) | `triggers::evaluate_gluon_with` |
| System intent / snapshot | `/etc/cast/system.glu` / `/usr/lib/system-model.glu` (generated) | admin / cast | `forge::system_model` |
| Boot topology / root fs intent | `/etc/cast/boot-topology.glu`, `root-filesystem.glu` | machine admin | forge boot intent evaluators |

Layering: vendor → admin → user, whole-fragment shadowing by logical name (no value merging),
invalid files are hard errors, `.glu` is the only format (YAML/KDL removed without fallback).

### 2.3 Footprint numbers

- **Dedicated Rust: ~11,100 lines** — gluon_config 3,031 · config 2,629 · stone_recipe 2,789 ·
  forge 1,719 · triggers 393 · mason ~500–800 embedded sections.
- **129 derive sites**, every one exactly `Getable + VmType` (read-only). `GluonOptional`/
  `GluonBool` re-declared in 4 crates (unification opportunity for any successor).
- **13 exported ABI constants / 18 embedded `.glu` ABI modules, 4,397 lines**: `cast.package.v3`,
  `cast.builders.{cmake,meson,cargo,autotools}.v2`, `cast.build_policy.v5`,
  `cast.build_policy.layers.v1`, `cast.trigger.v1`, `cast.system.v1`, `cast.repository.v1`,
  `cast.profile.v1`, `cast.boot_topology.v2`, `cast.root_filesystem.v1`.
- **Corpus: 202 `.glu` files, 12,637 lines** (docs/examples 116/3,989 · test fixtures 68/4,251 ·
  mason data 6/2,516 · embedded ABIs the rest). The embedded ABI files are release-fingerprinted
  by `tools_buildinfo` (`semantic_fingerprint.rs:147`).
- **Dependency graph: 52 of 514 lockfile crates exist only for Gluon**, including lalrpop 0.23
  (build-time cliff), gluon-salsa (ancient salsa fork pulling parking_lot 0.11 / crossbeam-utils
  0.7 / instant), syn 1.0.109 (second syn compile), futures 0.1.31. Est. 1.5–3 min of cold-build
  wall time. An mlua/vendored replacement is ~3–5 crates plus a C/C++ build.

### 2.4 Language-feature census (what scripts actually use)

| Feature | Files / occurrences | Lua mapping difficulty |
|---|---|---|
| `let` bindings | 173 / 744 | trivial (`local`) |
| records | ubiquitous | trivial (tables) |
| record update `.. base` | 111 / 393 | easy — host `merge`/`with` helper |
| `import!` | 164 / 261 (128 = `cast.package.v3`) | easy — restricted `require` registry |
| lambdas | 56 / 253 (mostly inside ABI modules) | trivial |
| `type` declarations | 39 / 170 (ABIs + generated snapshots) | Luau `type` (annotation-only) |
| pattern `match` | **7 / 11** | tagged tables + `if`/dispatch table |
| recursion (`rec`) | **1 file** | trivial (Lua allows it natively) |
| arithmetic | **0** | n/a |
| stdlib (`std.array/string.prim`) | 11 files | Lua `table.insert`/`string` subset |

## 3. Candidate evaluation

### 3.1 mlua + Luau — recommended

- **mlua 0.12.0** (2026-07-05, MSRV 1.88, ~5.6M downloads, active): Lua 5.1–5.5, LuaJIT, Luau;
  `vendored` builds from source; `serde` feature gives `from_value::<T>()` — typed extraction with
  field-level errors; async optional (leave off); `Lua::new_with(StdLib, …)` composes a stdlib
  subset; safe constructor refuses `debug`/`ffi`.
- **Luau** (Roblox, MIT, very active; new type solver GA 2025): **sandboxed by design** — `io.*`
  and `package.*` do not exist, `os.*` reduced to `clock/date/difftime/time` (stub these four),
  **no `__gc` metamethod at all** (kills finalizer nondeterminism/escape class), no bytecode
  loading, `Lua::sandbox(true)` makes builtin globals readonly with per-script environments;
  VM interrupts guaranteed at calls/loop back-edges (maps directly onto the existing watchdog
  deadline); `set_memory_limit` works (maps onto the 32 MiB limit).
- **Gradual typing**: `--!strict` annotations, inference, generics, unions, table types — checked
  by `luau-analyze` as a separate pass. Advisory, not sound; run it in `cast recipe check`/CI.

### 3.2 Other candidates

| Candidate | Status (2026) | Verdict |
|---|---|---|
| mlua + Lua 5.4 | fine, mainstream | Weaker sandbox: `__gc` exists, io/os must be stripped manually, no readonly globals. Choose only if Luau's C++ toolchain is unacceptable. |
| piccolo (pure-Rust VM) | 0.3.3 (2024-06), experimental | Conceptually ideal (memory-safe, fuel metering) — not production-ready. Revisit in 2–3 years. |
| rlua / hlua / hematita | archived / dead / dead | Not candidates. |
| Teal + tealr 0.11 | active | Typed-Lua compile step over mlua; stricter than Luau annotations but adds a compiler to the pipeline. Niche fallback. |
| Starlark (facebook/starlark-rust 0.14) | active | **Strongest determinism story of anything surveyed** (hermetic by design, Bazel/Buck2 pedigree). Not Lua, no gradual runtime typing — but if the goal is "deterministic config language with a real maintainer," it is the honest alternative to Luau. |
| rhai / rune / nickel / KDL | active | Dynamically typed niche / async-first / closest-in-spirit typed config but small ecosystem / data-only (can't compute — would fit *triggers* alone). |

### 3.3 Gluon status (the do-nothing baseline)

Single maintainer; releases 0.18.1 (2021-10) → 0.18.2 (2023-09) → 0.18.3 (**2026-07-08**, largely
a fix-compilation-on-modern-rustc release); 153 open issues; history of rustc ICEs (gluon#951);
function-call overhead measured 250–290× native (gluon#858 — irrelevant for evaluate-once configs,
indicative of maintenance depth). The `=0.18.3` pin works today; the risk is forward: every future
rustc/edition bump is a coin flip on an effectively dormant dependency.

## 4. Feature mapping — what changes, what carries over

### Carries over unchanged (language-agnostic)

`source.rs` hardened loader and `SourceRoot` model · `Limits` + `deadline.rs` watchdog design ·
`EvaluationFingerprint` scheme (hash Lua sources identically) · `Diagnostic` taxonomy ·
`config::Manager` fragment layering, atomic persistence, rooted/retained-descriptor loading ·
the versioned-ABI-module concept and the fingerprint-must-import-ABI contract · generated-file
markers and refuse-to-overwrite-authored logic.

### Translates mechanically

| Gluon | Lua/Luau replacement |
|---|---|
| record literal | table literal |
| `.. base` record update | `cast.with(base, {…})` host helper (or plain `merge`) |
| `import! cast.package.v3` | `require("cast.package.v4")` against an embedded, allowlisted module registry (custom loader; no filesystem `require` — Luau has no `package` lib to escape through) |
| `GluonOptional` Unset/Set | needs care — see R2 below; recommend explicit sentinel (`cast.unset`) rather than `nil` |
| variants (`| Sqlite | PostgreSql`) | tagged tables `{ tag = "sqlite" }` produced by ABI constructors; `match` → dispatch table |
| 129 `Getable`/`VmType` DTO derives | serde `Deserialize` structs via `mlua::LuaSerdeExt::from_value` with `deny_unknown_fields` — also unifies the 4 duplicated `GluonOptional`/`GluonBool` copies |
| HM static typing | Luau `--!strict` + `luau-analyze` gate (advisory) + serde validation (authoritative) |
| codespan type-error spans | mlua line/col runtime errors + `serde_path_to_error` field paths + analyzer diagnostics |
| canonical Gluon pretty-printers (4 hand-rolled) | one shared canonical-Lua emitter (sorted keys, escaped strings, marker header) |
| `std.array.prim` / `std.string.prim` opt-ins | curated `table`/`string` subsets exposed per policy |

### Determinism checklist for the Lua evaluator

1. Luau + `Lua::sandbox(true)`; do not link `io`/`package` (absent in Luau anyway); stub or remove
   the four remaining `os` functions; no `math.random` (or fixed seed).
2. `set_memory_limit(32 MiB)`; interrupt handler wired to the existing single-deadline watchdog.
3. **Iteration order**: `pairs` order is unspecified (Luau: sequential over 1..#n, unordered
   beyond). Host ingests tables via serde into ordered structures (BTreeMap/sorted Vec) and
   canonicalizes before hashing; lint recipes for order-dependent constructs; provide `spairs` if
   scripts ever need to iterate maps.
4. No `__gc` (Luau), no bytecode loading, no address leakage in output (`tostring` of tables must
   never reach persisted artifacts — the canonical emitter already owns serialization).
5. Precedent that "Lua, but deterministic" is solved: Factorio's lockstep-deterministic Lua and
   factorio-mlua; xmake/premake/LuaRocks for Lua-as-build-config; WezTerm/Yazi for
   mlua-in-Rust-hosts with typed config extraction.

## 5. Migration design sketch

1. **`lua_config` crate** mirroring `gluon_config`'s public API (`Evaluator`, `Source`,
   `SourceRoot`, `Limits`, `Evaluation<T>`, `EvaluationFingerprint`, `Diagnostic`) so consumers
   swap imports, not architecture. Bump `CONFIGURATION_ABI_VERSION`/`EVALUATOR_POLICY_VERSION`.
2. **ABI modules re-authored in Luau strict mode** with version bumps (`cast.package.v4`,
   `cast.trigger.v2`, …) — the versioned-ABI system was built for exactly this kind of break.
3. **DTO layer**: replace the 129 derives with serde structs (`deny_unknown_fields`), one shared
   `Unset`-sentinel handling, keep the existing `TryFrom` semantic-validation layers untouched.
4. **Corpus translation**: a one-shot `.glu → .lua` translator covers the mechanical 90%
   (records/let/imports/record-update); the 7 match-using files and the ABI modules are rewritten
   by hand. The project's own precedent applies: hard cutover, no dual-format fallback (YAML/KDL
   were removed the same way) — but keep a feature-gated dual-runtime window *internally* for
   differential testing: evaluate all 202 files under both runtimes, require identical DTOs.
5. **Docs**: `gluon-configuration.md` (639 lines) and 116 examples re-authored; `cast recipe
   check` gains a `luau-analyze` pass.

## 6. Risks

1. **R1 — Loss of sound static typing** *(the big one)*. Today a missing field is a type error
   with a span; under Lua it is a serde validation error at evaluation time. `deny_unknown_fields`
   + `serde_path_to_error` + Luau strict annotations on the ABI modules recover most of the
   developer experience, but the guarantee weakens from "checked before evaluation" to "checked at
   evaluation" — acceptable for config (evaluation *is* load), a real regression for authoring
   feedback. Mitigation: ship analyzer-checked ABI type definitions and make `recipe check` run
   the analyzer.
2. **R2 — `nil` semantics vs `Unset/Set`.** Lua tables cannot distinguish "absent" from "nil";
   Gluon's explicit `optional.unset/set` pattern must map to a host sentinel (`cast.unset`) or
   field-presence conventions, and serde must be taught the sentinel. Easy to get subtly wrong;
   pin with contract tests.
3. **R3 — C++ enters the build.** Luau is C++ (vanilla Lua is C — the workspace already builds C
   via bundled sqlite/zstd, but C++ is new). Pure-Rust piccolo would fix this and is not ready.
   Accept, or choose Lua 5.4 with a weaker sandbox.
4. **R4 — Error-reporting regression.** codespan-quality parse/type diagnostics with byte spans
   become Lua runtime messages with line numbers. The `Diagnostic` shape survives, its content
   gets coarser. Analyzer diagnostics partially compensate.
5. **R5 — Security-parity burden of proof.** The gluon evaluator's threat posture is enforced by
   ~30 contract tests (import denial, deadline, memory, GLUON_PATH, symlink escapes…). Every one
   must be re-pinned against the Lua evaluator; mlua contains substantial internal `unsafe` and
   Luau's sandbox is fuzzed-not-proven. The threat model (packager-authored scripts, defense in
   depth) is unchanged, but the evidence must be rebuilt.
6. **R6 — Rewrite volume and provenance churn.** 202 script files, 18 ABI modules, 639-line canon
   doc, release fingerprints over embedded ABIs (`tools_buildinfo`), Makefile gluon suites, and 4
   hand-rolled pretty-printers. None hard; all wide.
7. **R7 — Iteration-order nondeterminism** creeping in through `pairs` in future recipes — needs
   a standing lint plus canonical host-side ingestion (see checklist), not a one-time fix.
8. **R8 — The counterfactual is viable.** Gluon 0.18.3 just shipped; nothing is broken today. The
   migration is justified by trajectory (bus factor, rustc-breakage history, 52-crate tree, build
   time, contributor familiarity), not by an outage. Decide on strategy, not urgency.

## 7. Effort estimate

| Workstream | Size | Rough effort |
|---|---|---|
| L0 · `lua_config` core (evaluator, sandbox policy, limits/deadline/interrupt, fingerprints) | M | 4–6 wk |
| L1 · DTO conversion: 129 derives → serde across 5 crates, sentinel design, unify Optional/Bool | M | 3–5 wk |
| L2 · ABI modules ×18 re-authored in Luau strict + version bumps | M–L | 4–6 wk |
| L3 · Corpus: translator + 202 files + 116 doc examples + fixtures | L | 4–8 wk |
| L4 · Canonical-Lua emitter replacing 4 pretty-printers (snapshots, locks, generated fragments) | S–M | 2–3 wk |
| L5 · Differential dual-runtime harness + security contract re-pinning + determinism proofs | M | 2–4 wk |
| L6 · Docs, CLI (`recipe check` analyzer pass), Makefile suites | S–M | 2–3 wk |

**Total: ~4.5–7 engineer-months**, largely parallelizable after L0/L1. Payoff: −52 lockfile
crates, the lalrpop cold-build cliff gone, a maintained runtime with a large talent pool, and a
scripting language contributors already know.

## 8. Recommendation

Migrate — but as a planned strategic move, not a fire drill, and only with these commitments:

1. **mlua 0.12 + Luau, vendored, serde**, sandbox mode on, memory/interrupt limits wired to the
   existing `Limits`/deadline design; Lua 5.4 only if the C++ toolchain is a hard blocker.
2. **Keep the entire non-VM architecture** (source hardening, fingerprints, fragment layering,
   versioned ABIs, generated-file discipline) — it is the actual crown jewel and it is
   language-agnostic.
3. **Treat typing as a two-layer contract**: serde `deny_unknown_fields` as the authoritative
   gate, `luau-analyze --!strict` as the authoring-time experience. Write this down in the docs as
   an explicit, honest weakening of the old guarantee.
4. **Differential-test the cutover** across the full 202-file corpus before deleting Gluon, then
   cut hard (project precedent: no fallback formats).
5. If during design review the team weighs determinism above contributor familiarity, evaluate
   **Starlark** in a one-week spike before committing — it is the only surveyed option whose
   determinism story is *stronger* than the current Gluon setup, at the cost of not being Lua.
