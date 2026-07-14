
# Historical plan: Establish Gluon as the Declarative Language for OS Tools

> **ARCHIVED — DO NOT EXECUTE.** This migration is complete. Everything below
> records the pre-Cast repository at commit `80d7ac5`: split-product names,
> paths, package targets, commands, and validation snippets are historical
> evidence, not current usage or compatibility guidance. Do not copy or run
> any shell fragment from this file. Current behavior is documented in
> [`../../README.md`](../../README.md),
> [`../gluon-configuration.md`](../gluon-configuration.md), and
> [`../architecture/cast.md`](../architecture/cast.md).

## Status

- **Priority**: P1
- **Effort**: L, expected as a sequence of reviewable changes rather than one PR
- **Risk**: HIGH
- **Depends on**: none
- **Category**: architecture, migration, security, tests, DX
- **Planned at**: commit `80d7ac5`, 2026-07-12
- **Implementation**: complete, 2026-07-13

## Historical goal

At the time of this plan, the goal was to make Gluon the single canonical
human-authored declarative language for Boulder
and Moss configuration in this repository. The end state removes YAML and KDL
from package recipes, Boulder macro/profile configuration, recipe overlays, Moss
repository and trigger configuration, and Moss system-model intent.

The migration must establish a secure, deterministic, typed evaluation
foundation before converting individual formats. It must not merely rename
`stone.yaml.in`-style text templates to `.glu.in`, preserve YAML/KDL behind a
Gluon wrapper, or expose Gluon's host I/O facilities during configuration
evaluation.

## Historical non-goals

- Migrating or editing `../bedrock`; its recipes, scripts, documentation and
  generated artifacts are explicitly owned by the user and out of scope.
- Removing YAML required by external platforms such as GitHub Actions or
  Dependabot.
- Replacing Boulder's build sandbox, Moss's transaction engine, the `.stone`
  archive format, or shell as the initial execution language for build phases.
- Changing workspace release/version metadata.
- Keeping permanent YAML/KDL compatibility. A short, explicit read-only
  deprecation window is acceptable; dual-writing and hidden fallback are not.
- Making Moss or Boulder edit arbitrary user-authored Gluon expressions.

## Why this matters

The current formats are data serializers with several extra mutation and macro
layers. Gluon can provide immutable records, functions, imports, static typing
and reusable policy modules, which are the properties needed for Nix-like
declarative composition. However, an embedded language is executable code: the
runtime, import policy, resource limits, type boundary and provenance model are
now security and reproducibility boundaries.

Building those boundaries once in a small shared crate avoids independent,
inconsistent Gluon VMs in Boulder and Moss. It also permits YAML and KDL to be
removed rather than retained as internal intermediate formats.

## Baseline state at `80d7ac5`

### Repository and validation conventions

- The workspace is Rust 2024 and requires Rust 1.91 (`Cargo.toml`).
- The root `Makefile` is the repository-native build, lint and test interface.
- `make test` runs Clippy, formatting, typos and `cargo test --all`.
- At the planned commit, `env RUSTUP_TOOLCHAIN=1.93.0 cargo test --all` passes.
- In the planning environment, the validation command used Rust 1.88 and failed the
  MSRV check; with Rust 1.93 it reached the typos step and failed because the
  external `typos` executable was absent. Treat these as environment
  prerequisites, not product failures.

### YAML recipe boundary

`crates/stone_recipe/src/lib.rs:25-31` directly decodes YAML:

```rust
pub fn from_slice(bytes: &[u8]) -> Result<Recipe, Error> {
    serde_yaml::from_slice(bytes)
}

pub fn from_str(s: &str) -> Result<Recipe, Error> {
    serde_yaml::from_str(s)
}
```

`bin/boulder/src/recipe.rs:30-45` loads `stone.yaml` and then optionally applies a
KDL control file:

```rust
let path = resolve_path(path)?;
let control_file_path = path.with_file_name("control.kdl");
let source = fs::read_to_string(&path).map_err(Error::LoadRecipe)?;
let mut parsed = stone_recipe::from_str(&source)?;
```

Directory resolution is hard-coded to `stone.yaml` at
`bin/boulder/src/recipe.rs:136-147`. CLI defaults in
`bin/boulder/src/cli/build.rs` and `bin/boulder/src/cli/chroot.rs` do the same.

