# Plan: Add Lua as a First-Class Declarative Frontend

**Status:** Active implementation handoff
**Priority:** P1
**Effort:** XL
**Risk:** High
**Planned against:** `develop` at `700202dc`
**Audit date:** 2026-07-23

This plan replaces the archived Lua feasibility report. It describes the work required to add
Lua without weakening Cast's declarative, reproducible, atomic system model. It also separates
the two possible end states:

1. **Recommended:** use Gluon only as a temporary differential-testing bridge, then remove it and
   make Lua the sole authored declaration language.
2. **Optional:** keep Lua and Gluon as permanent first-class authored frontends behind one
   language-neutral evaluation and domain layer.

The shared foundation is the same through the parity gate. The final Gluon-removal phase is not
started until the endpoint is recorded explicitly in `PLAN.md` and `FUTURE_PLAN.md`.

## 1. Goal and non-negotiable invariants

### Goal

Allow users to describe packages, policies, profiles, repositories, triggers, system intent, and
boot intent in a small deterministic Lua declaration language. Lua must produce the same owned
Rust domain values that Gluon produces today; the runtime must disappear before any transaction,
artifact, or activation code runs.

This is a declaration-frontend migration, not a package-manager rewrite.

### Invariants that must survive unchanged

- `.stone` remains the built package artifact. No new package artifact format is introduced.
- Package construction, container-based transaction triggers, atomic activation, atomic rollback,
  USR merge compliance, and the separation between OS state and local configuration remain owned
  by the existing Rust transaction/system layers.
- Evaluation remains one-shot: source enters a fresh restricted VM, becomes an owned Rust value,
  and the VM is dropped. No script callbacks or live VM objects enter the transaction engine.
- Source loading remains descriptor-rooted, bounded, symlink-safe, TOCTOU-resistant, and detached
  from ambient interpreter search paths.
- Invalid declarations fail closed before mutation.
- Evaluation provenance remains part of derivation identity. Different engines may never alias
  merely because their source bytes or resulting Rust values happen to match.
- YAML and KDL remain removed. Lua support must not restore either format or add fallback parsing.
- There is no automatic Nix-to-Lua translator and no Nix compatibility target. Similarity is a
  language-design preference, not a compatibility promise.

### Executor rules

- Work only in this repository. Do not read from or change `../bedrock` as part of execution.
- Work on `develop`; leave `main` untouched and do not leave migration branches behind.
- Use the root Makefile for every relevant build, test, audit, and validation action. Add a focused
  Make target before introducing a new validation lane.
- Commit after every cohesive slice. Do not accumulate several phases in one commit, and do not
  push unless the user asks.
- Do not change release/version metadata unless a later explicit request requires it.
- Do not add or restore `.clippy.toml`, `.rustfmt.toml`, or `.typos.toml`. Formatting and typo-policy
  configuration is outside this plan.
- Do not use Python to edit files. Use patch/edit tools.
- No source file may exceed 1,000 lines. Split near-limit files by behavior and name the new files
  for that behavior, never `file_01`, `part_02`, or similar.
- Do not wrap Git, Make, or compilation commands in `timeout`. Use a timeout only when actually
  executing the evaluator/application or a remote VM operation that may hang.
- Never perform ESP, BOOT, filesystem, block-device, activation, rollback, reboot, or destructive
  system tests on the host. Those tests run only inside a user-provided disposable VM.
- Any useful work discovered outside this plan goes into `FUTURE_PLAN.md`; it does not expand the
  current implementation.

**Known baseline gate:** `make source-loc` currently fails on the pre-existing 2,374-line
`CHANGELOG.md`. Do not report the repository-wide LOC gate as green. Resolve that previously
requested repository cleanup before Phase 0 implementation, without folding it into a Lua commit.
The Lua plan itself must add `*.lua` coverage before creating its first Lua source file.

## 2. Current state and real migration size

The former report was stale. The current tracked declaration surface is:

| Surface | Files | LOC |
|---|---:|---:|
| Versioned embedded Gluon ABI modules | 13 | 2,007 |
| Other crate-owned/shipped Gluon declarations | 5 | 2,390 |
| Documentation examples | 132 | 4,554 |
| Test fixtures | 68 | 4,251 |
| **All tracked `.glu` files** | **218** | **13,202** |
| Tracked `.lua` files | 0 | 0 |

The 13 public ABI modules are package v3; four builder v2 modules; build-policy v5; policy-layers
v1; profile v1; repository v1; system v1; boot-topology v2; root-filesystem v1; and trigger v1.
The broader 18-file/4,397-LOC crate-owned count includes five shipped Mason declarations and must
not be confused with the embedded ABI count.

### Rust coupling

- `crates/gluon_config` is about 3,031 Rust LOC, including 2,373 production LOC.
- The primary production Gluon boundary is about 11,752 LOC across 26 files before documentation,
  fixtures, and broad lexical references.
- There are 129 `Getable`/`VmType` derive sites: 123 production and 6 test-only.
- Five crates depend directly on Gluon/codegen: `gluon_config`, `stone_recipe`, `mason`, `forge`,
  and `triggers`; `config` depends through `gluon_config`.
- Six output paths emit canonical Gluon: draft recipes, profiles, repositories, system snapshots,
  source locks, and build locks.
