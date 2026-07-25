# Plan: Add Lua Through the Language-Agnostic Declaration Adapter

**Status:** Planned; blocked on [`agnostic_config.md`](agnostic_config.md)
**Priority:** P1 after the P0 foundation
**Effort:** Extra large
**Risk:** High
**Planned against:** `develop` at `5e995525`
**Audit date:** 2026-07-23

This plan starts only after [`agnostic_config.md`](agnostic_config.md) is completely accepted. That
plan extracts Cast's declaration infrastructure, reconnects all existing Gluon behavior through the
first adapter, generalizes storage and all 12 domain boundaries, and makes evaluation identity v2
authoritative.

Lua must then be implemented as a second adapter over those frozen contracts. It must not trigger
another source-loader, config-manager, fingerprint, persistence, or domain-schema redesign.

## 1. Goal and endpoints

### Goal

Add a deterministic Lua declaration profile for packages, build policy, profiles, repositories,
triggers, system intent, boot topology, and root-filesystem intent. A Lua source is loaded and
prepared by the shared core, evaluated by `lua_config`, decoded through the same shared domain
wire/semantic types used by the Gluon adapters, and dropped before downstream planning or mutation.

The architecture after Lua registration is:

```text
                     crates/declarative_config
        shared source / limits / graph / diagnostics / identity v2
                       adapter and codec contracts
                          /                 \
                         /                   \
       crates/gluon_config                   crates/lua_config
          GluonEngine                           LuaEngine
                \                               /
                 \                             /
          per-domain Gluon/Lua typed adapters and codecs
                              |
                    shared Rust domain values
                              |
             existing planners / transactions / `.stone`
```

### Possible final endpoints

#### Endpoint A — Lua-only (recommended)

Keep Gluon during a complete differential and installed-state migration window. After every Lua
domain, generated artifact, example, active configuration, and archived state is proven, remove the
Gluon adapters, dependencies, and `.glu` corpus.

This produces one user language and one long-term runtime/security matrix.

#### Endpoint B — permanent Lua plus Gluon

Keep both adapters as first-class public languages. Selection remains explicit by extension; there
is no content sniffing, fallback, cross-language import, or dual write.

This avoids a forced authored-source conversion but permanently retains both dependency trees,
both runtime threat surfaces, both ABI implementations, and both complete test matrices.

The endpoint must be recorded before Lua becomes public, but it does not change the Lua adapter
architecture. Endpoint A and B share every phase through production parity.

## 2. Inherited foundation contracts

Do not restate or reimplement the P0 architecture. Lua must consume these accepted outputs from
`agnostic_config.md`:

- one descriptor-rooted `Source`/`SourceRoot` implementation;
- shared source/import/graph/resource limits and monotonic deadline budget;
- deterministic prepared-module graph and rooted/embedded resolution;
- adapter/domain-supplied `AbiCatalog` entries binding semantic ABI IDs to the active language's
  immutable implementation bytes and capability requirements;
- stable generic diagnostics;
- validated opaque `LanguageId`, `EngineId`, `LanguageSpec`, `AbiId`, and evaluator policy;
- evaluation identity v2 already used by Gluon and every provenance consumer;
- typed `DeclarationEvaluator<T>` and writable `DeclarationCodec<T>` contracts;
- engine-neutral `config::Manager`, fragment layering, logical-slot collision policy, atomic
  persistence, and one-active-authority rule;
- all 12 shared wire/semantic domain boundaries;
- Gluon as the passing reference adapter.

The accepted P0 evaluator policy is also fixed: each root or fragment has one budget beginning
immediately before descriptor-rooted read and ending after typed decode; each shadowed fragment is
still evaluated under its own budget; and import cycles fail before VM execution. Lua implements
those rules exactly rather than choosing different admission points.

If Lua reveals a missing shared capability, stop and amend/review `agnostic_config.md`. Do not hide
a Lua-only source loader, deadline, module resolver, diagnostic format, fingerprint field, config
store, or persistence path inside `lua_config`.

## 3. Non-negotiable rules

- `.stone` remains the package artifact. Lua does not alter the artifact format or transaction
  architecture.
- Atomic updates, rollback architecture, container-trigger behavior, USR merge compliance, and
  OS/local-configuration separation remain owned by existing Rust layers.
- YAML and KDL remain removed. No legacy parser or fallback is restored.
- There is no Nix-to-Lua translator or Nix compatibility goal.
- Do not read or modify `../bedrock`.
- Work on `develop`; leave `main` untouched and do not leave migration branches behind.
- Use the root Makefile for all relevant actions. Put Lua targets in an imported fragment such as
  `misc/make/lua-tests.mk`; keep the FOL-style root Makefile small.