### Source mutation boundary

Boulder edits YAML text in place:

- `bin/boulder/src/upstream.rs:188-193` rewrites Git refs after resolution.
- `bin/boulder/src/upstream.rs:224-280` uses `yaml::Updater` for those edits.
- `bin/boulder/src/cli/recipe.rs:258-274` edits release fields.
- `bin/boulder/src/cli/recipe.rs:319-436` edits versions, URLs, hashes and refs.
- `crates/yaml/src/updater.rs` is a line-oriented YAML source updater.

Arbitrary Gluon expressions cannot be safely or generally rewritten this way.
Machine-resolved source information must move into a generated lock artifact.

### Macro and configuration boundaries

- `bin/boulder/src/macros.rs:20-49` scans only `*.yaml` files and deserializes them
  into `stone_recipe::Macros`.
- `crates/stone_recipe/src/control_file.rs` implements append/prepend/override
  recipe changes in KDL.
- `crates/config/src/lib.rs:55-90` loads YAML and KDL, gives KDL higher priority,
  and saves KDL by default.
- `crates/config/src/lib.rs:231-245` silently converts read or parse failures to
  `None`. The new evaluator must return path-aware errors instead.
- `crates/triggers/src/format.rs:126-140` tests trigger decoding through YAML.

### System-model boundary

- `bin/moss/src/installation.rs:105-106` loads
  `/etc/moss/system-model.kdl` as declarative system intent.
- `bin/moss/src/system_model/decode.rs` manually parses repositories and packages.
- `bin/moss/src/system_model/encode.rs` generates KDL snapshots.
- `bin/moss/src/system_model/update.rs` edits KDL while trying to retain comments
  and formatting.
- `bin/moss/src/client/mod.rs:1267-1303` records an updated
  `/usr/lib/system-model.kdl` in every produced system state.
- `bin/moss/src/cli/sync.rs:37-42` and `bin/moss/src/cli/state.rs:79-89` expose KDL in the
  CLI import/export contract.

User-authored Gluon intent and Moss-generated state snapshots must be separate.
Moss must never try to round-trip an arbitrary functional program.

## Architecture decisions

These decisions are requirements, not optional suggestions.

### A. One shared restricted evaluator

Add `crates/gluon_config` as the sole place that creates and configures Gluon
VMs. Boulder, Moss, `stone_recipe`, `config` and `triggers` must not construct
their own general-purpose VMs.

The crate must provide:

```text
crates/gluon_config/
  Cargo.toml
  src/
    lib.rs
    diagnostic.rs
    evaluator.rs
    fingerprint.rs
    import.rs
    limits.rs
    source.rs
  gluon/
    core/
```

The final names may change slightly to match implementation constraints, but
the responsibilities must remain separated and tested.

### B. Configuration evaluation is pure

The VM must not make the following available to evaluated configuration:

- filesystem reads or writes, except source loading performed by the controlled
  Rust importer;
- process execution;
- network access;
- environment variables or ambient `GLUON_PATH`;
- wall clock or nondeterministic time;
- random-number facilities;
- arbitrary native Rust functions.

Do not use Gluon's default `new_vm`/`VmBuilder` unchanged if it registers host
filesystem, I/O or process modules. Construct a restricted environment or a
whitelisted importer and prove the restriction with negative tests.

### C. Imports are explicit and contained

Support two import classes:

1. Embedded, versioned modules shipped by `os-tools`, addressed under stable
   namespaces such as `boulder.*` and `moss.*`.
2. Relative source modules contained beneath an explicitly supplied source
   root.

Canonicalize every relative import, reject traversal and symlink escapes, do
not search the current working directory implicitly, and ignore `GLUON_PATH`.
Record every imported module in the evaluation fingerprint.

### D. Resource use is bounded

Every evaluation must have:

- a configurable but conservative memory limit;
- a wall-clock watchdog which calls the VM interrupt mechanism;
- a maximum source/import graph size;
- a maximum imported file size;
- an error that distinguishes limit exhaustion from parse, type and conversion
  failures.

Do not merge a runtime that can hang indefinitely on a recursive expression.

### E. Gluon values cross through DTOs