- At least 227 focused tests exercise evaluator and downstream boundary behavior. A broader selected
  inventory reaches 259 tests. Phase 0 must generate one checked-in category ledger instead of
  relying on either approximate count.

The 12 decoded root shapes are package, build-policy total, build-policy patch, policy-layer
manifest, source lock, build lock, profile fragment, repository fragment, system intent/snapshot,
boot topology, root-filesystem intent, and trigger. Each is an independent parity obligation.

### Gluon features actually used

| Feature | Current use | Lua implication |
|---|---:|---|
| `let` | 189 files / about 807 occurrences | `local` bindings |
| `import!` | 180 / 286 | restricted, prepared imports |
| record update | 117 / about 420 | immutable `cast.extend` helper |
| type declarations | 41 / about 173 | authoritative Rust schema; optional authoring types |
| pattern matching | 7 / 11 | closed tagged variants |
| recursion | 1 / 1 | allowed only within resource policy |
| array/string primitives | 8 files each | tiny deterministic host helpers |

The corpus is mechanically approachable, but translation is not the hard part. The hard parts are
source provenance, import discovery, sandbox capability removal, DTO semantics, generated state,
and proving that system transactions remain unchanged.

### Current evaluator contracts to preserve

`crates/gluon_config` currently enforces these defaults:

- root source: 1 MiB;
- explicit input: 1 MiB;
- each imported file: 256 KiB;
- imports: 64;
- total import graph: 2 MiB;
- VM memory: 32 MiB;
- VM stack: 64 KiB;
- one two-second wall-clock deadline spanning load, imports, fingerprinting, and execution.

`SourceRoot` and most of `source.rs` are language-neutral in behavior, but they are physically
owned by `gluon_config`. Imports, VM interruption, diagnostics, `.glu` normalization, and the
fingerprint schema are language-specific. Therefore the existing crate cannot simply be copied.

The 64-KiB Gluon stack setting has no assumed one-to-one Lua API. Phase 0 must characterize its
observable recursion boundary and diagnostic first. The selected engine must either enforce an
equivalent bound or propose a versioned engine-neutral call-depth policy for explicit approval.
Silently substituting an engine default is not acceptable.

## 3. Required decisions before implementation

### 3.1 Engine/dialect gate: Lua 5.4 versus Luau

Luau is not “Lua 5.4 with types.” It is derived from Lua 5.1 and deliberately differs from PUC
Lua. The project must select and name one dialect; it must not compile both Lua engines.

| Candidate | Advantages | Costs/risks |
|---|---|---|
| `mlua 0.12` + vendored Lua 5.4 + serde | Literal Lua 5.4, C toolchain, simpler musl/static release story, familiar language | Must construct a minimal environment manually; hook-based deadline; `__gc`, loaders, IO, OS, package, debug, and nondeterministic facilities must be absent |
| `mlua 0.12` + Luau + serde | Memory limiter, interrupt support, sandbox primitives, no standard `io`/`package`, optional authoring-time analyzer | Lua 5.1-derived dialect, C++17 build/runtime implications, sandbox is still broader than Cast's current empty VM and must be reduced |

**Starting preference:** test standard Lua 5.4 first because that is the requested language and it
has the simpler C/musl packaging story. Select it only if every security, deadline, memory, stack,
import, determinism, and release gate passes. Select Luau instead if Lua 5.4 cannot meet those
contracts without fragile patches. If neither passes, stop the migration; do not silently weaken
the evaluator policy.

The Phase 0 spike must prove, for both candidates where feasible:

1. workspace MSRV and all supported release targets build through Make;
2. the release-profile musl binary runs and has the intended static/dynamic linkage;
3. a fresh VM has only the explicit allowlist;
4. memory exhaustion, infinite loops, deep recursion, and caught errors cannot bypass limits;
5. one total deadline covers source loading through output validation;
6. a grammar-aware literal-import pass can prepare the complete graph before execution;
7. repeated evaluation in separate processes produces identical normalized output and fingerprints;
8. dependency licenses/notices are included by the existing packaging lane.

Record the winner, exact crate features, runtime version source, toolchain implications, and rejected
alternative in this file before Phase 1. Do not use system Lua: host-selected runtimes would break
reproducibility and provenance.

### 3.2 Endpoint gate: replacement versus permanent coexistence

The architecture must support a temporary dual-runtime window for differential testing. That does
not automatically make both languages a permanent public feature.

#### Endpoint A — Lua-only (recommended default)

- Add the language-neutral foundation and Lua while Gluon remains operational.
- Translate and differential-test every domain.
- Make Lua authoritative.
- Remove all production Gluon code, dependencies, public names, declarations, and gates in one
  final sequence of small commits.

This yields one language, one security matrix, and the dependency/build-maintenance benefit.

#### Endpoint B — permanent Lua plus Gluon

- Retain both evaluator adapters and both complete test matrices.
- Dispatch only by explicit file extension; never sniff source and never fall back after an error.
- Retain all Gluon dependencies while adding Lua's runtime/toolchain.
- Accept a permanently doubled ABI/documentation/security maintenance surface.

This avoids forced declaration conversion but does not deliver the Gluon dependency reduction.