- Commit each green slice. Runtime, pilot domain, fragment domains, recipes, system intent, corpus,
  installed-state migration, and final Gluon removal must not be one commit.
- Do not change release/version metadata unless separately requested.
- Do not add or restore `.clippy.toml`, `.rustfmt.toml`, or `.typos.toml`.
- Do not use Python to edit files.
- No file may exceed 1,000 lines. Split by function before crossing the limit and use descriptive
  names, never numbered chunks.
- Do not wrap Git, Make, or compilation in `timeout`. Bound only execution of an already-built
  evaluator/application or remote VM operation that could hang.
- Never run ESP, BOOT, block-device, activation, rollback, reboot, or destructive system tests on
  the host. Rerun only already-established, user-approved VM gates, retaining their exact non-claims.
  Do not autonomously reboot the VM.
- Unrelated improvements go into `FUTURE_PLAN.md` rather than expanding this plan.

### Source-LOC prerequisite

The P0 plan requires the pre-existing oversized `CHANGELOG.md` blocker to be resolved. Before the
first Lua source is added, extend the existing source-LOC classifier to include `*.lua` and prove
`make source-loc` rejects an oversized Lua fixture.

## 4. Lua-specific migration surface

The Gluon reference corpus currently contains:

| Surface to pair/convert | Files | LOC |
|---|---:|---:|
| Embedded versioned ABIs | 13 | 2,007 |
| Other shipped declarations | 5 | 2,390 |
| Documentation examples | 132 | 4,554 |
| Test fixtures | 68 | 4,251 |
| **Total `.glu` reference corpus** | **218** | **13,202** |

There are six generated Gluon paths requiring Lua codecs or deliberate migration behavior: draft
recipe, profile, repository, system snapshot, source lock, and build lock.

### Used language features

| Gluon feature | Current use | Lua declaration-profile mapping |
|---|---:|---|
| `let` | 189 files / about 807 occurrences | initialized `local` binding |
| `import!` | 180 / 286 | grammar-validated literal import supplied to the shared graph |
| record update | 117 / about 420 | pure `cast.extend(base, patch)` |
| type declaration | 41 / about 173 | shared Rust schema; optional authoring-only annotations |
| pattern match | 7 / 11 | closed tagged variants and explicit dispatch |
| recursion | 1 / 1 | allowed only if the selected profile and limit policy prove it |
| array/string primitives | 8 files each | small deterministic ABI helpers |

Translation is broad but not the main risk. The main Lua-specific risks are capability removal,
literal-import parsing, resource enforcement, mutable-table semantics, bounded output conversion,
runtime packaging, and conversion of installed/archived Gluon state.

## 5. Select exactly one Lua dialect/runtime

Luau is not Lua 5.4. It is derived from Lua 5.1 and has meaningful syntax/runtime differences.
Cast must select and document one dialect; it must not compile both Lua engines.

| Candidate | Advantages | Risks |
|---|---|---|
| `mlua 0.12` + vendored Lua 5.4 + serde | Literal Lua 5.4, C toolchain, simpler musl/static packaging, familiar language | Manual minimal sandbox, hook-based deadline, `__gc`/loaders/stdlib must be absent, no native readonly tables |
| `mlua 0.12` + Luau + serde | Interrupt support, memory limiter, sandbox primitives, no standard `io`/`package`, optional analyzer | Lua 5.1-derived dialect, C++17 build/link implications, upstream sandbox is still broader than Cast policy |

**Starting preference:** test standard Lua 5.4 first because it is the requested language and has
the simpler C/musl release story. Select it only if it passes every gate below. Use Luau if Lua 5.4
cannot satisfy the policy without fragile runtime patches. If neither passes, stop; do not weaken
the shared evaluator policy to force Lua in.

The engine spike must prove:

1. workspace MSRV and every supported build target through Make;
2. release-profile musl build, execution, and intended static/dynamic linkage;
3. a fresh VM exposes only the explicit allowlist;
4. memory exhaustion, infinite loops, deep recursion, and caught errors cannot bypass host limits;
5. the shared monotonic deadline can drive the engine hook/interrupt and remains host-latched;
6. a maintained grammar-aware parser can validate the Cast Lua profile and extract literal imports;
7. repeated evaluation in separate processes produces identical normalized output and v2 identity;
8. selected runtime sources and notices enter semantic fingerprints and packaged licenses.

The current Gluon 64-KiB stack setting has no assumed Lua equivalent. Characterize it in the P0
baseline, then require either an equivalent selected-engine bound or an explicitly approved,
versioned engine-neutral call-depth policy. Never accept an undocumented engine default.