Do not expose internal domain structs as the public language ABI. Each consumer
defines a Gluon-facing DTO made only from stable primitive values, arrays, maps,
options and explicit variants. Convert the DTO into the existing domain model
with `TryFrom` and run semantic validation there.

Examples of values that stay on the Rust side until conversion:

- `url::Url`;
- `PathBuf`;
- `fnmatch::Pattern`;
- repository identifiers and scoped versions;
- dependency/provider parsers;
- package path-kind validation.

### F. Evaluation produces provenance

Return both the typed result and an `EvaluationFingerprint` containing at least:

- hash of the root source;
- ordered paths/names and hashes of imported modules;
- Gluon dependency version or pinned revision;
- `os-tools` configuration ABI/schema version;
- hash of explicit input/lock data;
- evaluator policy version.

Do not include host paths in reproducible identifiers when a stable logical
module name is available.

### G. Authored source and generated state are different artifacts

- Authored `.glu` files may use functions and imports and are never rewritten.
- Generated `.glu` files are canonical, standalone literals marked as
  generated.
- Resolved upstream information belongs in a generated lock artifact, not in
  source edits.
- Moss state snapshots are generated values, not rewrites of system intent.

### H. Gluon becomes canonical, not an adapter to YAML/KDL

The in-memory Rust model is the only intermediate representation. No Gluon
loader may serialize to YAML or KDL and feed the old loaders. At the end of the
migration, `serde_yaml`, `kdl`, `crates/yaml` and the KDL Git patch must be gone
unless another independently justified non-configuration use remains.

## Commands required

| Purpose | Command | Expected on success |
|---|---|---|
| Targeted evaluator tests | `env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p gluon_config` | exit 0; positive and negative policy tests pass |
| Recipe tests | `env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p stone_recipe -p boulder` | exit 0 |
| Moss/config tests | `env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p config -p triggers -p moss` | exit 0 |
| Workspace tests | `env RUSTUP_TOOLCHAIN=1.93.0 cargo test --all` | exit 0; all unit and doc tests pass |
| Formatting | `env RUSTUP_TOOLCHAIN=1.93.0 cargo fmt --all -- --check` | exit 0, no diff |
| Clippy | `env RUSTUP_TOOLCHAIN=1.93.0 cargo clippy --workspace -- --no-deps` | exit 0; no new warnings in changed code |
| Repo-native final gate | `env RUSTUP_TOOLCHAIN=1.93.0 make test` | exit 0 when `typos` is installed |

Do not install tools or dependencies by editing the host environment silently.
If `typos` is unavailable, report that prerequisite while still running the
verified Cargo, formatting and Clippy lanes.

## Scope

### In scope

- `Cargo.toml`, `Cargo.lock`
- `Makefile` only if new check/conversion targets are required
- `README.md` and in-repository user-facing format documentation
- new `crates/gluon_config/**`
- `crates/stone_recipe/**`
- `crates/config/**`
- `crates/triggers/**`
- `crates/yaml/**` for eventual deletion
- `bin/boulder/Cargo.toml`, `bin/boulder/src/**`, `bin/boulder/data/**`
- `bin/moss/Cargo.toml`, `bin/moss/src/**`
- `tests/**` and new format fixtures
- repository-owned examples required to prove the new language

### Explicitly out of scope

- `../bedrock/**` in all phases, including read-time generation, documentation,
  scripts, recipes, artifacts and pins
- `.github/workflows/*.yaml`, `.github/dependabot.yml`
- `.stone` binary/archive format changes
- unrelated transaction, VFS, package analysis, database or boot work
- workspace package version changes
- publishing, pushing or opening a PR

## Git workflow

- Use a dedicated branch such as `feature/gluon-config-foundation`.
- Prefer one commit per phase or independently reviewable slice.
- Match the repository's imperative commit style, for example
  `boulder: support package-private runtime closures`.
- Do not commit generated build output.
- Do not push unless explicitly requested.

## Implementation phases

### Phase 0: Pin and prove the Gluon dependency

1. Add an exact Gluon dependency pin at workspace level. Start by evaluating
   the latest released version available when implementation begins; do not use
   an unconstrained semver range. Disable unnecessary default features.
2. Record why each enabled feature is necessary.
3. Confirm the chosen release builds with Rust 1.91, Rust 1.93 and the Linux
   targets used by this project.
