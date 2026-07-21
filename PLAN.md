# Plan: Make Stone a Fully Declarative Package Function

> **Executor instructions:** Follow this plan in order and keep the repository
> buildable between slices. This work is limited to `os-tools`: never edit,
> generate files in, or otherwise modify `../bedrock`. Do not use Python to edit
> files. Use patch/edit tools and the repository Makefile for relevant build,
> check, lint, and test work. Commit every few cohesive changes; do not wait
> until the end of a phase. Do not push unless the user explicitly requests it.
>
> The completed Gluon-only format migration in
> [`docs/plans/gluon-migration.md`](docs/plans/gluon-migration.md) is the
> foundation for this plan. Preserve its evaluator, provenance, and complete
> YAML/KDL removal guarantees rather than reopening the format migration.

## Goal

Make a Stone recipe behave like a pure package declaration rather than a
manifest that Mason completes through hidden Rust policy.

The target pipeline is:

```text
pure Gluon package factory
    -> concrete typed PackageSpec
    -> source, policy, and dependency resolution
    -> canonical DerivationPlan
    -> one or more .stone packages
```

The `.stone` file remains the package artifact. `DerivationPlan` is Stone's
own frozen build description and reproducibility boundary.

## Non-negotiable constraints

- Gluon is the only OS Tools configuration language.
- YAML and KDL support must be completely absent from owned configuration,
  including loaders, fallback paths, compatibility shims, dual writes, and
  documentation. YAML required by external services such as GitHub is not OS
  Tools configuration.
- `../bedrock` is outside this plan and must not be modified.
- Gluon evaluation remains pure and capability-restricted: no network,
  processes, host filesystem discovery, environment reads, or wall-clock time.
- Generated locks and plans are distinct from authored package modules.
- The final architecture must have one source of truth. Versioned migration
  code may exist while a phase is being implemented, but the old recipe ABI,
  macro language, and compatibility paths must be removed before completion.
- Work lands as small, reviewable commits. Each implementation slice below
  should normally be one commit, with tests committed beside the behavior.
  Commit after every few cohesive changes rather than accumulating an entire
  phase in the working tree.
- Use the repository Makefile for build, check, lint, and test operations.
- Do not use Python to edit repository files.
- Do not change release/version metadata or the `.stone` archive format as part
  of this work.

## Current foundation

- [x] `stone.glu` is executable Gluon rather than decoded data.
- [x] Recipes can import local modules, call functions, and update immutable
  records.
- [x] The evaluator restricts capabilities and applies resource limits.
- [x] Recipe source, imports, ABI modules, and explicit inputs participate in
  evaluation fingerprints.
- [x] Sources can be represented by an authored recipe plus a generated
  `sources.lock.glu`.
- [x] YAML and KDL configuration paths have been removed from OS Tools.

The public recipe boundary is now `cast.package.v3`; the former recipe-v1
embedded module, encoder, evaluator, and fixtures have been
removed. Standard builders produce typed phase steps, and the planner can
resolve an exact package closure into `build.lock.glu`, freeze a canonical
`DerivationPlan`, and explain its derivation ID.

Package v3 and build-policy v3 make executable selection structural: every
direct command, shell interpreter, non-builtin shell program, source-preparation
command, PGO tool, and analyzer program binds one normalized guest path to an
exact provider request in `build.lock.glu`. Frozen execution also enforces the
explicit `execution.jobs` value as PID 1's inherited CPU affinity before any
build or analyzer descendant can run.

The normal build path now plans first and carries the validated
`DerivationPlan` through exact root installation, locked-source materialization,
the isolated container, phase execution, package analysis and collection,
manifest verification, artifact emission, and plan-owned cleanup. It records
the plan's derivation ID rather than synthesizing an identity from runtime
state.

Git resolution is also byte-bound rather than commit-only. Source-lock schema
v2 records the canonical normalized checkout SHA-256, derivation schema v15
includes it directly in plan identity and explanation, and frozen setup
recomputes it before execution. Authored `clone_dir` is validated and preserved
as the exact plan destination. Old source-lock schemas and digest mismatches
fail closed instead of synthesizing compatibility state.

Archive expansion is a typed built-in prepare step in derivation schema v15.
The executor accepts only plain, gzip, xz, or standard-frame zstd tar streams,
preflights their bounded entry graph before writing, extracts beneath a private
descriptor root, and publishes only when the repeated digest and second
manifest still match. Unsupported containers, unsafe links and paths, sparse
or special entries, topology collisions, and mutations fail closed without an
external unpacker.

Planning and packaging now consume `PackageSpec` and `DerivationPlan`
directly. The former `stone_recipe::RecipeSpec` semantic domain, its
conversions, and its duplicated build and output values have been deleted.
Mason retains a `recipe::Recipe` loaded-input context around the concrete
`PackageSpec`, authored path/source, source lock, evaluation provenance, and
timestamp needed before freezing. It is not a second package model, and the
frozen executor and packager consume only `DerivationPlan`.

Phase planning resolves layout, toolchain, tuning, environment, commands, and
source overlays through a finite typed build context. The legacy macro policy,
`%action`/`%(definition)` parser, and its duplicated tuning domain have been
deleted. Explicit `Shell` steps are literal data and cannot invoke hidden
expansion syntax.

Repository build policy now starts at the explicit `policy.glu` manifest. It
names ordered layers and strict `add`, `replace`, and `modify` entries; a
`modify` entry evaluates a total typed patch and validates the resulting policy
before the next entry. Manifest order, operation kind, module origin, and each
module's complete evaluation fingerprint participate in policy identity and
are reported by `cast recipe explain`.

### Deliberate limitation

- Mutable local recipe-directory inputs are rejected before freeze. Supporting
  them requires a local-source ABI which hashes their content and destination
  into the derivation rather than exposing an untracked recipe-directory mount.
  Rejecting this untracked input preserves the completed frozen-plan boundary;
  a future local-source ABI can extend it without adding a compatibility path.

## Target semantics

### Package factories

The documented authoring unit will be a pure Gluon function:

```text
PackageInputs -> PackageSpec
```

`PackageInputs` contains only explicit, immutable values such as dependency
references, platform information, selected features, and builder functions.
The package factory is called inside Gluon; Rust receives a concrete DTO, not
a VM closure.

The current ABI expresses that contract directly:

```gluon
// package.glu
let b = import! cast.package.v3
let cmake = import! cast.builders.cmake.v2

\deps ->
    let base = b.mk_package (b.meta {
        pname = "hello",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/hello",
        license = ["MIT"],
    })
    {
        sources = [
            b.source.archive
                "https://example.invalid/hello-1.0.0.tar.xz"
                "sha256-hex",
        ],
        native_build_inputs = [deps.pkgconf],
        build_inputs = [deps.zlib],
        builder = cmake.builder {
            flags = ["-DBUILD_TESTS=ON"],
            .. cmake.defaults
        },
        .. base
    }
```

The root explicitly supplies the scope:

```gluon
let make = import! "./package.glu"
let pkgs = import! "./package-set.glu"

make {
    pkgconf = pkgs.pkgconf,
    zlib = pkgs.zlib,
}
```

There is no automatic argument-name reflection in the first implementation.
Explicit records preserve Gluon's type checking and make missing dependencies
visible.

The initial split-output set is a deterministic `cast.package.v3` ABI
default. It is evaluated into the concrete `PackageSpec` and can be replaced
by a package factory; it is neither hidden Rust policy nor a repository layer.
Changing that default incompatibly requires a new package ABI version.

### Three specification layers

1. **`PackageSpec`** records authored package intent: metadata, sources,
   symbolic inputs, builder selection, hooks, outputs, and package rules.
2. **`PolicySpec`** records explicit repository policy: platforms, toolchains,
   builders, tuning, environment, analyzers, source preparation, and sandbox
   layout.
3. **`DerivationPlan`** is the fully resolved, target-specific, canonical data
   consumed by the executor.

The executor must not discover dependencies, reload policies, infer builder
tools, or mutate package outputs after `DerivationPlan` has been frozen.

### Typed package relations

Dependencies must become typed values rather than unvalidated strings:

```gluon
b.dep.package "zlib"
b.dep.binary "cmake"
b.dep.pkgconfig "openssl"
b.dep.soname "libz.so.1"
b.dep.output (b.package_ref "zlib") "dev"
```

The first implementation may lower these variants into the current provider
syntax. The final implementation must share one dependency representation
between Mason and Forge rather than maintaining separate parsers.

Inputs are separated by purpose:

- `native_build_inputs`: programs executed by the builder;
- `build_inputs`: libraries and headers used for the target output;
- `check_inputs`: dependencies needed only by checks;
- output-specific runtime dependencies.

The data model must leave room for build, host, and target platform roles
without making cross compilation implicit.

### Structured builders and hooks

Build systems become pure Gluon modules such as:

```text
cast.builders.cmake.v2
cast.builders.meson.v2
cast.builders.cargo.v2
cast.builders.autotools.v2
```

A builder returns structural data containing its tools, environment, phases,
and hooks. Mason must not learn that CMake or Ninja is required by expanding
a `%cmake` string.

Standard builders should expose typed configuration and pre/post hooks. A
deliberate `custom` or `shell` escape hatch remains available; declarative
construction requires explicit inputs and effects, not the elimination of
shell commands.

### Overrides and policy composition

Two different operations remain explicit:

1. **Package argument override:** call a package factory with a different
   dependency, feature, builder, or platform value.
2. **Typed attribute override:** transform an already produced `PackageSpec`
   through a total `PackagePatch`.

Patch operations use strict semantics:

```text
Keep
Set value
Replace values
Prepend values
Append values
```

Policy composition initially uses ordered one-way transformations:

```text
PolicySpec -> PolicySpec
```

Policy maps use explicit `add`, `replace`, and `modify` operations. Duplicate
unqualified additions are errors. Stone deliberately uses these ordered,
one-way transformations in the current ABI so package composition remains
finite and inspectable. This plan does not require recursive overlay fixed
points; future interoperability can be evaluated separately without making it
the organizing goal of Stone's package model.

### Frozen derivation plan

`DerivationPlan` must include every input that can change the build:

- schema version plus Cast implementation version and semantic fingerprint,
  including the production source tree, Rust compiler context, and effective
  native compiler, linker, archiver, flags, dependency controls, and tool
  identities;
- recipe and imported-module fingerprints;
- locked sources and source-lock digest;
- exact Forge-resolved package and output identities;
- build, host, and target platform values;
- selected policy, profile, toolchain, and builder fingerprints;
- all resolved phases, hooks, arguments, and environment values;
- networking, cache, PGO, emulation, and tuning choices;
- package outputs and path-selection rules;
- the chosen reproducible source timestamp.

Canonical encoding produces the identity:

```text
derivation_id = hash(canonical_encode(DerivationPlan))
```

Every emitted Stone and manifest records this ID. Stone payload hashes remain
output-integrity checks; they do not replace the derivation identity.

## Implementation phases

### Phase 1: Freeze the contracts

- [x] Document `PackageSpec`, `PolicySpec`, and `DerivationPlan` ownership and
  invariants in the code that will own them.
- [x] Inventory every value Mason currently adds after recipe evaluation.
- [x] Classify each value as authored intent, repository policy, resolved
  dependency, executor-only state, or forbidden ambient state.
- [x] Add regression tests proving that the same explicit inputs evaluate to
  the same result and fingerprint.
- [x] Rework or remove breakpoint source-line recovery that still assumes
  legacy YAML block syntax.

**Exit gate:** every current hidden input has an assigned destination in one
of the three specification layers.

### Phase 2: Make policy explicit

- [x] Create one Gluon policy root that explicitly declares the base policy,
  source preparation, targets, toolchains, builders, tuning, environment,
  analyzers, and sandbox layout; bind its imported modules into the root
  fingerprint.
- [x] Delete directory-enumerated macro loading and evaluate the typed
  `BuildPolicySpec` root instead.
- [x] Implement strict typed `add`, `replace`, and `modify` composition over a
  total `BuildPolicySpec` patch.
- [x] Retain and propagate policy and profile fingerprints.
- [x] Include selected target and policy inputs in evaluation provenance.
- [x] Add diagnostics showing which module introduced, replaced, or modified
  policy state. `recipe explain` emits every ordered transition and failures
  retain its policy, layer, operation, order, and module origin.

**Exit gate:** policy order is visible in Gluon, duplicate semantics are
explicit, and no policy/profile fingerprint is discarded.

### Phase 3: Introduce the versioned package-function ABI

- [x] Add the versioned `PackageSpec` ABI without changing the executor yet.
- [x] Establish `PackageInputs -> PackageSpec` as the package authoring
  convention.
- [x] Add defaults and a complete typed patch algebra covering every field.
- [x] Lower the concrete package value deterministically into the validated
  package/domain
  model.
- [x] Add imported-package, dependency-override, attribute-override, and error
  diagnostic examples.
- [x] Add an evaluation command that prints the concrete normalized
  `PackageSpec`.

**Exit gate:** real fixtures are authored as package factories and Rust only
receives concrete, validated package data.

### Phase 4: Type dependencies and outputs

- [x] Introduce `DependencySpec`, `PackageRef`, and `OutputRef` variants.
- [x] Separate native build, target build, check, and runtime relations.
- [x] Move the canonical provider parser and representation into a shared
  crate used by both Mason and Forge.
- [x] Make root and split outputs explicit in `PackageSpec`.
- [x] Detect missing references, duplicate output names, and dependency cycles
  before execution.
- [x] Remove duplicated shallow dependency-string validation in favor of the
  shared canonical relation parser.

**Exit gate:** no package relationship depends on an opaque, independently
validated recipe string.

### Phase 5: Add structured builders

- [x] Implement CMake, Meson, Cargo, and Autotools builder modules.
- [x] Make each builder return its required tools, environment, phases, and
  supported hooks.
- [x] Initially lower structured builders into the current shell pipeline.
- [x] Preserve an explicit custom builder for packages outside standard build
  systems.
- [x] Add equivalence tests against representative existing macro-driven
  recipes.

**Exit gate:** standard recipes do not use `%action` strings and their build
dependencies are structural.

### Phase 6: Normalize and freeze `DerivationPlan`

- [x] Resolve sources, dependencies, target, policy, builder, profile, and
  reproducibility inputs into one canonical plan.
- [x] Add generated `build.lock.glu` data for the exact package/output closure,
  base build state, repository snapshot, toolchain, target, and policy
  identities.
- [x] Wire `build.lock.glu` into Mason planning with explicit missing, stale, and
  update behavior.
- [x] Keep authored package modules separate from `sources.lock.glu` and
  `build.lock.glu`; Gluon evaluation describes requests while Rust performs and
  freezes I/O-backed resolution.
- [x] Bind every Git source to both its complete commit and a canonical
  normalized-tree SHA-256 in source-lock schema v2 and derivation schema v15;
  use one refresh/execution materializer, reject schema v1 and byte mismatches,
  and preserve validated authored `clone_dir` destinations.
- [x] Eliminate wall-clock and Git fallback; plan creation requires an
  explicitly selected timestamp and records it in the plan.
- [x] Implement stable canonical encoding and derivation hashing.
- [x] Add `cast recipe plan` and `cast recipe explain` commands.
- [x] Add derivation-ID fields to JSON manifests, binary manifest metadata, and
  Stone metadata, and supply the validated ID during frozen-plan emission.
- [x] Change the build executor to consume only the frozen plan. Normal builds
  require explicit target and source timestamp inputs, require or update
  `build.lock.glu`, exact-install its package closure, materialize only locked
  sources, execute with `exec_frozen`, package through `FrozenPackager`, verify
  manifests on the host, and clean only plan-owned paths.
- [x] Prove that changing any source, dependency, target, policy, builder,
  phase, environment, output, or timestamp changes the derivation ID.

**Exit gate:** after plan creation, Mason performs execution but no semantic
composition.

### Phase 7: Package scopes and controlled policy layers

Scopes are ordinary, nonrecursive imported Gluon records passed to factories:
missing fields are Gluon type errors, local output cycles fail before planning,
and Forge closure cycles report their exact dependency path. No hidden recursive
scope graph or Rust `PackageSet` ABI is implied.

- [x] Add explicit reusable dependency scopes backed by Forge provider
  resolution.
- [x] Support ordinary Gluon package-argument overrides.
- [x] Support typed whole-package patches analogous to attribute overrides.
- [x] Allow configured, ordered policy layers only when they are visible in
  `recipe explain` and included in the derivation identity.
- [x] Detect missing scope entries and cycles with actionable diagnostics.

**Exit gate:** packages are reusable functions without creating a second
recursive package universe inside Gluon.

### Phase 8: Retire the transitional model

- [x] Replace phase strings with typed `StepSpec` sequences for standard
  builders where structural steps are possible.
- [x] Remove `%action` and `%(definition)` parsing after golden parity tests;
  explicit `Shell` steps remain literal.