Phase 0 must amend the Gluon-only promises in `PLAN.md`, `FUTURE_PLAN.md`, `README.md`, and
`docs/gluon-configuration.md` only after the endpoint is chosen. Until then, this file is the
implementation proposal and those files still describe current shipped behavior.

## 4. Target architecture

### 4.1 Crate boundaries

Create one shared core rather than cloning the security-critical loader:

```text
crates/declarative_config
    Source / SourceRoot / hardened reads
    limits / total deadline
    generic diagnostics
    language-tagged fingerprint v2
    prepared-module graph and evaluation policy traits
             |                         |
             v                         v
crates/gluon_config              crates/lua_config
    Gluon AST/import adapter         Lua AST/import adapter
    Gluon VM adapter                 selected Lua VM adapter
             \                         /
              v                       v
                    crates/config
          extension dispatch, fragment layering,
              atomic persistence, collisions
                         |
                         v
        shared wire DTOs -> existing semantic TryFrom
                         |
                         v
        stone_recipe / mason / forge / triggers
```

Responsibilities:

- `declarative_config` owns behavior that must be identical for every engine. Moving code must be a
  behavior-preserving extraction with characterization tests before Lua is introduced.
- `gluon_config` becomes an adapter and remains green during the migration.
- `lua_config` owns only selected-dialect parsing, prepared imports, VM construction/interruption,
  and conversion into the validated language-neutral value tree.
- `config` becomes an engine-neutral store/fragment manager. It must not duplicate evaluation or
  source-root logic.
- Domain crates own shared wire DTOs and existing semantic validation. Engine adapters must not
  independently redefine package, policy, repository, or system meaning.

### 4.2 Fingerprint v2 and persisted identity

The current fingerprint contains `gluon_version` and uses the hash domain
`os-tools-gluon-evaluation\0`. Lua values must never enter that identity.

Before the first Lua domain is accepted, introduce fingerprint schema v2 with:

- declaration language and language-profile version;
- engine kind and exact engine version/source;
- configuration ABI version and evaluator-policy version;
- root logical name and source hash;
- sorted prepared import identities and hashes;
- explicit-input identities and hashes;
- resource-policy identity;
- a new language-neutral hash domain.

V2 is an intentional identity boundary. Existing v1 build locks, derivation identities, snapshots,
and metadata may be readable for diagnostics, but they must be rejected as current inputs and
regenerated. A Lua and Gluon declaration that normalize to the same domain value must have
different evaluation fingerprints and derivation identities. Document this as expected cache/lock
invalidation, not an accidental regression.

Do not rewrite or delete historical immutable generations. V1 declarations/locks are stale for
creating a new derivation, but already materialized generations must remain addressable for
rollback without re-evaluating their old declaration source. The VM matrix must cross this migration
boundary in both directions.

Update every consumer, including `crates/stone_recipe/src/derivation/provenance.rs`, Mason recipe
state/explanations, package metadata emission, Forge boot intent contracts, and trigger contracts.

### 4.3 Lua declaration contract

Lua is accepted through a Cast declaration profile, not as unrestricted general-purpose Lua.
A dialect-aware source pass enforces the profile before the VM runs. Authored roots and relative
modules may use initialized local bindings, literals, pure expressions, calls to allowlisted ABI
helpers, functions required by the translated corpus, conditionals, and one final return. Reject
global writes, reassignment, post-construction table mutation, duplicate literal keys, loops/goto,
dynamic loading/imports, varargs, and coroutine constructs unless a later policy version adds a
specific construct with tests. Embedded ABI modules may use a wider reviewed subset internally,
but run in the same capability sandbox and are included in semantic fingerprints.

The declaration result obeys these rules:

- An authored root returns exactly one data value. Imported/embedded modules may privately return
  helpers or constructors, but executable values may not cross the root-output boundary.
- Values are limited to booleans, UTF-8 strings, signed integers, arrays, closed records, and
  explicitly tagged variants/options.
- Floating-point values, NaN/infinity, functions, threads/coroutines, userdata/lightuserdata,
  metatables, bytecode, cycles, sparse arrays, and mixed-key tables are rejected before
  serde/domain conversion.
- Arrays are contiguous and one-based. Records use string keys only. Unknown fields are errors.
- Conversion is bounded by depth, node count, table entries, per-string bytes, and aggregate host
  allocation; those limits are part of evaluator policy and fingerprint identity.
- `nil` is not an option value. ABI helpers return explicit tagged records such as
  `{ kind = "none" }`, `{ kind = "some", value = ... }`, `{ kind = "keep" }`, and
  `{ kind = "set", value = ... }`.
- Record update uses a pure `cast.extend(base, patch)` helper. Array helpers return new arrays.
  Shared defaults and ABI values are deeply frozen or otherwise made unmodifiable.
- Existing Rust semantic `TryFrom` validation remains authoritative after structural conversion.
- Lua 5.4 has no static type guarantee. If Luau is selected, analyzer output is an additional
  authoring aid only; it is never the runtime security or correctness boundary.

Prepared modules execute dependencies-first in one deterministic order, at most once per fresh VM.
There is no process-global module cache. Module exports are read-only within that evaluation: use
an engine-native freeze only if it is complete, otherwise expose host-owned wrappers or independent
copies. A module must not leak mutable state between importers, and no state survives evaluation.