Do not use system Lua. Host-selected runtime versions, modules, and search paths would break
reproducibility and provenance.

## 6. Lua adapter contract

### 6.1 `crates/lua_config`

Create `crates/lua_config` as an engine implementation, not a parallel config framework. It owns:

- selected-dialect parsing and declaration-profile validation;
- literal-import extraction into shared `ImportRequest` values;
- preparation of engine-native chunks from the core-prepared graph;
- fresh restricted VM construction;
- mapping shared memory/deadline/call-depth policy to the selected engine;
- host-latched limit failure and panic containment;
- Lua-native error translation into the shared diagnostic envelope;
- bounded conversion of the final Lua value into the shared domain decoder callback.

It must not own:

- filesystem/path discovery or hardened reads;
- independent source/import/graph limits;
- relative path resolution;
- fragment layering or persistence;
- evaluation identity encoding;
- domain DTO meaning;
- generated-file authority.

`LuaEngine` supplies opaque language/profile/engine descriptors to the shared core. The core must
not gain `if language == Lua` branches.

### 6.2 Cast's Lua declaration profile

Lua is accepted through a restricted declarative profile, not as unrestricted general-purpose
Lua. A dialect-aware AST pass runs before execution.

Authored roots and relative modules may use:

- initialized local bindings;
- scalar/table literals and field/index reads;
- pure expressions and allowlisted ABI helper calls;
- functions/conditionals required by the translated corpus;
- one final root return.

Reject unless a later policy version explicitly adds and tests them:

- global writes;
- reassignment or post-construction table mutation;
- duplicate literal keys;
- loops/goto and variable/dynamic imports;
- varargs and coroutine constructs;
- bytecode, `load`, `loadfile`, `dofile`, or external modules.

Embedded ABI modules may use a wider reviewed subset internally, but remain capability-restricted,
implementation-fingerprinted, and unable to leak mutable state across evaluations.

### 6.3 Final value-tree validation

Only the authored root's final value crosses into Rust. Imported modules may privately return
functions/constructors; executable values may not appear in the root result.

Before shared schema decoding, walk the Lua value and accept only:

- booleans;
- UTF-8 strings;
- signed integers with explicit target-bound checks;
- contiguous one-based arrays;
- string-keyed closed records;
- explicit tagged option/patch/variant records.

The tree walk rejects floats, NaN/infinity, functions, threads, userdata/lightuserdata, metatables,
bytecode, cycles, sparse arrays, mixed-key tables, and invalid UTF-8. The existing shared domain
schema then rejects missing/unknown fields, malformed tags, and target integer overflows. Bound
depth, total nodes, table entries, per-string bytes, and aggregate host allocation before schema
conversion. These limits are part of the shared evaluator-policy identity.

`nil` is never an option value. Use explicit values such as:

```text
{ kind = "none" }
{ kind = "some", value = ... }
{ kind = "keep" }
{ kind = "set", value = ... }
```

`cast.extend` and array helpers return new values. Shared defaults/module exports must be read-only
or independently copied. Modules execute dependencies-first, at most once per fresh VM; there is
no process-global module cache and no state survives evaluation.

### 6.4 Capability policy

Construct an empty-by-default environment. Do not expose filesystem, IO, environment, process,
network, clocks/time, randomness, OS, package loaders, debug, FFI, bytecode loading, coroutines,
mutable ambient globals, or nondeterministic `pairs`/`next` iteration.

Omit `pcall`/`xpcall` unless tests prove every hook/interrupt/memory/call-depth failure remains
latched in Rust and rejects the evaluation after any script catch. A fresh VM is required for every
evaluation.

### 6.5 Import policy

Lua parses imports; the shared core resolves them.

- Embedded imports use semantic ABI names such as `cast.package.v3`.
- Relative imports contain an exact `.lua` extension.
- Only grammar-recognized literal imports are accepted.
- Absolute paths, traversal, NUL, ambient `LUA_PATH`/`LUA_CPATH`, computed imports, and
  cross-language imports are rejected.
- The existing core applies graph ordering/deduplication, source authority, limits, and identity.
- The VM receives only the prepared in-memory module set; standard `package` loading is absent.

If the selected dialect lacks a maintained grammar-aware parser, stop. A regex/text scanner is not
an acceptable security boundary.

## 7. Register Lua without special cases

Lua registration must use the second-adapter seam already proven synthetically by the P0 plan.
Register a real production `LanguageSpec` through that seam and retain the synthetic conformance
tests; do not redesign dispatch.

Concrete rules:

- `.lua` selects `LuaEngine`; `.glu` selects `GluonEngine` while Gluon is supported.
- `stone.lua` plus `stone.glu` in one recipe directory is a hard collision before evaluation.
- `system.lua`/`system.glu`, `boot-topology.lua`/`.glu`, and
  `root-filesystem.lua`/`.glu` collide in the same logical slot.
- Same-layer `foo.lua` plus `foo.glu` is a hard duplicate. Across vendor/admin/user layers, the
  existing higher-layer whole-fragment override remains, but every discovered fragment validates.
- A failed source is never retried with the other engine.
- Cross-language imports are never allowed.

Generated artifacts have one active authority:

- `stone.lua` writes `sources.lock.lua` and `build.lock.lua` only;
- `stone.glu` retains the `.glu` pair while supported;
- a system snapshot uses the accepted system intent's language;
- new CLI-created declarations may default to Lua only after public activation;
- existing generated files preserve their registered codec until explicit migration;
- emitters sort keys, escape deterministically, use explicit tags, and never rewrite authored files.

Every save/delete/update checks every registered extension through the same retained directory
authority. An alternate-language authored file blocks mutation. Alternate generated state requires
the explicit transactional migration path; a normal command never creates dual authority or
silently orphans old locks/snapshots.

Do not bump `cast.package.v3`, `cast.trigger.v1`, or other semantic ABI names merely because Lua
implements them. ABI versions change only with domain meaning; engine/language belongs in v2.

## 8. Lua-specific path impact

| Area | Expected path | Lua work only |
|---|---|---|
| Runtime | new `crates/lua_config/` | parser/profile, imports, VM, capabilities, output validation, diagnostics adapter |
| Workspace | `Cargo.toml`, `Cargo.lock`, build shell/CI/release files | selected `mlua` engine/features/toolchain only |
| Test integration | new `misc/make/lua-tests.mk` | exact Lua spike, evaluator, parity, release, dependency, installed-state targets |
| Semantic fingerprint | `crates/tools_buildinfo/src/semantic_fingerprint.rs` | register the selected Lua ABI source root and runtime notices |
| Triggers | `crates/triggers/` Lua adapter/ABI | first public read-only domain |
| Profiles | `crates/mason/` profile Lua adapter/ABI/emitter | paired fragment evaluation and canonical encoding |
| Repositories | `crates/forge/src/repository/` Lua adapter/ABI/emitter | paired fragment evaluation and canonical encoding |
| Package/policy | `crates/stone_recipe/` Lua adapters and ABI tree | package, builders, policy total/patch/layers |
| Recipes/locks | `crates/mason/` and Stone Recipe lock modules | `stone.lua`, drafts, source/build lock Lua codecs |
| System | `crates/forge/src/system_model/` | Lua intent and canonical Lua snapshot |
| Boot intent | Forge boot-topology/root-filesystem adapters | Lua model parity; no new transaction architecture |
| Corpus/docs | 218-reference Gluon corpus and current configuration docs | paired Lua sources, translation review, user contract |

The P0 plan must already have split `crates/mason/src/source_lock.rs` by function. Lua must add a
new codec module rather than growing a near-limit file again.

## 9. Implementation phases and commits

### Phase L0 — Verify the prerequisite and select the engine

1. Verify every completion criterion and accepted commit from `agnostic_config.md`.
2. Prove the shared core and generic `config` still have no Gluon dependency and no Lua special
   case.
3. Add `*.lua` source-LOC coverage before adding any Lua source.
4. Add `misc/make/lua-tests.mk` with exact targets `lua-engine-spike`,
   `lua-engine-spike-release`, `lua-config-test`, `lua-domain-parity-test`, `lua-release-test`,
   `lua-dependency-audit`, and `lua-installed-state-test`.
5. Run the Lua 5.4/Luau spike from section 5 and select one exact engine/feature set.
6. Register the now-selected Lua runtime/ABI source roots in semantic implementation fingerprints
   before adding production Lua sources.
7. Select Endpoint A or B before public registration and reconcile `PLAN.md`, `FUTURE_PLAN.md`,
   README, and current configuration documentation.

**Commit examples:** `test(lua): add adapter guardrails`, then
`test(lua): prove selected runtime constraints`, then `docs(plan): select lua endpoint`.

**Exit:** the dialect, parser, features, packaging, endpoint, and rejected alternative are recorded;
no production domain accepts `.lua`.

### Phase L1 — Implement isolated `LuaEngine`

1. Create `crates/lua_config` using the selected features only.
2. Implement AST profile validation, literal-import extraction, shared graph integration, fresh VM,
   capability allowlist, limit/deadline mapping, host latches, and diagnostic translation.