- [x] Remove filesystem-discovered macro composition.
- [x] Remove the retired recipe-v1 ABI, its standalone encoders and
  evaluator, and migrate all tracked recipes and fixtures to package v3.
- [x] Remove legacy `stone_recipe::RecipeSpec` and its lowering/conversions;
  retain only Mason's loaded-input context needed before the executor and
  packaging path consume `DerivationPlan`.
- [x] Remove the obsolete macro defaults, domain conversions, generic
  `KeyValue`, and duplicated Rust/Gluon macro wire definitions.
- [x] Audit the repository for YAML/KDL loaders, fallbacks, compatibility
  paths, examples, and documentation. The only owned YAML files are the
  external GitHub interfaces under `.github/`; negative tests and historical
  migration documentation are intentional text-only references. The Makefile
  `config-formats` gate rejects any tracked YAML/KDL path outside the exact
  external-service allowlist.
- [x] Update the Gluon configuration contract and package-authoring guide.

**Exit gate:** only the package-function ABI, explicit Gluon policy, and frozen
plan model remain.

### Phase 9: Harden the complete declaration boundary

The declarative architecture is not considered robust merely because Gluon
evaluation is pure. Every byte, collection, subprocess, filesystem walk, and
archive expansion between authored source and the published Stone bundle must
also be finite and fail closed. Limits are operational safety ceilings, not
hidden package semantics, and boundary tests must admit exactly `N` while
rejecting `N + 1`.

- [x] Bound root source, imported modules, the complete import graph, VM memory,
  stack, evaluation time, and host conversion of the evaluated package value.
- [x] Anchor recipe and imported-module reads to a trusted descriptor; reject
  traversal, symlink components, root replacement, non-regular files, and
  blocking special files.
- [x] Bound generated locks, frozen plan jobs, phases, steps, arguments,
  environment, paths, individual strings, aggregate process data, and the
  final `execve` footprint.
- [x] Require secure source transports, bounded downloads and metadata,
  authoritative streamed byte limits, pre-publication hashes, private staging,
  and atomic cache publication.
- [x] Bound analyzer duration and output, and kill the full analyzer process
  group before joining its output readers.
- [x] Bound every external frozen build command by wall time, independent and
  combined output budgets, fixed-size drains, child-local descriptor/core
  limits, and complete descendant cleanup.
- [x] Preflight and extract locked source archives structurally with exact
  compressed, decoded, entry, path, depth, file, aggregate, and wall-time
  limits; reject traversal, unsafe links, sparse/special entries, topology
  collisions, mutation, and unsupported compression or containers before
  publication.
- [x] Enforce aggregate per-derivation PID, memory, swap, and CPU ceilings in a
  delegated cgroup v2 boundary, plus byte and inode ceilings for every writable
  scratch filesystem. Rootless hosts without the required delegation or quota
  backend must fail before execution rather than silently weakening policy.
  Mason authenticates the delegated subtree and exact controller readback
  before payload entry; its private `/tmp` and Forge transaction-trigger
  `/tmp` mounts have exact size/inode readback on both activation paths, while
  the setup-only minimal `/dev` is recursively sealed read-only.
- [x] Apply the same finite process, output, progress-record, repository-size,
  and repository-entry policy to Git mirrors, fetches, and checkouts.
- [x] Load, save, and delete Gluon configuration fragments through
  descriptor-anchored, size/count-bounded, race-resistant operations.
- [x] Bound build-policy collections and recursive `TextSpec` expansion before
  and after every policy patch.
- [x] Bound Stone payload counts, records, declared and expanded sizes,
  malformed lengths, content streaming, and aggregate archive consumption.
- [x] Replace recursive/unbounded package collection with a bounded,
  descriptor-safe, deterministic walk and verified content reads. Frozen-root
  materialization now preflights a 32-GiB aggregate independent-copy budget,
  charging duplicate assets once per output inode and pinning each admitted
  length through copy. Its exact VFS inventory is then proven and normalized
  in two descriptor-rooted passes with raw-byte ordering, inode/depth limits,
  same-owner/device and single-link checks, POSIX access/default-ACL rejection,
  symlink-target and regular-content verification, mode-zero support, stable
  timestamps, and bottom-up final witnesses. Retained-capability helpers cap
  interrupted-syscall retries, while normalization-local retries recheck the
  materialization deadline. Private stage wrappers are now kernel-random 0700
  directories created beneath a retained destination-parent descriptor. A
  finite advisory parent lock serializes cooperating Forge writers, and
  publication uses descriptor-relative no-replace rename, pre/post durability
  barriers, and exact reconciliation of both names after every syscall result,
  including an error reported after the move applied. Public discard now takes
  the same retained-parent lock, admits only a same-owner directory on that
  filesystem, detaches it durably into a retained random 0700 quarantine, and
  reconciles both names before destructive work. A mode-zero root is widened
  through its pinned descriptor immediately before the cross-parent rename and
  private traversal. Every observed in-process detach failure attempts,
  durability-syncs, and revalidates an exact mode restore of that retained
  inode; a failed restore is returned as a structured dual error rather than
  hidden.
  Recursive removal opens every child beneath retained descriptors without
  symlink, magic-link, or mount traversal, enforces the same inode/depth/time
  bounds, and reconciles error-after-applied or interrupted unlinks before any
  retry. Internal failure cleanup can recurse only when the wrapper name still
  identifies the retained root; foreign source, destination, child, or wrapper
  substitutions are preserved. Production
  materialization stops ordinary work 30 seconds before its overall timeout;
  after any namespace mutation, reconciliation and provisional-wrapper cleanup
  receive that fresh, separately bounded recovery budget instead of reusing an
  already-expired work deadline. Linux cannot
  make rename or unlink conditional on an earlier inode observation, so the
  final-component guarantee deliberately remains the private-stage and
  cooperating-writer boundary rather than a claim of safety against an
  uncooperative same-EUID process. The materialization deadline likewise
  remains cooperative around individual syscalls rather than a claim that an
  arbitrary blocking filesystem can be preempted in-process.
  Crash-reopen discovery and reclamation of a durably detached random discard
  quarantine, including interruption between public mode widening and
  rename-or-restore, remains part of Phase 11; this phase claims bounded
  in-process preservation, not journal-backed reboot recovery.

**Exit gate:** malformed, oversized, changing, blocking, or resource-exhausting
inputs are rejected with structured diagnostics; no error path leaves a child,
partial cache, staging object, or ambiguous fallback eligible for reuse.
Adjacent hardening makes self-upgrade and malformed requests fail before mutation, bounds canonical state IDs before installation open, and makes drafting consume admitted manifests, safe cache copies, bounded analyzers, typed builders, and atomic no-clobber Gluon publication.

### Phase 10: Prove representative package declarations

The matrix inventory, frozen closure, live-run history, evidence rules, and
current blocker are indexed by the
[delegated fixture execution subplan](docs/plans/delegated-fixture-execution.md).
Offline source, lock, and planning proofs are not contentful execution proof;
the live items below remain open until a non-skipped required run proves them.

- [x] Maintain a checked corpus covering CMake, Meson, Cargo, Autotools,
  custom steps, hooks, feature functions, argument and attribute overrides,
  typed dependency roles, multiple sources, split outputs, conflicts, tuning,
  profiles, a source-less meta-package, and a larger daemon.
- [x] Discover every checked-in example, require one non-symlink `stone.glu`
  root, reject orphaned modules, and run public `cast recipe check` and
  deterministic repeated `cast recipe eval` without mutating authored files.
- [x] Freeze every example using hermetic content-addressed fixtures, write and
  reuse its exact build lock, and require identical canonical plan bytes and
  derivation IDs.
- [x] Prove that the metadata-only closure used to check and freeze documented
  examples is rejected at the exact executable boundary before container
  entry or artifact publication.
- [x] Make the complete check/evaluate/freeze/fail-closed proof a discoverable,
  zero-test-resistant `make examples` gate and document what it does and does
  not prove. Real execution remains exclusive to contentful fixture closures.
- [x] Add content-addressed, offline fixture sources with real bytes and hashes
  for Autotools, configured no-check Autotools, Cargo, feature-selected
  multi-binary Cargo, vendored Cargo, CMake, custom-step, header-only,
  staged-post-install, generated-daemon, pre-setup-hook, Meson, mixed archive/exact-Git/raw, external-test-vectors, explicit plugin-output, split-output, gettext-localization, declarative system-integration, desktop-integration, font-family, vendored Go-module, offline PEP 517 Python-module, dependency-relation-policy, and genuine LLVM PGO builds.
  Seed them through a narrow verified cache-import boundary; do not weaken the
  production HTTPS source policy or expose the mutable recipe directory.