### 4.4 Capability and import contract

Each evaluation uses a new VM with an explicit empty-by-default environment. Do not expose IO,
filesystem, environment, process, network, clocks/time, randomness, OS, package loaders, debug,
FFI, bytecode/loadfile/dofile, coroutines, mutable ambient globals, `pairs`/`next`, or exception
paths capable of clearing a host-side limit latch. Omit `pcall`/`xpcall` unless the selected design
proves that every host limit remains latched across them. Any hook/interrupt/memory/stack violation
is latched by Rust and rejects the evaluation even if script code catches an interpreter error.

Imports must retain the current pre-execution evidence model:

- only grammar-recognized literal imports are accepted;
- embedded ABI imports use names such as `cast.package.v3`;
- relative imports include an exact `.lua` extension and resolve through `SourceRoot`;
- absolute paths, traversal, NUL, ambient `LUA_PATH`/`LUA_CPATH`, dynamic/computed imports, and
  cross-language imports are rejected;
- the complete graph is loaded, bounded, cycle-checked, and fingerprinted before VM execution;
- the VM receives only the prepared in-memory module registry.

If the selected dialect lacks a maintained grammar-aware way to prove literal imports, stop at
Phase 0. A text/regex scanner is not acceptable, and changing to runtime-only import tracing needs
a separate explicit policy decision.

### 4.5 File discovery, collisions, and generated output

In temporary or permanent dual mode:

- Extension is authoritative: `.lua` selects Lua and `.glu` selects Gluon.
- `stone.lua` and `stone.glu` in one recipe directory are a hard error before evaluation.
- Fixed intents such as `system`, `boot-topology`, and `root-filesystem` hard-fail when both
  extensions exist.
- Within one fragment layer, `foo.lua` plus `foo.glu` is a hard duplicate error. Across vendor,
  admin, and user layers, the existing higher-layer whole-fragment override by logical stem remains
  valid regardless of language, but every discovered fragment is still validated first.
- No import crosses engines.
- No command retries a failed declaration under the other engine.

Generated artifacts have one authority, never dual writes:

- `stone.lua` produces `sources.lock.lua` and `build.lock.lua`; `stone.glu` keeps the `.glu` pair.
- A save operation preserves the language of an existing generated fragment; a newly created
  fragment requires an explicit language, with Lua becoming the CLI default only after activation.
- A system snapshot uses the language of its accepted system intent and records that language in
  provenance. The corresponding alternate-extension snapshot must not coexist in the same active
  logical slot; immutable historical generations may retain their original snapshot.
- Emitters sort keys, use stable escaping/tagged values, never rewrite authored files, and emit no
  metatables or executable ambient lookups.

Every save, delete, update, and language-switch operation must inspect both extensions through the
same retained directory authority used for mutation. An alternate-extension authored file blocks
the operation. Alternate generated locks/snapshots also hard-fail until an explicit transactional
language migration validates the replacement, durably switches the authoritative names, and
removes stale generated names. A normal save/delete command must never create `foo.lua` beside
`foo.glu` or silently orphan a `.glu` lock pair after switching to `stone.lua`. Tests must interrupt
the migration at every persistence boundary and prove one recoverable authority remains.

Do not bump names such as `cast.package.v3` merely because their implementation language changes.
Provide a Gluon and Lua implementation of the same semantic ABI during coexistence. Bump an ABI
version only when its domain meaning changes. Engine/language identity belongs in fingerprint v2.

### 4.6 Installed-state upgrade contract

Endpoint A requires a bridge release; deleting repository Gluon support is not enough. Phase 0
must inventory active authored `.glu` (including `/etc/cast/system.glu`), active generated
snapshots/locks (including `/usr/lib/system-model.glu`), and every archived state snapshot that can
still be exported or activated. The default upgrade sequence is:

1. ship one dual-runtime bridge version that can read v1/Gluon state and write v2/Lua state;
2. require operators to supply/approve Lua replacements for authored files—never overwrite
   authored Gluon automatically;
3. transactionally create canonical Lua copies/index entries for active and archived generated
   snapshots without modifying the immutable old generations;
4. record a durable migration-complete marker only after live load, archived export, interrupted
   resume, and rollback checks pass;
5. make the final Lua-only version refuse upgrade with a precise diagnostic if unmigrated state is
   found, directing the operator through the bridge version;
6. remove Gluon only after an old installation containing archived states passes the complete
   upgrade/export/rollback acceptance path.

If immutable archived snapshots cannot be represented without in-place mutation, stop and choose
either a bounded legacy generated-snapshot reader or a new versioned archive representation. Do not
pretend regeneration covers historical state, and do not retain unrestricted Gluon evaluation as a
hidden fallback in the Lua-only endpoint.

## 5. Domain impact map