3. Implement bounded final value-tree validation and typed decoder callback integration.
4. Test every prohibited construct/capability, source/import limit, deadline bypass, OOM, recursion,
   panic, cycle, malformed value, fresh-VM isolation, and repeated-process determinism case.
5. Run both debug and actual release execution through Make.

**Commit examples:** `feat(lua): implement declaration engine adapter`, then
`test(lua): pin capability and value boundaries`.

**Exit:** `LuaEngine` passes its complete isolated contract; it is not registered for a production
domain.

### Phase L2 — Prove one complex domain privately

Use package v3 or build-policy v5 because it exercises records, variants, options/patches, builders,
and semantic validation.

1. Implement the corresponding Lua ABI under the selected adapter source tree.
2. Implement the domain's Lua `DeclarationEvaluator<T>` against the already-shared wire type.
3. Differentially evaluate paired Gluon/Lua fixtures into normalized Rust values.
4. Test missing/unknown fields, tags, integer boundaries, immutable update helpers, module export
   isolation, and stable field-path diagnostics.
5. Keep the adapter private; do not add `.lua` discovery for users yet.

**Commit:** `test(lua): prove complex domain parity`.

**Exit:** the hardest schema passes without changing any shared P0 interface.

### Phase L3 — Register the trigger pilot

Triggers are the smallest read-only ABI and need no encoder.

1. Add the Lua implementation of `cast.trigger.v1`.
2. Register `.lua` through the generic trigger evaluator registry.
3. Preserve exact embedded-ABI and empty-explicit-input evidence contracts in v2.
4. Add paired success/failure/identity/determinism fixtures and collision/fallback negatives.

**Commit:** `feat(triggers): register lua declaration adapter`.

**Exit:** Lua triggers normalize identically to Gluon; engine identities intentionally differ; no
transaction runs during host tests.

### Phase L4 — Add profile and repository fragments

1. Add Lua ABIs and typed evaluators/codecs for profile and repository domains.
2. Add canonical Lua emitters using the shared generated-authority/persistence path.
3. Register Lua for these domains without modifying generic layering.
4. Test same-layer cross-extension collisions, cross-layer logical shadowing, validation of shadowed
   fragments, atomic save/delete, authored protection, alternate-authority blocks, and interrupted
   generated-language migration.

**Commit separately:** `feat(profile): add lua declaration codec` and
`feat(repository): add lua declaration codec`.

**Exit:** fragment storage behavior is adapter-independent and no dual write exists.

### Phase L5 — Add packages, policies, recipes, and locks

1. Add Lua implementations for package v3, four builder v2 ABIs, build-policy v5, and
   policy-layers v1.
2. Add Lua adapters for package, policy total/patch/layers, source lock, and build lock.
3. Register exact `stone.lua` discovery and `stone.lua`/`stone.glu` collision behavior.
4. Add language-matched source/build lock codecs, draft generation, check/freeze/explain behavior,
   and v2 provenance labels.
5. Test recipe-language switching, stale alternate lock pairs, interrupted migration, authored-file
   refusal, repeated builds, and normalized plan parity.

**Commit by function:** ABI/parity, recipe registration, source-lock codec, build-lock codec, then
CLI behavior.

**Exit:** paired recipes yield equal normalized package plans and expected package filenames while
retaining intentionally different evaluation/derivation identities. `.stone` format is unchanged.

### Phase L6 — Add system and boot intent

1. Add Lua implementations for system v1, boot-topology v2, and root-filesystem v1.
2. Register Lua through the generic fixed-logical-slot path and preserve collision errors.
3. Emit/re-evaluate a canonical Lua system snapshot with v2 language provenance.
4. Prove host-safe model and plan parity first.
5. In the disposable UEFI VM, rerun only already-established relevant gates showing equivalent Lua
   values reach the same Rust plans/publication paths. Retain every existing non-claim for startup
   repair, interruption, selected-payload bootability, reboot recovery, and power-loss durability.
6. Never reboot autonomously. Any new reboot experiment remains in `FUTURE_PLAN.md` and requires
   fresh approval, confirmed snapshot/guest/target-disk identity, and loss-of-access planning.

**Commit separately:** system intent, boot topology, root filesystem, then VM evidence.

**Exit:** Lua reaches the same accepted system-model boundaries as Gluon without changing the
transaction engine or claiming deferred closure.

### Phase L7 — Pair the complete corpus and documentation

1. Pair all 13 embedded ABIs, five shipped declarations, 68 fixtures, and 132 documentation
   examples.
2. Use mechanical translation only for simple constructs; hand-review ABI modules and every file
   using matches, recursion, variants, options/patches, or primitive helpers.