- [x] Add source-less generated-configuration and generated-shell fixtures whose
  Gluon scripts author exact payloads using only frozen `bash` and `install`.
  Give the installed script an explicit Bash runtime relation and test it
  without `RunBuilt`; admit no source lock, archive, network, or recipe mount.
- [x] Add a source-less userspace-profile declaration which composes shell,
  core-command, discovery, trust-store, and archive roles as pure Gluon
  functions over one explicit package set. Its dedicated Make gate runs the
  public checker and two byte-identical evaluations, proves five empty phases
  and five exact runtime relations, and does not pretend that evaluation built
  a Stone archive.
- [x] Admit that userspace profile to the contentful delegated fixture matrix.
  The offline gate resolves its exact five direct package identities and
  separately pinned 70-package transitive closure from the immutable Stone
  index, freezes an empty-phase execution topology, and installs strict bundle,
  manifest, relation, and reproduction expectations without adding a source
  lock or host tool.
- [ ] On a supported delegated host, emit and decode its empty-root `out`
  Stone, prove its exact five-package runtime relation set, and attach the
  byte-identical locked replan/rebuild evidence. A capability skip is not
  execution proof.
- [x] Maintain a pinned, contentful Stone bootstrap closure for every real
  execution fixture containing its declared tools and runtime dependencies.
  Test-only command shims, undeclared host tools, and a mounted host or Nix
  store do not count as frozen execution. The offline fixture lane verifies
  each of the twenty-eight exact closure declarations and their exact 179-package,
  388,713,448-byte aggregate bootstrap pool before the delegated runner materializes
  the production-format root.
- [x] Before entering the container, require every frozen executable binding's
  entry point to belong to its declared provider and resolve to a regular
  executable through uniquely owned symlink hops inside the exact frozen
  closure. Missing or ambiguous handoffs and provider metadata without its
  promised entry point fail closed.
- [ ] Actually configure, compile, check, install, analyze, package, and publish
  at least one hermetic fixture for each standard builder: CMake, Meson, Cargo,
  and Autotools. Also execute one honest custom-step fixture and one native
  split-output fixture containing an executable, shared library, development
  files, pkg-config metadata, documentation, and a man page. Also execute both
  source-less generated payloads and the explicit plugin-output relation; install
  their exact bytes without a hidden source or link-time dependency.
- [ ] Decode each emitted fixture bundle and prove the expected metadata,
  layout, index, content, output relations, modes, and manifest membership.
  Rebuild from the unchanged source and build locks and require byte-identical
  plans, derivation IDs, Stone files, and manifests before accepting reuse.
- [x] Add a required-capability Make lane for CI where unavailable namespace or
  mount support is a failure, not a skip. The ordinary developer lane may report a
  narrowly classified capability skip, but never as execution success or over a
  payload failure. `make fixtures-ci` selects all fixtures with `REQUIRE_EXECUTION=1`;
  its harness-free runner creates an authenticated bounded-lifetime delegated unit.
  CI first runs `make delegated-execution-preflight` through that production boundary,
  before restoring the bootstrap cache. Live execution, decoding, and repetition are
  implemented. Only a complete matrix from the exact accepted commit may publish its
  v2 receipt: 56 executions, 84 bundle validations, 134 Stones, 56 manifests, and 190
  artifacts, with repeated plans, locks, publication outcomes, digests, and ledgers.
  Exact validation rejects drift; focused tests do not substitute for live execution.
  Accepted runtime commit `249b5c8b` sets preflight, named-fixture, `all`, and outer
  service limits to 30, 7,200, 14,400, and 21,600 seconds; `all` is capped at 18,000.
  Status is runtime plus twice kill-after, a five/sixty/ten-second client margin, and
  five seconds: defaults are 50, 7,325, 14,525, and 21,675 seconds; inner/outer caps
  are 18,665/22,215. Shorter outer limits are only fault/cleanup tests, not run proof.
  At exact commit `10d51fb9`, the disposable NixOS VM completed both runs for 19/26
  fixtures; `hooks-patch` passed twice, while `multiple-sources` passed once with nine
  Stones before its second run hit the former two-hour limit. A clean `7b3770b1` rerun
  proved the new budgets but filled the live-root tmpfs during pre-fixture compilation: zero fixtures ran, no receipt was published, and a cross-tmpfs recovery caused OOM service loss without disk modification or reboot. The three live items remain open pending a capacity-proven persistent guest.
  CI covers `develop`, matching the required untouched-`main` integration workflow.

**Exit gate:** every example is checked and frozen through public production
boundaries; all four standard builders plus the custom and split-output cases
perform real offline builds using only their frozen Stone closure; decoded
outputs and repeated bundles are byte-identical; and the required-capability
lane passes on its supported Linux CI host.

### Phase 11: Make state activation crash-recoverable

The detailed implementation evidence, completed foundations, remaining
coordinator work, and phase-specific recovery rules are indexed by
[the state-activation recovery subplan](docs/plans/state-activation-recovery.md).
That hub and its linked continuations are a required part of this canonical
plan, not optional appendices.