| Area | Primary paths | Required change |
|---|---|---|
| Shared evaluation | `crates/gluon_config/src/{source,limits,deadline,diagnostic,fingerprint}.rs` | Extract generic behavior into `crates/declarative_config`; leave Gluon adapters |
| Lua adapter | new `crates/lua_config/` | VM, prepared imports, capability allowlist, output-tree validator, diagnostics |
| Workspace/dependencies | `Cargo.toml`, `Cargo.lock`, `flake.nix`, CI/release workflows | Add only selected engine/features; prove musl/linkage/licenses; later remove Gluon only for Endpoint A |
| Config store | `crates/config/src/gluon.rs`, `src/gluon/fragment_collection.rs`, `src/gluon/atomic_persistence.rs`, `src/rooted_gluon.rs` | Generalize public names, extension dispatch, collision rules, atomic persistence |
| Package ABI | `crates/stone_recipe/gluon/package.glu`, `src/package/gluon.rs` | Shared wire DTO; Lua ABI; package parity |
| Build policy/builders | `crates/stone_recipe/gluon/build_policy*.glu`, `builders/*.glu`, related Rust modules | Lua ABIs, immutable helpers, policy/patch parity |
| Locks/provenance | Mason `source_lock.rs`; Stone Recipe build-lock and derivation modules | Split near-limit source-lock file; language-aware canonical codecs; fingerprint v2 |
| Recipes/CLI | `crates/mason/src/recipe.rs`, `draft.rs`, `cli/recipe/` | Exact root selection, Lua drafts, explanations, no fallback |
| Profiles | `crates/mason/src/profile.rs`, `data/profile.d/`, `crates/config` | Shared DTO, Lua codec/emitter, layer parity |
| Repositories | `crates/forge/src/repository/` | Shared DTO, Lua codec/emitter, layer parity |
| Triggers | `crates/triggers/src/gluon.rs`, Forge trigger discovery | First pilot domain, exact ABI evidence, read-only Lua path |
| System model | `crates/forge/src/system_model/` | Lua intent and canonical snapshot with explicit language provenance |
| Boot intents | Forge active-reblit boot-topology/root-filesystem modules | Lua adapters and exact fingerprint contracts; VM-only system acceptance |
| Semantic build identity | `crates/tools_buildinfo/src/semantic_fingerprint.rs` | Include selected Lua ABI trees and tests in implementation fingerprint |
| Gates/tooling | `misc/make/*.mk`, `misc/scripts/check-config-formats.sh`, `check-source-loc.sh` | Language-neutral target names, Lua fixtures, retain YAML/KDL rejection, count `*.lua` LOC |
| Corpus/docs | 218 `.glu` files, `docs/examples/gluon/`, `docs/gluon-configuration.md`, README/PLAN files | Paired corpus during parity; convert/delete only at Endpoint A finalization |

`crates/mason/src/source_lock.rs` is already near the 1,000-line limit. Split it by decoding,
canonical emission, and domain validation before adding Lua behavior. Apply the same functional
split rule to every touched file that would cross the limit.

## 6. Implementation phases and commit boundaries

Each phase ends with a clean tree, a focused Make gate, and one or more cohesive commits. If a
phase needs unrelated work, record that work in `FUTURE_PLAN.md` instead of widening the phase.

### Phase 0 — Record choices and prove the engine

1. Clear the known baseline LOC blocker separately, then add `*.lua` classification and tests to
   the source-LOC gate before adding any Lua source.
2. Teach `tools_buildinfo` semantic collection to include the chosen crate-level Lua ABI tree and
   pin inclusion/exclusion tests before adding any Lua ABI.
3. Add exact Make targets `lua-engine-spike` and `lua-engine-spike-release` for Lua 5.4 and Luau
   experiments without connecting either to production. The release target must execute the built
   release evaluator tests, not merely compile them.
4. Prove the eight engine gates in section 3.1, including release-profile musl execution.
5. Inventory every current focused test as generic, Gluon-specific, or future differential.
6. Inventory every persisted declaration/lock/snapshot and its reader/writer/rollback lifetime,
   including state-query/export readers and the installed-state bridge in section 4.6.
7. Select dialect, exact `mlua` features, import parser, and Endpoint A or B.
8. Update this plan and reconcile `PLAN.md`, `FUTURE_PLAN.md`, README, and current configuration
   documentation. Put rejected engines and deferred enhancements in `FUTURE_PLAN.md`.

**Commit examples:** `test(lua): prove evaluator runtime constraints`, then
`docs(plan): select lua migration endpoint`.

**Exit:** no production consumer uses Lua; `make source-loc` recognizes Lua and passes; semantic
fingerprint tests recognize a Lua ABI tree; all current Make gates still pass; the selected engine
has evidence for sandbox, import, resource, musl, deterministic, and licensing contracts.

### Phase 1 — Extract the language-neutral core

1. Create `crates/declarative_config` and move `Source`, `SourceRoot`, and hardened reads first.
2. Move generic limits and total-deadline accounting in a second green slice.
3. Move engine-neutral diagnostic primitives and prepared-module graph policy separately from
   Gluon AST parsing and VM interruption.
4. Keep public compatibility re-exports in `gluon_config` only while consumers migrate.
5. Add characterization tests before and after each move; do not change fingerprint bytes yet.

**Commit examples:** `refactor(config): extract hardened source loading`,
`refactor(config): extract evaluation limits and deadline`, and
`refactor(config): separate prepared module policy`.

**Exit:** Gluon-only behavior and every existing security gate are byte/diagnostic compatible;
there is one hardened loader implementation, not two.

