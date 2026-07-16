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

### Phase 10: Prove representative package declarations

Offline source, lock, and planning proofs are not contentful execution proof.
The contentful build, decoded-bundle, reproduction, and required-capability
items below remain open until a non-skipped required-capability run provides
that evidence.

On 2026-07-14, `make examples` and `make execution-fixtures` passed for the
then-complete checked-in corpus and all twelve locked real-source fixtures. The
production-format offline bootstrap root also materialized successfully. The
ordinary delegated lane then reported its narrowly classified lack of a
reachable systemd user manager, while the same run with
`REQUIRE_EXECUTION=1` rejected that condition. This is useful fail-closed host
capability evidence, but it is not contentful execution or bundle-reproduction
evidence and therefore does not close either item below.

On 2026-07-15, the custom fixture became a real compiled C package. Its check
phase uses the typed `RunBuilt` operation for the generated native Linux ELF.
The relative authored path is frozen beneath the phase working directory and
opened without symlink or mount traversal; `execveat` receives that exact
close-on-exec inode capability without reopening its public pathname or using
procfs. Scripts remain explicit `Shell` steps, and descriptor-executed shebangs
fail closed. `make test`, `make examples`, and `make execution-fixtures` pass
with the regenerated source archive and exact bootstrap closure. The required
live container run remains blocked on this host by the
narrowly classified supplementary-group `EPERM`, so this stronger offline and
unit evidence still does not close the two required-execution items.

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
  multi-binary Cargo, vendored Cargo, CMake, custom-step, generated-daemon,
  pre-setup-hook, Meson, and split-output builds.
  Seed them through a narrow verified cache-import boundary; do not weaken the
  production HTTPS source policy or expose the mutable recipe directory.
- [x] Add a thirteenth, source-less generated-configuration fixture whose
  declarative Gluon script authors the complete payload. Require only frozen
  `bash` and `install` providers; admit no source lock, archive, network, host
  shim, or mounted recipe input.
- [x] Maintain a pinned, contentful Stone bootstrap closure for every real
  execution fixture containing its declared tools and runtime dependencies.
  Test-only command shims, undeclared host tools, and a mounted host or Nix
  store do not count as frozen execution. The offline fixture lane verifies
  each of the thirteen exact closure declarations and their aggregate bootstrap
  package pool before the delegated runner materializes the production-format
  root.
- [x] Before entering the container, require every frozen executable binding's
  entry point to belong to its declared provider and resolve to a regular
  executable through uniquely owned symlink hops inside the exact frozen
  closure. Missing or ambiguous handoffs and provider metadata without its
  promised entry point fail closed.
- [ ] Actually configure, compile, check, install, analyze, package, and publish
  at least one hermetic fixture for each standard builder: CMake, Meson, Cargo,
  and Autotools. Also execute one honest custom-step fixture and one native
  split-output fixture containing an executable, shared library, development
  files, pkg-config metadata, documentation, and a man page. Also execute the
  source-less generated-configuration fixture and install its exact authored
  bytes without introducing a source path.
- [ ] Decode each emitted fixture bundle and prove the expected metadata,
  layout, index, content, output relations, modes, and manifest membership.
  Rebuild from the unchanged source and build locks and require byte-identical
  plans, derivation IDs, Stone files, and manifests before accepting reuse.
- [x] Add a required-capability Make lane for CI where unavailable namespace or
  mount support is a failure, not a skip. The ordinary developer lane may
  report a narrowly classified capability skip, but must never report it as an
  execution success or use it to hide a payload failure. `make fixtures-ci`
  selects every fixture with `REQUIRE_EXECUTION=1`; its harness-free runner
  creates an authenticated, bounded-lifetime delegated systemd unit and CI
  establishes and preflights the required user manager first.
  The complete live execution, bundle decoding, and repeated-build assertions
  are implemented, but the two items above remain open pending one non-skipped
  `make fixtures-ci` run attached to the exact accepted commit. A local required
  custom-fixture run reached the delegated service and then failed closed at
  credential preflight because this host denies isolated `setgroups`; no build
  or publication was misreported as successful. CI now also covers `develop`,
  matching the required untouched-`main` integration workflow.

**Exit gate:** every example is checked and frozen through public production
boundaries; all four standard builders plus the custom and split-output cases
perform real offline builds using only their frozen Stone closure; decoded
outputs and repeated bundles are byte-identical; and the required-capability
lane passes on its supported Linux CI host.

### Phase 11: Make state activation crash-recoverable

The detailed implementation evidence, completed foundations, remaining
coordinator work, and phase-specific recovery rules live in
[the state-activation recovery subplan](docs/plans/state-activation-recovery.md).
That document is a required part of this canonical plan, not an optional
appendix.

Completed foundations include canonical transition IDs, no-replace merged-/usr
link publication, the bounded checksummed journal, retained tree identity and
marker primitives, strict startup evidence gates, and database ownership
probes. They now also include the persisted-phase recovery-direction classifier,
an unwired one-shot coordinator effect which reaches durable `UsrExchanged`
without retry, reversal, cleanup, or root-link publication, and a
diagnostic-only startup assessment which releases mutation authority before
returning and cannot execute stale evidence. The remaining closure is
to finish recovery-ordered mutable client construction, replace residual
path-based lifecycle authority, establish the durable pre-journal baseline,
route every state transition through one durable coordinator, execute every
persisted phase on startup, and prove convergence with deterministic
interruption coverage.

**Exit gate:** after a kill or power-loss-equivalent interruption at every
persisted boundary, reopening Cast either completes the committed transition,
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