Phase 11 remains open. Completed foundations include canonical transition IDs, no-replace merged-/usr link publication, the bounded checksummed journal, retained tree identity and marker primitives, strict startup evidence gates, database ownership probes, and an operation-typed durable RootLinks rollback prefix through `RollbackComplete` for NewState, ActivateArchived, and ActiveReblit, plus descriptor-rooted
activation-namespace assessment. The ActiveReblit boot track now composes its exact database/Stone projection -> bounded asset plan -> sealed CAS snapshot -> Stone binding chain, exact state-root authority and schemas, and retained local command-line policy under caller-owned deadlines with terminal checks.
Its lifetime-bound render-input aggregate joins those owners to package/local command-line meaning, authenticated root intent, exact systemd-boot/kernel/initrd coordinates, and a fail-closed nonempty kernel projection while retaining the absolute deadline.
Commits `035d0843` and `04911701` normalize exact forward `UsrExchanged` startup root ABI evidence for all 32 canonical-link subsets across NewState, ActiveReblit, and ActivateArchived.
Each incomplete entry invokes the retained publisher at most once, remains at the exact source, and returns `RecoveryPending`; complete-at-entry evidence always syncs the retained root before fresh rollback-decision capture, while possibly-applied publisher errors require fresh reconciliation.
Authority binds the exact public `.cast`, journal directory, lock, and `Arc<File>`-retained record inode and inventories bounded noncanonical root-entry identities against file, symlink, and root replacement races; the canonical links remain complete throughout rollback.
The completion-fault integration proof models one startup entry for an intent source and two for an initially incomplete exchanged source; complete-at-entry exchanged evidence reaches the decision in one, with 97/97 coordinator and 19/19 normalizer tests passing.
Commit `03c5fd13` adds the independently reviewed production in-process `UsrExchanged` -> `RootLinksComplete` transition: it consumes the exact bound predecessor, publishes the retained no-replace root ABI once, revalidates every authority, conditionally advances only to the operation's exact successor, and retains that successor binding. Commit `a4f16351` wires exact durable `RootLinksComplete` + `POST` into startup for all three operations: a non-Clone predecessor-record binding is consumed to persist precisely one `RollbackDecided` with source `RootLinksComplete` and pending `/usr`; the already-complete root ABI is neither republished nor synchronized. Commit `2201a24b` then admits only that exact decision through the journal-only resume route to `ReverseExchangeIntent`. Commit `66e3cf6b` closes the remaining successor-identity window in both boundaries: same-store validation retains the non-Clone successor binding across destruction of the old lock-bearing store, and an independently opened canonical store must prove that exact successor inode and record inside a final installation-revalidation sandwich. Commit `1b34d718` carries exact non-Clone record identity through the complete reverse admission, effect, durability, and persistence chain and admits the RootLinks source for all three operations and current or historical record epochs. Its durable authority privately seals `Applied` only after one reconciled reverse exchange, or `AlreadySatisfied` after exact `PRE` evidence, so a caller cannot choose the persisted outcome. Bound publication validates the successor in the same store and again by inode and record after canonical reopen; the focused operation/epoch/outcome, five-fault, and same-byte replacement matrices never turn uncertainty into success or a second effect. Commits `fec890ad`, `c9140a88`, and `043a3c24` complete exact candidate-writer persistence for NewState, ActivateArchived, and ActiveReblit: each consumes its exact predecessor binding, derives the sole `CandidatePreserved` successor from its private effect origin, validates that successor in the same store, destroys the old lock-bearing store, and requires an independent canonical reopen to match the exact successor inode and record inside an installation-revalidation sandwich. Commit `67ad3de0` then deliberately widens only the scoped RootLinks source passage through candidate preservation. For current and historical epochs, all three operations, and both recorded `/usr` outcomes, fresh entries advance `RootLinksComplete` -> `RollbackDecided` -> `ReverseExchangeIntent` -> `UsrRestored` -> `CandidatePreserveIntent` -> `CandidatePreserved`, perform exactly one reverse exchange, and preserve all five canonical root-link targets and identities. The route proof rejects 360 root-link mutation races split across its two revalidation seams, and candidate admission separately rejects 360 races spanning all five links. Common binding totals are 64 pre-effect, 64 post-effect, and 24 preparation-restart cases. NewState and ActivateArchived writer totals are 24 success, 120 storage-fault, 96 binding-substitution, and 48 fresh-restart executions; ActiveReblit totals are 32, 160, 128, and 64 respectively. Accepted commit `e35a2183` advances only exact RootLinks-sourced NewState `CandidatePreserved` generation 15 to `FreshDbInvalidationIntent` generation 16 through the exact non-Clone record-inode binding, bound advance, same-store successor validation, old-store destruction, and independent canonical reopen. Its proof covers 24 successes, 120 storage faults, 96 binding substitutions, 48 fresh-handle reopens, and 240 all-five-root-ABI races across two seams. Accepted commit `a3fb25d3` independently advances exact RootLinks-sourced ActivateArchived `CandidatePreserved` generation 11 to `RollbackComplete` generation 12 while retaining its exact record-inode binding from capture through bound advance, same-store successor validation, and independent canonical reopen. Its proof covers 24 successes, 120 storage faults, 96 binding substitutions, 48 fresh-handle reopens, and 360 all-five-root-ABI mutation cases across capture, fresh-namespace, and final-revalidation seams. Database state, archived-wrapper and state-slot identities, and all five root-link identities remain unchanged; this journal-only entry performs no database or non-journal effect, cleanup, finalization, or boot action. A later archived entry leaves generation 12 byte-stable because RootLinks terminal finalization remains closed. At that checkpoint, NewState remained at generation 16 and ActiveReblit at `CandidatePreserved` generation 13. Its fresh-handle reopen proof was not process-death evidence, and RootLinks process death, terminal finalization, boot repair, cleanup, reboot, and power-loss durability remained unclaimed.
Separate foundations provide explicit Gluon ESP/XBOOTLDR intent and retained same-deadline mountinfo/sysfs topology evidence. Commit `13a8d928` admits exactly `vfat`, requires per-mount `rw,nosuid,nodev,noexec,nosymfollow` plus superblock `rw`, retains the closed policy facts, and rejects drift across repeated observations. Commit `b8acd3d4` authenticates one borrowed destination directory through a bounded `fstat`/`fstatfs` sandwich as the Linux MSDOS filesystem family without claiming that magic alone is exact `vfat`; commit `029f0590` keeps that descriptor private inside the retained attachment, and commit `a93efe70` composes both filesystem claims in every topology pass. Commits `9c688dc6` through `28c4735b` add bounded kernel `DEVNAME` and fixed-512-sector geometry evidence to retained sysfs snapshots; commit `24d82abf` seals those parent-disk expectations to one freshly revalidated sysfs view. Commit `78b87df9` admits only an exact `/dev` root attachment reported as `devtmpfs`, while commit `dceab6cc` supplies stable same-mount `TMPFS_MAGIC` descriptor evidence without mistaking shared tmpfs magic for exact `/dev` authority. Commit `bfa3a0c2` now binds the exact retained `/dev` attachment to that devtmpfs evidence, opens the sealed parent `DEVNAME` below the same private destination, and owns the opening-preflight -> GPT-pass-one -> private name rebind -> exact inter-pass observation -> GPT-pass-two -> closing observation -> reconciliation schedule. Commits `5ed70923`, `c2539d7f`, `215b9032`, `2eeaa22c`, `f8a5da34`, and `1f9d578a` provide the strict two-pass GPT profile, retained geometry and image bounds, pure reconciliation, positional block reads, and closed live read-provenance evidence beneath that composition. Commit `365e0ae5` completes the bounded production retained-destination observer behind the pure `Absent`/`Exact`/`Different` classifier; commit `8620986a` retains its exact observed-root device, inode, and mount ID; and commit `3f8309b1` sandwiches namespace assessment through the same private destination `File` between opening and closing boot-filesystem authentication before requiring that root triple to match. These are bounded closed observations, not whole-root non-bind provenance or ongoing-currentness claims; same-thread `setns` still requires outer aggregate revalidation, and Linux MSDOS magic still does not prove exact `vfat`. Commit `97fb33b3` closes the bounded expected-source bridge by streaming generated slices and sealed descriptors without materializing the roughly 10-GiB publication ceiling; commit `576982fe` binds those sources and canonical requests into the retained alias or distinct collision domains without copying bytes, duplicating descriptors, rediscovering topology, or granting mutation authority. Commit `5673c586` checksum-addresses immutable payloads by exact XXH3 digest and length so retained rollback entries cannot silently resolve to later bytes at a reused version path; XXH3 remains a content checksum, not cryptographic authentication. Commit `dfa247d5` separately retains typed exact-byte SHA-256 identities through the existing bounded snapshot copy, renderer deduplication, publication plan, and final source binding without changing the XXH3 path grammar or claiming cryptographic destination verification. Commit `738ebd06` projects the bound plan into a bounded canonical desired-publication inventory whose SHA-256 binds every destination-visible scalar while excluding binding indices, descriptors, source kind, and all ownership authority. The next client blocker is separately authenticated ownership provenance: decoded, self-consistent, borrowed, or first-adoption records must never grant deletion authority. Write authority, real publication ordering, durability, device flushes, restart reconciliation, and disposable-VM evidence remain open. The `nosymfollow` requirement makes future boot-publication admission effectively Linux 5.10 or newer while generic `linux_fs` facilities retain the Linux 5.6 baseline. Commit `aa341706` completes the bounded deterministic BLS renderer and binds its exact authenticated inputs and sealed source catalog to the topology-aware pure publication plan without destination or mutation authority; the durable publisher and real ESP/BOOT publication remain open.
Accepted commit `a05997d8`, with acceptance-gate follow-up `cfb5a70d`, independently admits exact RootLinks ActiveReblit generation-13 `CandidatePreserved` and carries its exact record-inode binding through capture, one bound advance to generation-14 `RollbackComplete`, same-store successor validation, and independent canonical reopen. Its proof covers 24 successes, 120 storage faults, 96 predecessor or successor binding substitutions, and 48 fresh-handle reopens; these are not process-death cases. Another 240 cases mutate all five root-ABI links, split exactly into 120 `CaptureSandwich` and 120 `FinalRevalidation` cases; the legacy fresh-namespace-capture race remains separate. The complete RootLinks endpoint performs exactly one reverse `/usr` exchange and one ActiveReblit wrapper exchange. It invokes no boot, database, non-journal namespace, finalization, or cleanup effect. `BootSyncStarted` remains disjoint and routes to `BootRepairRequired`. Accepted commit `7457b259` advances exact RootLinks NewState generation-16 `FreshDbInvalidationIntent` to generation-17 `FreshDbInvalidated` through exact capture, effect, bound-advance, same-store, and independent-reopen binding. Present evidence performs at most one exact removal and proved joint absence zero; the success/storage/binding/fresh-handle matrices cover 48/240/192/96 executions. All-five-link races cover 240 capture, 240 pre-effect, 120 Applied post-attempt, 240 initial-persistence, and 240 final-revalidation executions. Accepted commit `68759ba3` adds the RootLinks-only genuine same-boot process-death proof for that exact NewState boundary: 20 cases = two record epochs x five real SQLite application-transaction seams plus five journal-update durability seams. After the parent releases every installation, journal, and database handle, each crash child re-executes production `CleanSystemStartup` and must self-terminate by `SIGKILL`; a 15-second supervisor deadline kills and reaps only a hung child. The recovery child is the first SQLite opener and re-enters the same production path. The selected fresh row is nonempty. SQLite rolls back the first four database seams, so recovery performs one exact `Applied` removal; the post-commit and journal seams perform zero removals and converge through exact `AlreadySatisfied` or already-published successor evidence. Post-crash raw temporary-file inventory precedes any recovery journal-store or SQLite open, all five root-link identities remain exact, and unrelated namespace, exchange, and boot effects stay zero. Accepted commit `f2b305d4` then admits only exact RootLinks NewState generation-17 `FreshDbInvalidated`, captures its non-Clone record-inode binding, consumes it through one bound advance to generation-18 `RollbackComplete`, validates the successor in the same store, drops the old lock-bearing store, and requires an independent canonical reopen to match the same successor inode and record. Its base-success/storage-fault/binding-substitution/fresh-handle matrices cover 48/240/192/96 executions; fresh-handle reopen is not process death. Another 480 cases preserve all five root-ABI identities across 240 capture and 240 final-revalidation races. This boundary is journal-only: it invokes no database, non-journal namespace, reverse-exchange, candidate, boot, cleanup, terminal-delete, or finalization effect. NewState now stays byte-stable at generation 18, ActivateArchived at generation 12, and ActiveReblit at generation 14 because RootLinks terminal finalization remains closed. The existing 20-case `SIGKILL` proof remains exclusively generation 16 -> 17; a recovery entry whose invalidation successor was already canonical may naturally take generation 17 -> 18, but this adds no completion-boundary process-death claim. The next blocker is an exact record-bound terminal-deletion primitive before any operation's RootLinks finalization is widened.
The production startup ladder handles one freshly observed checkpoint per entry, not a recovery loop. Separate entries normalize exchange durability, persist and route rollback, reverse `/usr`, and persist `UsrRestored`; RootLinks-sourced recovery now also preserves every operation's candidate. NewState advances exact generation 15 through `FreshDbInvalidationIntent` generation 16 and `FreshDbInvalidated` generation 17 to `RollbackComplete` generation 18; ActivateArchived advances generation 11 to `RollbackComplete` generation 12; ActiveReblit advances generation 13 to `RollbackComplete` generation 14. The endpoint performs one reverse exchange and preserves all five root-link identities; all three records stop because RootLinks terminal finalization remains closed.
The pre-existing later source set retains its operation-specific suffixes: NewState invalidates the exact fresh row, reaches `RollbackComplete`, and finalizes to authenticated
journal absence; ActivateArchived reaches `RollbackComplete` and finalizes through separate bounded entries; ActiveReblit with no required boot repair advances from `CandidatePreserved` to `RollbackComplete`, then
finalizes separately. A `BootSyncStarted` rollback instead routes `CandidatePreserved` to `BootRepairRequired`; no boot
effect is wired, and a fresh startup observing `BootRepairStarted` invokes boot zero times and retains terminal
`BootRepairUnverified` evidence. Commit `406cabe5` adds the versioned, explicit
`BootRepairComplete` success vocabulary. Commit `ffc32ce1` production-routes an
already durable successful repair record to `RollbackComplete`, but no
production path performs the repair or emits that successful record. Commit
`19f60c51` adds the 28-case NewState candidate-move matrix.
ActiveReblit retains its 12-case terminal and 32-case wrapper-exchange matrices; ActivateArchived retains its 12-case terminal and 28-case preservation matrices.
Every process-death matrix remains same-boot `SIGKILL` evidence, not reboot or power-loss proof.