4. Produce a temporary size/build measurement for Boulder and Moss before and
   after linkage. Do not commit build artifacts; record results in the eventual
   architecture documentation.
5. Confirm the dependency and license fit the repository's licensing policy.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo check -p gluon_config
env RUSTUP_TOOLCHAIN=1.93.0 cargo tree -p gluon_config
```

Expected: both exit 0; the dependency is exact; optional I/O, web, random and
regex facilities are absent unless a later documented requirement proves one
necessary.

### Phase 1: Implement the restricted evaluator

1. Create `crates/gluon_config` and add it to the workspace.
2. Define `Evaluator`, `SourceRoot`, `ImportPolicy`, `Limits`, `Diagnostic`,
   `Evaluation<T>` and `EvaluationFingerprint` or equivalent types.
3. Implement embedded module loading without ambient filesystem lookup.
4. Implement contained relative imports with canonical path enforcement.
5. Implement memory, time, source-size and import-graph limits.
6. Normalize Gluon errors into path/span/category diagnostics without dropping
   their original source chain.
7. Ensure evaluation never calls `process::exit`, writes diagnostics directly
   to stdout/stderr, or panics on user input.

Required tests in `crates/gluon_config/tests/`:

- a literal typed record evaluates successfully;
- a relative import inside the source root succeeds;
- `../` traversal is rejected;
- a symlink escaping the source root is rejected;
- ambient current-directory imports are rejected;
- setting `GLUON_PATH` does not affect resolution;
- importing filesystem, I/O, process and random modules fails;
- infinite recursion is interrupted within the configured deadline;
- memory exhaustion returns a structured limit error;
- imported module content changes the fingerprint;
- path aliases resolving to the same module produce a stable logical identity;
- malformed and ill-typed programs include source path and span information.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p gluon_config
```

Expected: exit 0 and all positive, security and determinism tests pass.

### Phase 2: Define the versioned recipe ABI

1. Preserve the existing `stone_recipe::Recipe` as the domain model initially.
2. Add Gluon-facing DTOs with explicit names and fields. Avoid Serde-specific
   shapes such as untagged maps, scalar-or-sequence coercions and YAML stringy
   booleans.
3. Add `TryFrom<RecipeSpec> for Recipe` and conversion errors with field paths.
4. Move existing invariant checks currently in `bin/boulder/src/recipe.rs:57-71`
   into reusable recipe validation so every caller receives the same result.
5. Define an ABI version and embedded Gluon modules containing constructors,
   defaults and explicit variants for:
   - package metadata;
   - build phases and dependencies;
   - archive and Git upstreams;
   - paths/path kinds;
   - profiles and architectures;
   - subpackages;
   - tuning/toolchain options.
6. Use maps or arrays for dynamically named profiles/subpackages. Do not encode
   dynamic identifiers as fixed record fields.

Required tests:

- minimal package;
- all fields populated;
- archive and Git upstream variants;
- multiple profiles/subpackages;
- invalid URL, provider, version, release and path kind;
- default-value behavior;
- unknown field/type mismatch diagnostics;
- deterministic equality against the existing Rust model for equivalent data.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p stone_recipe
```

Expected: exit 0; conversions are explicit and no Gluon-facing DTO contains
`serde_yaml::Value`, `url::Url`, `PathBuf` or another domain parser type.

### Phase 3: Add canonical `stone.glu` loading to Boulder

1. Change directory resolution to prefer `stone.glu`.
2. During the short deprecation window only, accept `stone.yaml` when no
   `stone.glu` exists and emit one clear deprecation warning.
3. If both exist, fail with an ambiguity error; never silently choose one.
4. Update build/chroot CLI defaults and help to `./stone.glu`.
5. Preserve explicit file paths so fixtures can exercise both formats during
   migration.
6. Store evaluation fingerprint/provenance alongside the loaded recipe so
   package emission can eventually record it.
7. Add `boulder recipe check <path>` or equivalent if there is no cheap command
   that typechecks and validates a recipe without building it.

Required tests:

- explicit `.glu` path;
- directory containing only `stone.glu`;
- deprecated YAML-only directory;
- directory containing both formats fails;
- invalid Gluon returns source diagnostics;
- two evaluations of the same source yield the same recipe and fingerprint.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p boulder -p stone_recipe
```