### Phase 2 — Introduce fingerprint v2 deliberately

1. Implement the schema in section 4.2 and canonical encoding/validation.
2. Migrate derivation provenance, explanations, package metadata, locks, triggers, and boot intent
   checks as separate commits.
3. Add v1-read/stale-reject tests and prove no old lock is accepted as current.
4. Preserve historical generation references and prove rollback does not re-evaluate stale v1
   declarations.
5. Document expected derivation/cache invalidation.

**Commit examples:** `feat(config): add language-tagged evaluation fingerprint`, then
`refactor(recipe): consume evaluation fingerprint v2`.

**Exit:** Gluon evaluations use v2 correctly before Lua can create a domain value.

### Phase 3 — Build the isolated Lua evaluator

1. Create `crates/lua_config` using only the selected engine/features.
2. Implement the empty capability environment, prepared literal imports, one total deadline,
   host-latched resource failures, panic containment, and a fresh VM per call.
3. Validate the bounded Lua value tree before serde and existing semantic conversion.
4. Map parse/import/schema/limit/runtime/internal failures into stable generic diagnostics with
   logical source names and line/column or field paths where available.
5. Add adversarial tests for every forbidden capability and bypass listed in sections 3 and 4.

**Commit examples:** `feat(lua): add restricted evaluator`, then
`test(lua): pin resource and capability boundaries`.

**Exit:** evaluator tests pass in debug and release profiles; no production domain dispatches to
Lua yet.

### Phase 4 — Establish shared schema and one complex proof

1. Define shared wire representations for closed records, arrays, tagged variants, options, and
   patches. Do not maintain separate semantic models per engine.
2. Convert one representative complex contract—package v3 or build-policy v5—without exposing it
   publicly.
3. Differentially evaluate paired Gluon/Lua fixtures into normalized Rust domain values.
4. Cover missing/unknown fields, option/patch tags, integer boundaries, aliasing, mutation,
   sparse/mixed tables, cycles, and stable diagnostics.

**Commit:** `refactor(recipe): share declaration wire schema`.

**Exit:** the hardest DTO shape has semantic parity and intentionally different engine-tagged
fingerprints.

### Phase 5 — Pilot Lua through triggers

Triggers are the smallest public ABI and are read-only, making them the first end-to-end domain.

1. Add `cast.trigger.v1` as a Lua ABI with the same domain semantics.
2. Add exact extension selection and rooted loading without changing other domains.
3. Require the same explicit ABI/import and empty-input fingerprint contract as Gluon.
4. Add paired success, failure, determinism, and provenance fixtures.

**Commit:** `feat(triggers): accept restricted lua declarations`.

**Exit:** trigger planning is identical after normalization; no transaction runs during host tests.

### Phase 6 — Generalize the configuration manager

1. Replace `GluonCodec`/`DecodedGluon`/`load_gluon`-shaped internals with engine-neutral names.
2. Preserve atomic persistence, authored-file protection, retained descriptors, root identity,
   layer order, and fail-closed evaluation.
3. Implement extension dispatch and same-stem collision rules exactly as section 4.5.
4. Migrate profile and repository fragments with paired fixtures and canonical Lua emitters.
5. Preserve temporary compatibility re-exports only until all in-tree callers move.

**Commit examples:** `refactor(config): generalize declaration storage`,
`feat(profile): add lua fragments`, and `feat(repository): add lua fragments`.

**Exit:** mixed-layer behavior is deterministic; same-layer collisions are errors; no fallback or
dual write exists.

### Phase 7 — Migrate recipes, policy, and generated locks

1. Split `source_lock.rs` by function before extending it.
2. Add Lua implementations of package v3, builder v2, build-policy v5, and policy-layers v1.
3. Add exact `stone.lua`/`stone.glu` root collision handling.
4. Add language-matched source-lock and build-lock codecs and canonical emitters.
5. Migrate recipe draft/check/explain CLI behavior and provenance labels.
6. Differentially test every root contract and representative package corpus before expanding to
   all examples.

**Commit examples:** `refactor(lock): separate source lock codecs`,
`feat(recipe): evaluate stone lua declarations`, and
`feat(lock): emit language-matched canonical locks`.

**Exit:** a Lua recipe builds the same normalized package plan and expected package filename as its
Gluon pair while retaining intentionally distinct evaluation/derivation identity; the `.stone`
format is unchanged, and authored/generated files cannot conflict silently.

### Phase 8 — Migrate system and boot intent

1. Add Lua implementations of system v1, boot-topology v2, and root-filesystem v1.
2. Generalize fixed-path discovery and hard-fail alternate-extension conflicts.
3. Emit and re-evaluate a language-matched canonical system snapshot.
4. First pass all evaluator/model/plan tests on the host without touching disk state.
5. In the disposable UEFI VM, rerun only the already established, user-approved VM gates needed to
   show that equivalent Lua intent reaches the same Rust plans and existing publication paths.
   Preserve every baseline non-claim: this migration does not close deferred startup repair,
   interruption, selected-payload bootability, reboot recovery, or power-loss durability.
6. Exercise the installed-state bridge with active and archived v1 data, including export,
   interrupted resume, and rollback selection, without rewriting the historical generation.