Commit `7e0618dc` adds the next candidate-preservation foundation, which at
that historical checkpoint was not yet on the production ladder. A sealed,
distinguishes exact staged evidence from an already-preserved crash prefix
across all three operations, both rollback sources, both recorded `/usr`
outcomes, and both layouts. Commit `d3bf0cd8` consumes only the admitted
NewState staged-plus-empty-quarantine typestate through a second sealed,
test-only checkpoint. It pre-syncs the exact staged candidate, issues at most
one no-replace move into the already-existing empty journal quarantine, treats
the raw syscall report as diagnostic only, and uses fresh namespace evidence
to classify `Applied`, `NotApplied`, or `Ambiguous`.

Commit `c998ad82` closes the stale non-namespace-evidence window around that
checkpoint. Namespace preparation now performs candidate sync and final PRE
capture without moving the candidate; the effect then repeats the open-journal
binding check first and revalidates journal, database, installation, and plan
evidence before consuming the opaque prepared move authority. Database or
journal changes during preparation therefore fail before the single move
attempt, while the trailing evidence observation remains in place after an
attempt or preparation failure. Commit `3da2b3d5` additionally requires every
existing NewState quarantine target to have permissions exactly `0700` in
staged-empty admission, already-preserved admission, move projection, and the
final PRE check. All fifteen otherwise-controlled non-`0700` modes are refused
for both layouts, and a final-PRE change to `0755` prevents any move attempt.
POSIX access or default ACLs on these wrappers fail closed through namespace
capture; arbitrary wrapper xattrs are not inspected and are not claimed absent.
Commit `4f9e79cd` adds a raw one-shot descriptor-relative directory-creation
adapter with no retry, adoption, or reconciliation policy. It has no production
caller. Commit `fe880cde` then models all three NewState target prefixes without
mutating them: absent, an owned restrictive-mode residue left by interrupted
preparation, and an exact empty private target ready for movement. Residues are
retained as opaque identities with unknown contents and ACL state; they are
never represented as inspected empty wrappers. Unsafe target types and modes
remain deferred.

Commit `c1418ad0` consumes those exact read-only prefixes into three disjoint,
opaque, test-sealed capabilities: create an absent target, normalize a retained
residue, or move into an already canonical target. At that checkpoint Create
and Normalize exposed no operational API, while Move retained the previously
sealed one-shot operation. Binding-first full revalidation occurs before any
capability is selected, and archived activation or ActiveReblit still receives
only a fieldless unsupported result.

Commit `5ce3c2c9` consumes only the absent-target Create lease through an
undispatched, test-sealed, one-attempt reconciliation boundary. Consumption
checks the open-journal binding first, repeats the retained installation,
database, journal, and plan evidence around a final exact absent-target PRE,
and then attempts one descriptor-relative creation under the retained
quarantine parent with the exact journal name and requested mode `0700`. It
does not sync or move the candidate, retry, adopt an entry, normalize a
residue, or continue into another effect.

The raw creation report is diagnostic only. A fresh full namespace capture
classifies an unchanged exact fingerprint as `NotApplied`, a stable transition
to the exact restrictive residue or canonical empty private target as
`RestartRequired`, and every other result as `Ambiguous`. `RestartRequired`
describes a safe observed crash prefix, not proof that this invocation created
it. Every result is fieldless and consumes all retry, normalization, and move
authority, so even the safe prefix requires a fresh startup entry. Canonical
targets with access or default ACLs fail closed, restrictive residues retain
opaque payload and ACL state, and arbitrary user xattrs remain uninspected and
unclaimed. At that checkpoint the admission inventory remained 24/24,
target-prefix preparation passed 3/3, creation passed 11/11, the combined
authority run passed 38/38, and move reconciliation remained 10/10.

Commit `7bd1e640` separately consumes only the restrictive-residue Normalize
lease. After binding-first non-namespace checks and a final exact residue PRE,
it makes one descriptor-bound mode-normalization attempt against the retained
target inode. The raw result is diagnostic only. Fresh semantic evidence
classifies an unchanged exact fingerprint as `NotApplied`, the same-inode
transition to an exact empty private target as the only canonical prefix, and
every other observation as `Ambiguous`. Payload and ACL state remain opaque
until that fresh inspection, and arbitrary user xattrs remain uninspected and
unclaimed.

Commit `36fea65f` keeps that canonical prefix private until it completes
ordered durability against the exact retained target and then the retained
quarantine parent, revalidating the public name and identity around both
barriers. One final fresh canonical capture is required before the authority
may return `RestartRequired`. `RestartRequired`, `NotApplied`, and `Ambiguous`
are all fieldless; no result carries a descriptor, retry, move, or partial
durability capability. Every outcome therefore ends the startup entry, and
normalization can never fall through into candidate movement or persistence.

At that checkpoint target-prefix preparation remained sealed and undispatched:
it supplied no production candidate-preservation executor, journal or database
mutation, post-move durability, or effect for ActivateArchived or ActiveReblit.
The normalization lane passed 12/12, the complete target-prefix aggregate
passed 26/26, the combined authority run passed 50/50, and move reconciliation
remained 10/10. The preparation and creation lanes remained 3/3 and 11/11
respectively.