3. Differentially compare all paired roots after normalization into shared Rust values.
4. Verify v2 fingerprints differ by language/engine as designed.
5. Update scripts, CLI help, README, plans, configuration guide, and examples.
6. Keep YAML/KDL rejection explicit while allowing only the selected registered language set.
7. Audit source-LOC, semantic source-tree inclusion, runtime licenses, and release packaging.

**Commit in small domain/corpus slices**, never one mass translation commit.

**Exit:** every public domain/example has a passing Lua form and the documentation describes the
actual selected dialect, sandbox, import rules, file authority, and endpoint.

### Phase L8 — Ship the installed-state bridge and release parity

Endpoint A requires at least one dual-runtime bridge release. Endpoint B still needs safe explicit
language switching.

Inventory and handle:

- active authored files such as `/etc/cast/system.glu`;
- active generated files such as `/usr/lib/system-model.glu` and lock pairs;
- every archived system snapshot that current state-query/export paths can read;
- legacy generated profile/repository fragments;
- migration state and interrupted temporary files.

#### Durable migration authority

Use one concrete catalog rather than a loose marker or directory scan:

- add a `declaration_migrations` table to the existing
  `<installation-root>/.cast/db/state` SQLite database;
- use that catalog only for immutable state-owned material, keyed by `(state_id, logical_slot)` with
  `state_id` referencing `state(id) ON DELETE CASCADE`;
- store a catalog schema version, authenticated state-tree marker, original language/logical path
  and SHA-256, migrated language/blob SHA-256, and canonical evaluation-identity-v2 bytes;
- store immutable converted bytes at
  `<installation-root>/.cast/declaration-migrations/v1/blobs/<sha256>.lua`, created beneath a
  retained private `.cast` directory authority; the hash in the filename and catalog must match
  the reopened file;
- treat only a committed catalog row as selection authority. A file with no committed row is
  unreachable residue, never an implicit candidate; a committed row whose state marker, original
  source, converted blob, or v2 identity no longer matches fails closed rather than falling back.

The bridge writes a blob with no-replace semantics, synchronizes and reopens the file, synchronizes
its retained parent directories, and only then commits the catalog row in one exclusive SQLite
transaction. A retry accepts an existing blob/row only after exact byte, hash, state, slot, source,
and identity equality. The database commit is the single authority switch; directory naming,
mtime, newest-file order, and temporary files never select a declaration.

The state catalog is not authority for every mutable declaration store. Handle the remaining
scopes through their existing P0 authorities:

- profile/repository roots and other registered mutable config stores use
  `GeneratedDeclarationSlot`'s transactional language-authority switch, then prove by rooted
  enumeration that no generated `.glu` authority remains;
- a recipe tree is migrated only when the operator supplies its exact root and a Lua replacement
  for its authored `stone.glu`; after semantic comparison, regenerate/migrate the source/build lock
  pair through that recipe directory's retained generated-slot authority;
- user-authored system/profile/repository/recipe sources are never copied automatically and never
  receive a fabricated catalog row. They require an operator-provided Lua replacement;
- arbitrary recipe directories outside the roots explicitly presented to the bridge cannot be
  globally enumerated. Lua-only tooling must reject any later-discovered `.glu` recipe or lock with
  the precise bridge-release migration command instead of silently treating it as converted.

Bridge-era readers resolve every live, archive-export, and rollback request by state ID and logical
slot. If no committed row exists, they may invoke the authenticated legacy Gluon reader. If a row
exists, they must revalidate the state wrapper/tree marker and both source hashes before selecting
the Lua blob. Old bridge-release binaries remain safe because original immutable `.glu` material is
left in place and they ignore the new catalog; a later Lua-only binary requires complete catalog
coverage and never needs to reinterpret legacy bytes.

Migration completion is one aggregate report containing: database coverage of every required
state-owned logical slot for every retained state; rooted enumeration showing every registered
mutable generated store has Lua authority; and explicit records for every authored/config/recipe
root the operator asked the bridge to migrate. It is not a standalone marker file and does not
claim to discover arbitrary recipe trees elsewhere on disk. Pruning a state transactionally
cascades its catalog rows; content-addressed blobs are removed only by a later retained-authority
garbage collection pass after proving that no committed row references them. A crash may therefore
leave an unreachable blob, but may never leave a row pointing at an undurable or unverified blob.

Bridge sequence:

1. read/verify legacy state through the Gluon adapter and v1/container-aware compatibility path;
2. never overwrite authored Gluon automatically—require an operator-provided Lua replacement and
   prove normalized equality where requested;