7. Never reboot autonomously. A reboot experiment remains deferred to `FUTURE_PLAN.md` and needs
   fresh user approval plus confirmed VM snapshot, guest identity, target-disk identity, and a plan
   for losing guest state/SSH or returning to installation media.

**Commit examples:** `feat(system): accept lua system intent`, then
`test(vm): prove lua atomic activation and rollback`.

**Exit:** Lua reaches the same accepted model and VM boundaries as Gluon, and the report repeats the
baseline's exact non-claims. No risky operation ran on the host and no deferred system-manager
closure was pulled into this migration.

### Phase 9 — Convert the corpus, documentation, and tooling

1. Pair all 13 ABI modules, five shipped declarations, 68 fixtures, and 132 documentation examples.
2. Translate mechanically where safe; hand-review ABI modules and the files using pattern matching,
   recursion, variants, option/patch semantics, and array/string primitives.
3. Add differential gates over all paired domain roots. Compare normalized Rust values; assert that
   fingerprints differ by engine as designed.
4. Update Make fragments, scripts, CLI help, README, plan documents, and configuration guide.
5. Rename `check-config-formats.sh` or its messages so it permits the selected declarative
   language set while continuing to reject YAML/KDL exactly. Audit that the Phase 0 `*.lua` LOC
   and semantic-ABI fingerprint coverage still spans the final corpus layout.
6. Include selected runtime notices in license output.

**Commit in small corpus/domain slices**, not one repository-wide conversion commit.

**Exit:** every public example and fixture has a passing Lua form; docs describe the exact dialect,
capability/import rules, collision semantics, generated files, and migration path.

### Phase 10 — Release parity gate

Run the complete host-safe matrix through Make in debug and release configurations:

- evaluator/source-root security and races;
- import graph bounds and fingerprints;
- all 12 root declaration shapes;
- fragments and persistence;
- package, policy, profile, repository, trigger, system, and boot models;
- canonical source/build locks and system snapshots;
- repeated-process determinism;
- examples and semantic implementation fingerprints;
- musl/release binary execution and license notices.

The exact release execution gate is `make lua-release-test`; it must run evaluator and parity tests
against release-built binaries. `make build` alone is not release execution, and `make test` alone
is the debug test lane. Run `make declarative-config-test`, `make lua-config-test`,
`make declaration-parity-test`, and `make lua-release-test` before the aggregate project gates.

Then run the bounded disposable-VM parity matrix from Phase 8 without claiming the deferred reboot,
power-loss, or startup-repair campaigns. Fix only failures caused by this plan; record unrelated
improvements in `FUTURE_PLAN.md`.

**Exit:** Lua is production-capable and the tree is ready for the chosen endpoint.

### Phase 11A — Finish the recommended Lua-only cutover

1. Make Lua the only accepted declaration extension and remove temporary dispatch/fallback-shaped
   compatibility code.
2. Remove production `Getable`/`VmType` derives, Gluon DTOs, `gluon_config`, workspace Gluon/codegen
   dependencies, Gluon ABI sources, `.glu` data, fixtures, and examples.
3. Remove Gluon-only APIs, target names, diagnostics, documentation promises, and lexical paths.
4. Regenerate the lockfile and measure the net removed/added dependency and license surface. Do not
   repeat the old unverified “52 crates removed” claim.
5. Rerun Phase 10 and the VM matrix after deletion.
6. Do not remove the Gluon runtime/decoders until the bridge release has atomically migrated every
   active and archived state, and old-installation upgrade/export/rollback acceptance passes.

**Commit in domain/dependency/doc slices.** The final cleanup commit must contain only dead Gluon
removal after all consumers are already on Lua.

### Phase 11B — Finish permanent dual support instead

1. Remove only temporary migration shims; retain explicit extension dispatch.
2. Keep complete Lua and Gluon ABI, evaluator, corpus, diagnostics, docs, and release gates.
3. Document that both runtimes are security-critical and that Gluon dependencies remain.
4. Rerun both matrices for every ABI or evaluator-policy change going forward.

Do **one** of Phase 11A or 11B, never a partial mixture.

## 7. Make-driven validation matrix

During implementation, add the exact root targets `declarative-config-test`, `lua-config-test`,
`declaration-parity-test`, `lua-release-test`, and `lua-dependency-audit`, then compose them into
existing project targets. Users must not need raw Cargo commands to reproduce acceptance.

| Gate | Location | Required evidence |
|---|---|---|
| Focused evaluator/core | Host | sandbox/capabilities, source races, imports, resource latches, output-tree bounds |
| Domain differential | Host | paired normalized Rust values for all 12 root shapes |
| Storage/discovery | Host temp dirs | collisions, layer precedence, atomic save/delete, authored protection |
| Determinism | Host isolated processes | repeated values and fingerprints; no ambient env/path/time/random influence |
| Full project | Host | `make build`, `make test`, `make check`, `make examples`, `make source-loc` |
| Packaging | Host/CI-safe | `make lua-release-test` executes release tests; musl binary runs; linkage, semantic fingerprints, notices correct |
| Existing system/boot parity | Disposable VM only | rerun only accepted baseline gates against equivalent Lua-derived Rust plans, with the same explicit non-claims |