Expected: exit 0; `stone.glu` is canonical and YAML is read-only compatibility.

### Phase 4: Replace source rewriting with generated locks

1. Define a versioned `sources.lock.glu` or equivalently named generated lock
   schema. The name must make its generated nature clear.
2. Make source resolution consume authored upstream declarations and return
   resolved URLs, hashes and full Git commits as lock data.
3. Change `boulder recipe update` and Git ref resolution to update the lock
   atomically instead of editing `stone.glu`.
4. Do not add a general Gluon AST/source updater.
5. Include lock content in the evaluation fingerprint and package provenance.
6. Preserve useful diffs by emitting stable field ordering and formatting.
7. Decide release/version bump behavior explicitly:
   - either make them normal authored values and require source edits, or
   - place them in a small generated input module imported by `stone.glu`.
   Do not heuristically edit arbitrary expressions.

Required tests:

- branch/tag resolves to a full commit in the lock;
- authored source remains byte-for-byte unchanged;
- unchanged resolution does not rewrite the lock;
- interrupted write leaves the previous lock valid;
- lock changes alter the fingerprint;
- missing/stale lock behavior is explicit and reproducible;
- malformed lock fails with a path-aware error.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p boulder -p stone_recipe
git diff --exit-code -- tests/fixtures/gluon/authored-source.glu
```

Expected: tests exit 0 and source resolution never mutates authored Gluon.

### Phase 5: Replace YAML macros and KDL control files with Gluon composition

1. Introduce embedded/versioned Gluon policy modules for the current action,
   definition, architecture, tuning and package-default libraries.
2. Initially, helpers may return command strings consumed by the existing build
   script parser, but new user-facing composition must happen in Gluon.
3. Replace `control.kdl` semantics with normal imports and functions accepting
   and returning `RecipeSpec` values. Do not add `control.glu` as a mandatory
   sidecar.
4. Provide clear helpers for append/prepend/override operations where useful,
   while retaining immutable functional behavior.
5. Migrate repository-owned macro fixtures and Boulder data to `.glu` modules.
6. Update `make get-started` data installation to ship Gluon modules rather
   than `*.yaml` macro files.
7. After all repository-owned users migrate, remove the `%()` macro mini-language
   in a separate reviewable change if Gluon modules provide all equivalent
   composition. Do not combine that removal with the initial evaluator commit.

Required equivalence tests:

- current action macro output;
- nested definition expansion;
- dependency collection from actions;
- architecture-specific definitions;
- package defaults and tuning groups;
- append/prepend/override behavior represented as Gluon functions.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p boulder -p stone_recipe
find bin/boulder/data -type f \( -name '*.yaml' -o -name '*.kdl' \) -print
```

Expected before final removal: tests pass and remaining YAML/KDL files are
listed and tracked as explicit compatibility work. Expected after this phase's
cleanup: no Boulder-owned macro/profile/control data remains in YAML/KDL.

### Phase 6: Migrate profiles, repositories and triggers

1. Refactor `crates/config` from a YAML/KDL serializer into a Gluon fragment
   loader using `gluon_config`.
2. Preserve system/vendor/admin/user precedence, but make precedence explicit
   and deterministic in tests.
3. Use `.glu` as the only write format for CLI-generated fragments.
4. Generated fragments must be standalone canonical literals and marked as
   generated; they are not allowed to overwrite authored modules.
5. Return parse/type/conversion errors instead of silently skipping invalid
   files.
6. Convert Boulder profiles and Moss repository configuration.
7. Convert triggers through a dedicated `TriggerSpec` DTO and semantic
   conversion into `fnmatch::Pattern` and `Handler` domain values.
8. Evaluate trigger configuration through the same restricted VM. A packaged
   trigger must not gain host process/filesystem access merely because Gluon is
   now the source language; only the already-existing trigger executor may run
   the validated handler later in its current sandbox/scope.

Required tests:

- vendor/admin/user precedence;
- deterministic fragment ordering;
- duplicate logical fragment handling;
- malformed fragment causes a visible error with its path;
- repository direct-index and root-index variants;
- trigger run/delete variants, inhibitors and path patterns;
- forbidden Gluon effects in trigger source;
- saving and deleting generated fragments.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p config -p triggers -p moss -p boulder
```

Expected: exit 0; invalid configuration cannot disappear silently.

### Phase 7: Split Moss system intent from generated state snapshots

Adopt these canonical roles:

```text
/etc/moss/system.glu       user-authored desired system intent
/usr/lib/system-model.glu  generated normalized snapshot for a Moss state
```

Exact directory placement may be adjusted only if existing installation/root
semantics require it, but the authored/generated separation is mandatory.

1. Define `SystemSpec`, `RepositorySpec` and package/provider selection DTOs.
2. Evaluate `/etc/moss/system.glu` without mutating it.
3. Generate a canonical standalone snapshot for every state.
4. Preserve the current ability to recreate, verify, archive, activate and
   export states using generated snapshots.
5. Change `moss sync --import` to evaluate a supplied `.glu` system expression.
6. Change `moss state export` to emit a standalone generated Gluon literal.
7. Keep repo add/remove/enable/disable forbidden while declarative system intent
   is active, matching current system-model behavior.
8. When repository metadata needs migration, print a structured suggested
   source change or regenerate only a generated snapshot. Never rewrite the
   authored expression.
9. Preserve comments only in authored source by leaving it untouched; do not
   attempt comment-preserving updates of generated snapshots.

Required tests:

- empty and populated system intent;
- default repository fields;
- package/provider selections;
- import into an ephemeral target;
- state creation and export;
- state verify/reblit preserving the normalized model;
- archived state activation;
- repository update suggestion without source mutation;
- forbidden imperative repository commands with active intent;
- source fingerprint recorded with the state snapshot.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo test -p moss
```

Expected: exit 0; all state-model tests operate without KDL and authored source
is unchanged across sync/update/export/verify operations.

### Phase 8: Deprecate, convert fixtures, then remove YAML and KDL

1. Convert every repository-owned recipe, test fixture, trigger, macro, profile
   and system-model fixture needed by `os-tools` tests.
2. Update README/CLI help/examples to show only Gluon as canonical.
3. Keep one release or another explicitly chosen short window of YAML read-only
   support if external consumers need it. Emit a warning naming the removal
   milestone.
4. Never accept KDL as higher priority than Gluon during this window. If both
   old and new formats exist, fail instead of silently shadowing.
5. Remove YAML and KDL loaders after the compatibility window.
6. Delete:
   - `crates/yaml`;
   - `serde_yaml` workspace and crate dependencies;
   - `kdl` workspace and crate dependencies;
   - the `[patch.crates-io]` entry for `kdl`;
   - obsolete `control_file` and system-model KDL modules;
   - YAML/KDL-specific error variants and CLI text.
7. Retain external-platform YAML under `.github` and any test string that is
   genuinely about a package named YAML rather than configuration syntax.

**Verify**:

```sh
rg -n 'serde_yaml|kdl::|crates/yaml|control\.kdl|stone\.yaml|system-model\.kdl' \
  Cargo.toml bin crates tests README.md Makefile
find bin crates tests -type f \( -name '*.yaml' -o -name '*.yml' -o -name '*.kdl' \) -print
env RUSTUP_TOOLCHAIN=1.93.0 cargo tree -i serde_yaml
env RUSTUP_TOOLCHAIN=1.93.0 cargo tree -i kdl
```

Expected: the searches print no configuration-format references or files; the
two `cargo tree -i` commands report that the packages are absent. `.github`
YAML remains untouched.

### Phase 9: Run complete validation and document the language contract

1. Add architecture documentation covering:
   - evaluation purity;
   - import policy;
   - resource limits;
   - DTO/schema versioning;
   - authored source versus generated lock/snapshot artifacts;
   - fingerprint/provenance rules;
   - compatibility and removal policy.
2. Add a minimal `stone.glu`, compositional recipe, repository, trigger and
   system intent example owned by this repository.
3. Document how to typecheck without building and how diagnostics appear.
4. Run the full repository-native validation lane.

**Verify**:

```sh
env RUSTUP_TOOLCHAIN=1.93.0 cargo fmt --all -- --check
env RUSTUP_TOOLCHAIN=1.93.0 cargo clippy --workspace -- --no-deps
env RUSTUP_TOOLCHAIN=1.93.0 cargo test --all
env RUSTUP_TOOLCHAIN=1.93.0 make test
git status --short
```