Commit `0d93f979` strengthens every freshly selected Move lease independently.
It repeats the candidate-tree barrier, then synchronizes the exact canonical
target and retained quarantine parent in that order. Complete retained,
public-name, and full PRE evidence is revalidated around those barriers before
one fresh final PRE capture. The enclosing authority then repeats the
open-journal binding first and the full non-namespace evidence check; a final
exact pre-move revalidation is still required before at most one no-replace move.
The raw syscall helper is structurally private to that target-durable typestate,
so no sibling path can bypass the barriers or their final checks.

At that checkpoint the focused move lane passed 14/14, the target-prefix
aggregate remained 26/26 (3/3 preparation, 11/11 creation, and 12/12
normalization), and the combined authority run remained 50/50. `make check`
passed with only the four established warnings, and `make source-loc` reported
all 1058 tracked text files at no more than 1000 lines. It remained test-sealed
and supplied no production dispatch, persistence, or post-move durability.

Commit `a84d0f47` implements that indivisible post-move durability checkpoint
behind a distinct test-only seal. Newly `Applied` movement and independently
admitted exact NewState Finish evidence converge to one consuming suffix while
retaining fixed internal `Applied` and `AlreadySatisfied` provenance. The order
is exact: candidate tree, empty staging wrapper, journal target wrapper,
quarantine parent, then one final fresh exact POST capture. Complete retained-
descriptor and public-name identity checks surround every physical barrier.

Both origins start binding-first, repeat full pre-effect evidence, and finish
with a trailing binding-first full non-namespace gate. A partial physical
prefix returns no authority; a fresh exact Finish admission must rerun the
entire idempotent suffix. Archived and ActiveReblit Finish evidence still
selects only fieldless `Unsupported`.

The dedicated durability lane passes 6/6, the combined authority run passes
56/56, and the existing move lane remains 14/14. `make check` passes with only
the four established warnings, `make source-loc` reports all 1063 tracked text
files at no more than 1000 lines, and independent review found no issue. There
is still no production caller or dispatcher, persistence, database mutation,
trigger, cleanup, or power-loss claim.

Commit `269aae2c` adds the next test-sealed persistence checkpoint. The sealed
candidate-preservation authority derives its fixed outcome from its internal
origin, passes complete authority revalidation twice, and permits exactly one
journal advance from `CandidatePreserveIntent` to `CandidatePreserved`.
Reopening the canonical journal then has to classify the exact source or exact
successor; no other record is accepted. The persistence-specific authority is
functionally split from the established post-move durability boundary, so the
older durability gate remains intact rather than being widened for journal
persistence.

This checkpoint leaves the fresh database row and its provenance untouched.
After an interruption, reopening the source record reruns the idempotent
durability suffix without a second candidate move, while reopening the exact
successor skips preservation. The persistence lane passes 9/9, the established
post-move durability lane remains 6/6, and the combined authority run remains
56/56. `make fmt` and `make check` pass with only the four established warnings;
`make source-loc` reports all 1072 tracked text files at no more than 1000
lines; and independent review found no issue.

Commit `7bc33902` adds that separate routing checkpoint for exact NewState
`CandidatePreserved` evidence. It admits only a matching fresh transition row
with present matching provenance and the private preserved-candidate namespace.
Each of its two complete revalidation passes checks the open-journal binding
first, then observes the database, namespace, and database again in that exact
order. The retained authority fixes the route internally: it derives
`rollback_successor(None)` exactly once, advances the journal exactly once to
`FreshDbInvalidationIntent`, and then requires a canonical reopen to classify
only the exact source or exact successor record.

At that historical checkpoint, commit `0f041afe` kept the route test-only. Accepted commit `e35a2183` carries exact RootLinks NewState generation-15
`CandidatePreserved` through the bound record-inode advance to generation-16 `FreshDbInvalidationIntent`, same-store validation, and canonical reopen.
Accepted commit `7457b259` then retains that exact binding through capture, Apply-or-Finish effect reconciliation, a bound advance to generation-17
`FreshDbInvalidated`, same-store successor validation, and independent reopen. Present evidence performs at most one exact fresh-row removal; proved joint
absence performs zero. Success/storage-fault/binding-substitution/fresh-handle matrices cover 48/240/192/96 executions. Accepted commit `68759ba3` adds
exact RootLinks-only same-boot process-death proof: 20 cases = two epochs x (five real SQLite transaction seams + five journal-update durability seams).
After releasing all parent handles, crash and recovery children re-execute production `CleanSystemStartup` under 15-second kill-and-reap deadlines; recovery
is the first database opener. A nonempty selected row is transactionally rolled back at the first four SQLite seams and then removed once as `Applied`;
post-commit and journal paths remove zero, reconcile exact `AlreadySatisfied` or source-versus-successor journal evidence, inventory raw temporaries before any recovery store opens, preserve all five root-link identities, and perform no unrelated effects. Accepted commit `f2b305d4` separately consumes exact generation-17 `FreshDbInvalidated` through its non-Clone record-inode binding, one bound advance, same-store successor validation, old-store drop, and independent canonical reopen to generation-18 `RollbackComplete`. Its 48/240/192/96 base-success/storage/binding/fresh-handle cases and 480 all-five-root-ABI races are journal-only and do not add process-death evidence. Generation 18 remains byte-stable; the next blocker is an exact record-bound terminal-deletion primitive before RootLinks finalization. The existing 20-case `SIGKILL` claim remains only generation 16 -> 17 and is not reboot or power-loss proof.
Commit `9adc2760` keeps the inventory gates equivalent while avoiding the host argument-size limit.

Commits `20b36768` and `7af46ce9` complete Phase 11A's source-database-bound,
non-`Clone` exact fresh-transition removal substrate. One exclusive snapshot
covers state, selections, provenance, and the global in-flight invariant; one
no-retry transaction deletes the exact row set. Reconciliation preserves
invocation causality: net absence alone never proves which writer deleted it.

Commit `ab1bfd5e` adds the test-sealed Phase 11B
`FreshDbInvalidationIntent` effect authority. Exact NewState evidence retains
the journal, database, reservation, and preserved-candidate namespace through
binding-first database -> namespace -> database checks. Present may call the
substrate once; joint absence calls it zero times. Only proved `Applied` or
`AlreadySatisfied` outcomes retain a non-`Clone` persistence authority;
not-applied and ambiguous exits are fieldless. The detailed evidence and
ambient-namespace rules remain in the linked startup-reconciliation subplan.

Commit `51a4a348` completes Phase 11D by binding exact joint absence inside
database -> namespace -> database sandwiches around one successor projection,
one journal advance, and an exact reopen to either terminal route record.
Commit `a5313099` then wires all four exact NewState suffix checkpoints into production startup.
Each entry handles only its entry checkpoint, returns after one preparation,
effect, or persistence boundary, and cannot redispatch its resulting record or
mint sibling authority in safe Rust. Its 25 real-startup tests cover every
target/database matrix and all five faults at four persistence boundaries.
All adjacent and prior reverse gates, checks, the 1132-file limit, and review
are clean. At that historical point finalization was still absent.