Commands that execute the already-built evaluator or VM tooling should have bounded runtime. Builds
and Make compilation targets should not be wrapped in arbitrary timeouts; do not wrap one combined
compile-and-run command when that would also time-limit compilation.

## 8. Completion criteria

### Shared Lua production readiness

- One selected and accurately named Lua dialect is reproducibly built.
- There is exactly one hardened source loader and one engine-neutral limits/fingerprint policy.
- Lua has no ambient capabilities or search paths and cannot bypass latched resource limits.
- All 12 root declaration shapes reach existing Rust semantic validators.
- Every generated file has one authoritative language and canonical emitter.
- Evaluation fingerprints distinguish language, engine, policy, ABI, source graph, and inputs.
- `.stone` format and transaction/system architecture are unchanged.
- All host-safe gates and the bounded, already-established disposable-VM parity gates pass with
  exact non-claims for deferred system-manager closure.
- No touched file exceeds 1,000 LOC.

### Additional Endpoint A criteria: Lua-only

- `git ls-files '*.glu'` is empty.
- No production dependency on `gluon`, `gluon_codegen`, or `gluon_config` remains.
- No production `Getable`/`VmType` DTO derives or Gluon-specific public APIs remain.
- No CLI, documentation, Make gate, semantic fingerprint, or generated-file path promises Gluon.
- YAML and KDL remain rejected; there is no hidden legacy fallback.
- The bridge-release migration marker proves every active/archive state was migrated; an old
  installation with archived states passes upgrade, export, rollback selection, and interruption
  recovery before the last Gluon decoder is removed.

### Additional Endpoint B criteria: permanent dual

- Every supported domain accepts both `.lua` and `.glu` through explicit extension dispatch.
- Same-stem collisions, cross-language imports, fallback, and dual writes are test-pinned errors.
- Equivalent declarations normalize to equal domain values but intentionally different evaluation
  and derivation identities.
- Both complete evaluator/security/corpus/release matrices remain mandatory.

## 9. Effort and trade-off estimate

These are engineering ranges, not calendar promises. Refine them after Phase 0.

| Workstream | Estimate |
|---|---:|
| Engine proof plus language-neutral evaluation/fingerprint foundation | 5–8 engineer-weeks |
| Lua evaluator, shared schemas, and all domain bridges | 10–16 engineer-weeks |
| Corpus, documentation, tooling, packaging, and VM acceptance | 8–12 engineer-weeks |
| **Production dual-capable foundation through Phase 10** | **23–36 engineer-weeks** |
| Optional final Gluon deletion and post-delete proof | 3–5 engineer-weeks |
| **Recommended Lua-only endpoint** | **26–41 engineer-weeks** |

Permanent coexistence avoids the final deletion work but retains both dependency trees and creates
ongoing doubled security, ABI, documentation, and release-test cost. It is not the cheaper long-term
option.

## 10. Stop conditions

Stop the current phase and report evidence—do not improvise—if any of these occur:

- neither engine can meet deadline, memory, stack/call-depth, capability, deterministic, or musl
  release contracts;
- imports cannot be prepared with a real dialect-aware parser before execution;
- Lua output cannot be cycle-safe and bounded before serde conversion;
- fingerprint migration could allow a v1 lock/artifact to be accepted as current;
- persisted-file ownership or same-stem collision behavior remains ambiguous;
- a change would weaken `SourceRoot`, descriptor identity, atomic persistence, or authored-file
  protection;
- a touched file reaches 1,000 LOC without a functional split;
- a required test would touch host ESP/BOOT/block devices/system activation;
- completion would require changing `../bedrock`, release/version metadata, adding Nix translation,
  restoring YAML/KDL, or adding formatting/typo-policy configuration;
- an unrelated failure or desirable feature is outside this plan—record it in `FUTURE_PLAN.md`.

## 11. First executor batch

The first implementation batch is intentionally narrow:

1. Confirm a clean `develop` tree and record the current commit.
2. Clear the pre-existing repository LOC blocker outside the Lua commit, then make `*.lua` visible
   to `make source-loc` and add Lua ABI-tree semantic-fingerprint coverage.
3. Commit those two pre-source guardrails.
4. Add only the Phase 0 Make-driven engine spike and tests.
5. Run the host-safe engine/release gates; do not connect Lua to a domain.
6. Commit the spike evidence.
7. Record the dialect and endpoint decision in this plan and reconcile governing docs in a second
   documentation-only commit.
8. Stop for review before extracting `declarative_config`.

## 12. Primary external references

- [`mlua` 0.12 documentation](https://docs.rs/mlua/latest/mlua/) — supported runtimes, serde, and
  conversion model.
- [`mlua::Lua` API](https://docs.rs/mlua/latest/mlua/struct.Lua.html) — selected libraries, memory
  limits, hooks/interrupts, sandboxing, and custom Luau require support.
- [Lua 5.4 reference manual](https://www.lua.org/manual/5.4/) — language and standard-library
  capabilities that the host must explicitly exclude.
- [Luau sandbox documentation](https://luau.org/sandbox/) — the upstream sandbox baseline, which
  is still broader than Cast's required empty-by-default environment.
- [Luau compatibility documentation](https://luau.org/compatibility/) — why Luau must not be
  described as Lua 5.4.