Expected: all available gates exit 0, only intended `os-tools` files are
modified, and no path under `../bedrock` appears in status or diff output.

## Test strategy

### Golden equivalence fixtures

During the compatibility window, keep paired YAML/KDL and Gluon fixtures only
inside tests. Decode both to the Rust domain model and assert equality. Delete
the old half when the compatibility loaders are removed; retain Gluon fixtures
as regression coverage.

Cover at least:

- a minimal recipe;
- a maximum-shape recipe using every field;
- upstream variants and lock resolution;
- macros/policy composition;
- profile and repository fragments;
- transaction and system triggers;
- a system model with repositories and package/provider selections.

### Negative security fixtures

Keep explicit programs attempting:

- host file reads/writes;
- process execution;
- environment access;
- ambient or escaped imports;
- unbounded recursion;
- excessive allocation;
- type confusion at the DTO boundary.

Every case must fail deterministically with a classified error.

### Property and stability tests

Where practical, test that:

- evaluation of identical source/input is deterministic;
- map/fragment merge order does not depend on filesystem enumeration order;
- fingerprints change if any imported source or explicit input changes;
- canonical generated locks/snapshots round-trip to the same domain value;
- user-authored source remains byte-for-byte unchanged after all CLI actions.

## Done criteria

All items must hold before declaring the migration complete:

- [x] One shared restricted Gluon evaluator is used by all consumers.
- [x] Forbidden host capabilities and ambient imports have negative tests.
- [x] Evaluations have memory, time and import-size limits.
- [x] Gluon-facing DTOs are versioned and separated from domain structs.
- [x] `stone.glu` is the canonical Boulder recipe.
- [x] Resolved sources live in a generated lock; authored recipes are not edited.
- [x] YAML macro configuration and KDL control files are gone; repository build
  policy is a typed Gluon value.
- [x] Profiles, repositories and triggers load through Gluon with visible errors.
- [x] Moss separates `/etc` system intent from generated per-state snapshots.
- [x] State sync/export/verify/activation behavior is covered without KDL.
- [x] `serde_yaml`, `kdl` and `crates/yaml` are absent from the workspace.
- [x] No OS Tools configuration fixtures remain in YAML/KDL.
- [x] GitHub-required YAML remains unchanged.
- [x] `cargo fmt --all -- --check` passes.
- [x] `cargo clippy --workspace -- --no-deps` passes without new warnings.
- [x] `cargo test --all` passes.
- [x] `make test` passes in an environment with Rust >=1.91 and `typos`.
- [x] No file under `../bedrock` was modified.

## Historical stop conditions

Stop and report rather than improvising if any of these occur:

- Completing a phase appears to require modifying `../bedrock`.
- The only practical Gluon VM construction path necessarily exposes host
  filesystem, process, network, environment or random access.
- Gluon evaluation cannot be interrupted or memory-bounded reliably.
- The chosen Gluon release does not build on the workspace MSRV or the required
  musl target.
- Marshalled records cannot express the recipe/config shapes without relying on
  unstable or undocumented VM internals.
- A proposed design requires rewriting arbitrary authored Gluon source.
- System-model migration would lose the ability to verify, export, archive or
  activate existing Moss states.
- A verification command fails twice after a reasonable correction.
- The implementation begins depending on YAML/KDL as a hidden intermediate.
- Scope expands into `.stone` format, transaction, VFS, database or unrelated
  package work.

## Historical maintenance and review notes

- Treat the embedded Gluon modules and DTO field shapes as a public versioned
  API. Review them like a file format, not ordinary internal helpers.
- Review every newly exposed native function as a capability grant.
- Fingerprint and generated-file stability are reproducibility contracts;
  changes require explicit migration tests.
- Do not retain deprecated readers indefinitely. Once external consumers have
  migrated, delete the old paths rather than keeping silent compatibility.
- The plan called for measuring Moss and Boulder binary/build impact after
  major Gluon dependency changes, especially for static musl builds.
- Bedrock migration is intentionally deferred. This plan only makes `os-tools`
  strong and stable enough for the user to migrate Bedrock afterward.