Commit `6fc94f32` production-wires exact NewState terminal finalization as its
own bounded startup checkpoint. It retains the same locked store, authenticates
public journal identity and contents, attempts one exact delete, rechecks all
clean evidence and final absence, and returns no redispatchable record. Its 33
startup, 5 authority, 13 executor, and 5 clean-handoff contracts pass alongside
`make check` and the 1153-file limit. Commits `932ab3bb` and `0e56aff3` add
test-only delete-boundary seams and a 12-case current/historical, intent/exchanged
real-`SIGKILL` restart matrix through production startup. ActiveReblit preservation, completion, and terminal finalization are
in production with a 12-case terminal matrix. Commit `a9823307` adds 2 x 2 x 8 = 32 wrapper-exchange cases across both epochs,
`UsrExchangeIntent`/`UsrExchanged`, and eight death seams. Fresh Finish performs zero second exchange; it replays candidate,
candidate-wrapper, reservation-wrapper, roots-parent, and quarantine-parent syncs plus final POST proof, then persists exact
`CandidatePreserved` with `AlreadySatisfied`. Commits `8c22ec67` and `cbe3679a` add reviewed ActivateArchived child-move and
completion foundations; `c8c5ea41` production-wires completion, `32bf8589` adds terminal deletion plus clean handoff,
`c6362aae` adds its exact 12-case terminal matrix, and `bc6d6792` expands candidate-preservation death to 28 cases. All are
same-boot evidence, not reboot or power-loss proof. Commit `7b3770b1` captures one exact non-Clone `TransitionJournalRecordBinding` before namespace or database evidence and carries it through common NewState create/normalize/move, ActivateArchived, and ActiveReblit effect, durability, persistence-facing authority, and dispatch; this eliminates six coarse semantic loads, while preparation-only `RestartRequired` retains an opaque one-use unchanged-source authority. Its then-current identical-bytes/different-inode proof covered 44 pre-effect + 44 post-effect + 16 restart cases across current and historical epochs and both `/usr` outcomes, with `BootSyncStarted` only for ActiveReblit. Commits `fec890ad`, `c9140a88`, and `043a3c24` then close exact candidate persistence for NewState, ActivateArchived, and ActiveReblit respectively: consuming predecessor identity crosses conditional publication, same-store successor validation, old-store destruction, and an independently reopened exact successor check. Commit `67ad3de0` expands the scoped source axis through `CandidatePreserved` to RootLinks for all three operations, both record epochs, and both `/usr` outcomes. Its exact production endpoint reaches `UsrRestored` -> `CandidatePreserveIntent` -> `CandidatePreserved` with one reverse exchange and unchanged identities for all five root links. The route and admission gates reject 360 RootLinks mutation races each; route races split across two seams. Common binding totals expand to 64/64/24, while writer success/storage-fault/binding-substitution/restart totals are 24/120/96/48 for NewState and ActivateArchived and 32/160/128/64 for ActiveReblit. Commit `e35a2183` carries exact RootLinks NewState generation 15 through a non-Clone bound record-inode advance to `FreshDbInvalidationIntent` generation 16, same-store validation, old-store destruction, and independent reopen. Its 24/120/96/48 success/storage/binding/fresh-handle matrices and 240 two-seam root-ABI races fail closed. Commit `a3fb25d3` carries exact RootLinks ActivateArchived generation 11 through the exact record-inode binding, a bound advance to `RollbackComplete` generation 12, same-store validation, and independent reopen. Its 24/120/96/48 matrices and 360 three-seam all-five-root-ABI mutations fail closed while database, archived-wrapper, state-slot, and root identities remain unchanged. At that checkpoint, NewState stayed byte-stable at generation 16, ActivateArchived at generation 12 because finalization remained closed, and ActiveReblit at `CandidatePreserved` generation 13; those journal-only entries performed no database or non-journal effect, cleanup, finalization, or boot action. Their fresh-handle reopen proof was not process death. RootLinks terminal finalization, process death, roll-forward, durable boot publication and production boot-repair wiring, cleanup, reboot, and power-loss durability remained open.
Accepted commits `a05997d8` and `cfb5a70d` supersede that ActiveReblit checkpoint: exact RootLinks generation 13 now advances journal-only to generation-14 `RollbackComplete` through its bound record inode, same-store validation, and canonical reopen, then stays byte-stable because finalization remains closed. Accepted commit `7457b259` separately supersedes the NewState checkpoint: exact generation-16 `FreshDbInvalidationIntent` now reaches generation-17 `FreshDbInvalidated`, with at most one exact Present removal, zero jointly absent removals, exact binding through every authority and persistence boundary, 48/240/192/96 success/storage/binding/fresh-handle executions, and 1,080 all-five-link races split 240 capture + 240 pre-effect + 120 Applied post-attempt + 240 initial persistence + 240 final revalidation. Accepted commit `68759ba3` upgrades that RootLinks-only NewState boundary from fresh-handle evidence to exact genuine same-boot `SIGKILL` evidence: two epochs x (five SQLite transaction seams + five journal-update seams) = 20 crash/recovery cases through production `CleanSystemStartup`, bounded by 15-second child deadlines after the parent releases all handles. The recovery child is the first database opener; a nonempty selection proves real cascade deletion. The first four transaction deaths roll back and recovery applies one exact removal, while post-commit and journal deaths remove zero and converge through exact `AlreadySatisfied` or source-versus-successor records. Post-crash raw temporary inventory runs before any recovery store open, all five link identities remain exact, and unrelated effects stay zero. Accepted commit `f2b305d4` then advances exact RootLinks NewState generation 17 to generation-18 `RollbackComplete` by capturing and consuming its non-Clone record-inode binding through one bound advance, same-store successor validation, old-store drop, and independent canonical reopen. Its 48/240/192/96 base-success/storage/binding/fresh-handle executions and 480 capture/final all-five-root-ABI races are journal-only; they invoke no database, non-journal namespace, reverse, candidate, boot, cleanup, deletion, or finalization effect and do not claim process death. The original 20 `SIGKILL` cases remain generation 16 -> 17 only, even where an already-canonical invalidation successor lets the recovery entry naturally continue to generation 18. NewState generation 18, Archived generation 12, and ActiveReblit generation 14 remain stable because RootLinks terminal finalization is closed. The next blocker is an exact record-bound terminal-deletion primitive before operation finalization; `BootSyncStarted` remains on its disjoint `BootRepairRequired` route.

**Exit gate:** after a kill or power-loss-equivalent interruption at every persisted boundary, reopening Cast either completes the committed transition,
restores the exact previous `/usr` and preserves the candidate, or stops on a
structured manual-recovery record. It never starts a second transition while
the first is unresolved, never infers success from a pathname or an
out-of-epoch runtime witness alone, and never weakens atomic updates, state
separation, merged-/usr compliance, container trigger isolation, or fast
rollback.

## Validation gates

Every phase must add focused tests and finish with the relevant Make targets.
Before merging a phase:

```sh
make check
make test
```

The final architecture must demonstrate:

- identical explicit inputs produce byte-identical canonical plans and equal
  derivation IDs;
- source, dependency, target, builder, policy, profile, and environment
  changes invalidate the derivation ID;
- evaluator code cannot access the network, process environment, arbitrary
  host paths, processes, or time;
- the executor cannot add undeclared packages or policy after plan freezing;
- package overrides and policy composition have deterministic precedence;
- standard builders declare all tools structurally;
- generated plans explain the provenance of every resolved input;
- no OS Tools YAML/KDL compatibility path remains;
- existing `.stone` reading and package-management behavior remains covered by
  regression tests.

## Not objectives of this plan

Nix compatibility is deliberately undecided, not rejected. This plan neither
promises nor prohibits a future compatibility or interoperability layer. Such
work can be evaluated on its own merits after the Stone-native Gluon model is
solid; the current work is simply not organized around delivering it.

- Reimplementing the Nix language or Nix store.
- Building a lazy recursive Nixpkgs clone inside Gluon.
- Delivering translation of Nix expressions or evaluated Nix derivations into
  Gluon recipes as part of this plan. Nixpkgs is design and example inspiration
  here; a future translator or alternate frontend remains an open decision.
- Automatic `callPackage` argument-name reflection in the initial design.
- Evaluation-time fetching or import-from-derivation.
- Accepting mutable recipe-directory inputs before a content-addressed local
  source ABI exists. Frozen execution deliberately has no recipe mount and
  rejects commands which depend on `pkg/`; a future ABI must hash file type,
  mode, symlink target, content, and destination before this can change.
- Mounting the host or `/nix/store` into fixture containers, or substituting
  fake command shims for declared compilers and build systems, to make an
  execution example appear to pass.
- Unrestricted global overlays or user-home policy discovery.
- Removing Forge provider resolution in favor of a second dependency solver.
- Eliminating all shell execution.
- Changing workspace release/version metadata or the `.stone` archive format.
- Modifying or migrating `../bedrock`.

## Completion definition

This plan is complete when a Stone package is authored as a reusable pure
Gluon function, all policy and package relationships are typed and explicit, a
canonical target-specific `DerivationPlan` fully describes the build before it
runs, Mason executes only that plan, and no YAML, KDL, legacy recipe, or
macro compatibility path remains.

## Repository closure

- [ ] After every implementation and validation gate is complete, merge all
  surviving feature branches into `develop`, verify the combined tree through
  the Makefile, then delete every merged branch locally and remotely. Leave
  exactly `develop` and `main`; `main` must remain untouched throughout this
  work (no merge, rebase, reset, or direct commit).
- [x] Enforce a hard maximum of 1,000 lines for every repository-owned source,
  test, script, configuration, and documentation file before repository
  closure, regardless of whether it is fork-authored or inherited. Add a
  Makefile gate that inventories tracked files and fails above the limit; split
  every oversized file, including original AerynOS sources, into cohesive
  modules named for their actual functionality (never numbered placeholders
  such as `file_01` or `part_02`). Preserve behavior and public APIs through
  focused tests. When an inherited AerynOS file is split, retain its existing
  copyright/license header only in the original first file; do not copy that
  attribution header into the new fork-authored modules.