3. create content-addressed v2/Lua blobs and commit their state/slot catalog mappings without
   mutating immutable historical generations;
4. migrate each registered mutable generated store and each explicitly supplied recipe lock pair
   through its own retained P0 authority, never through the state catalog;
5. test crashes before blob sync, after blob sync but before database commit, during commit, after
   commit, during state pruning, and during deferred blob garbage collection;
6. test interruption before and after every mutable-store/recipe authority switch, including
   alternate authored files and stale generated lock pairs;
7. test same-byte path replacement, state-tree/source drift, catalog/blob mismatch, and repeated
   migration; every ambiguous or mismatched case fails closed;
8. prove live load, archived export, rollback selection, resume, forward selection, and aggregate
   state/store/operator-scope coverage reporting;
9. make a future Lua-only release refuse upgrade with a precise bridge instruction if any required
   state-owned or registered-store declaration remains unmigrated.

Run `make lua-config-test`, `make lua-domain-parity-test`, `make lua-release-test`,
`make lua-dependency-audit`, `make lua-installed-state-test`, and the aggregate project gates. The
release target must execute release-built tests; `make build` alone is not execution.

**Commit separately:** bridge implementation, interruption tests, old-installation fixture, then
release evidence.

**Exit:** production Lua and bridge behavior pass host-safe and bounded established VM gates; no
historical state is stranded and no deferred reboot/power-loss claim is made.

### Phase L9A — Finish Lua-only

1. Make Lua the sole registered production language.
2. Remove Gluon per-domain adapters, `gluon_config`, Gluon/codegen dependencies, Gluon ABIs,
   shipped `.glu` data, fixtures, examples, and Gluon-only documentation/Make targets.
3. Remove production `Getable`/`VmType` derives and Gluon-specific public names.
4. Remove the verified legacy reader only after complete catalog coverage and old-installation
   upgrade/export/rollback acceptance prove it is safe.
5. Regenerate the lockfile and report net removed and added dependencies/licenses; do not reuse an
   old gross dependency claim.
6. Rerun L8 after deletion.

Commit removal by domain, dependency, corpus, and documentation. The final cleanup commit contains
only dead Gluon removal.

### Phase L9B — Finish permanent dual support

1. Remove migration-only shims but retain both explicit adapters.
2. Keep both ABI/corpus/documentation/runtime/release/security matrices mandatory.
3. Preserve concrete collision, no-fallback, no-cross-import, and one-authority rules.
4. Document that Gluon dependencies remain and both engines are security-critical.

Do one of L9A or L9B, never a partial mixture.

## 10. Make-driven validation

All targets live in `misc/make/lua-tests.mk` and are invoked from the root Makefile:

- `make lua-engine-spike`;
- `make lua-engine-spike-release`;
- `make lua-config-test`;
- `make lua-domain-parity-test`;
- `make lua-release-test`;
- `make lua-dependency-audit`;
- `make lua-installed-state-test`;
- inherited P0 targets `declarative-config-test`, `gluon-adapter-test`, and
  `declaration-regression-test` while Gluon remains;
- `make build`, `make test`, `make check`, `make examples`, and `make source-loc`.

Validation matrix:

| Gate | Location | Evidence |
|---|---|---|
| Lua parser/runtime | Host | profile syntax, capabilities, imports, resource latches, output bounds |
| Shared-interface conformance | Host | no duplicated source/graph/identity/persistence and no Lua special case in core |
| Domain differential | Host | equal normalized values for all 12 shapes; intentionally distinct v2 identities |
| Storage/authority | Host temp dirs | extension collisions, layering, atomic persistence, interrupted migration |
| Determinism | Host isolated processes | no environment/path/time/random/order influence |
| Packaging | Host/CI-safe | release execution, musl/linkage, semantic fingerprints, notices |
| Installed state | Host fixtures; established VM if needed | live/archive export, catalog coverage, rollback selection, resume |
| System/boot parity | Disposable VM only | only accepted baseline boundaries with exact non-claims |

Bound evaluator/application executions, not compilation. Do not put an entire compile-and-run Make
invocation under an execution timeout.

## 11. Completion criteria

### Lua production readiness

- The exact selected Lua dialect/runtime is documented and reproducibly packaged.
- `LuaEngine` uses the P0 source, deadline, graph, diagnostics, identity, storage, and domain
  contracts without a fork or special case.
- The Lua AST profile, capability allowlist, imports, host-latched limits, and output tree are
  adversarially tested in debug and release.
- All 12 domain shapes reach the same shared Rust semantic validators.
- Equivalent Gluon/Lua declarations normalize to equal domain values and intentionally different
  v2 evaluation/derivation identities.
- Every generated logical slot has one active codec/authority; no fallback or dual write exists.
- The complete corpus, documentation, packaging, installed-state bridge, and bounded VM parity
  gates pass.
- `.stone` and transaction/system architecture remain unchanged.
- Every touched file is below 1,000 lines; YAML/KDL remain rejected; forbidden config files remain
  absent.

### Additional Endpoint A criteria

- `git ls-files '*.glu'` is empty.
- No production `gluon`, `gluon_codegen`, or `gluon_config` dependency remains.
- No production Gluon DTO derive, adapter, public API, generated path, CLI promise, or Make gate
  remains.
- The aggregate state-catalog, registered-store, and operator-supplied-root report proves every
  required in-scope declaration is migrated; an old installation passes upgrade, export, rollback
  selection, interruption resume, pruning, and forward selection; later-discovered out-of-scope
  Gluon recipe trees fail with an exact bridge instruction.

### Additional Endpoint B criteria

- Every supported domain registers both adapters through the shared registry.
- Same-slot collisions, fallback, cross-language imports, and dual writes are test-pinned errors.
- Both complete evaluator/corpus/release/security matrices remain mandatory.

## 12. Effort estimate

| Lua-specific workstream | Estimate |
|---|---:|
| Runtime selection and isolated `LuaEngine` | 3–4 engineer-weeks |
| Twelve Lua domain adapters and ABIs | 4–7 engineer-weeks |
| Generated codecs, corpus, docs, and tooling | 3–5 engineer-weeks |
| Installed-state bridge, release, and VM parity | 3–5 engineer-weeks |
| **Lua production parity through L8** | **13–21 engineer-weeks** |
| Optional final Gluon removal/proof | 3–5 engineer-weeks |

Together with the P0 foundation estimate of 10–17 engineer-weeks, permanent dual production is
approximately 23–38 engineer-weeks. The recommended Lua-only endpoint is approximately 26–43
engineer-weeks. Refine both after the engine spike and adapter foundation land.

Permanent dual support avoids deletion work but retains doubled ongoing runtime, ABI,
documentation, dependency, and security-test cost.

## 13. Stop conditions

Stop and report evidence if:

- any `agnostic_config.md` completion criterion is missing or stale;
- Lua requires a second `SourceRoot`, loader, deadline, graph resolver, fingerprint, config store,
  persistence path, or domain semantic type;
- the shared core would need a built-in Lua branch rather than an opaque adapter descriptor;
- neither engine satisfies capability, memory, deadline, call-depth, determinism, musl, or release
  constraints;
- literal imports cannot be extracted with a maintained dialect-aware parser;
- final Lua values cannot be bounded and cycle-safe before schema decoding;
- module exports cannot be isolated from mutation across importers/evaluations;
- `.lua` registration bypasses generic dispatch/collision/authority rules;
- a Lua value can enter provenance without complete identity v2;
- installed/archived state would be stranded, rewritten in place, or silently ignored;
- a file reaches 1,000 lines without a functional split;
- validation would touch host disk/ESP/BOOT/activation/rollback/reboot;
- completion requires `../bedrock`, version metadata, YAML/KDL, Nix translation, or
  formatting/typo-policy configuration;
- an unrelated improvement appears—record it in `FUTURE_PLAN.md`.

## 14. First implementation batch

1. Verify the accepted P0 commit and rerun its complete Make gates.
2. Add `*.lua` LOC coverage and `misc/make/lua-tests.mk` without production registration.
3. Commit those initial guardrails.
4. Run the engine spike through `lua-engine-spike` and `lua-engine-spike-release`.
5. Select the engine, register its real source roots in semantic fingerprints, and commit the spike
   code/evidence separately.
6. Record the engine and endpoint decision, plus rejected alternatives, in a documentation commit.
7. Stop for review before creating production `crates/lua_config`.

No production `.lua` discovery or domain adapter belongs in this batch.

## 15. Primary external references

- [`mlua` documentation](https://docs.rs/mlua/latest/mlua/) — supported runtimes and serde
  conversion.
- [`mlua::Lua` API](https://docs.rs/mlua/latest/mlua/struct.Lua.html) — selected libraries, hooks,
  interrupts, memory limits, sandboxing, and custom Luau require support.
- [Lua 5.4 reference manual](https://www.lua.org/manual/5.4/) — the standard language and libraries
  that Cast's profile must restrict.
- [Luau sandbox documentation](https://luau.org/sandbox/) — useful baseline, still broader than
  Cast's empty-by-default policy.
- [Luau compatibility documentation](https://luau.org/compatibility/) — why Luau must not be
  described as Lua 5.4.
