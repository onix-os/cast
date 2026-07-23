
# Declarative Stone contracts

This document freezes the ownership boundary and invariants for the three
specification layers introduced by the declarative Stone plan. It also retains
the Phase 1 baseline inventory of values the build frontend supplied after evaluation.
That inventory is historical; the implementation status below distinguishes
what has moved from what remains in the pre-freeze transition.

## Ownership

The declarative data contracts belong in `crates/stone_recipe`, not in the
Cast frontend. The crate is the format-neutral boundary shared by the
restricted Gluon evaluator and Mason.

| Contract | Target module | Owner and responsibility |
| --- | --- | --- |
| `PackageSpec` | `stone_recipe::package` | Authored intent returned by a pure Gluon package factory. Validation may inspect only this value and explicit function arguments. |
| `BuildPolicySpec` | `stone_recipe::build_policy` | Repository-supplied builders, platforms, toolchains, tuning, environments, analyzers, source preparation, and sandbox layout. An explicit manifest composes validated values and total patches through ordered, fingerprinted operations. |
| `DerivationPlan` | `stone_recipe::derivation` | Canonical, fully resolved build description. Its encoding and derivation ID are library behavior so the executor, tests, and inspection tools share one implementation. |

The target contract gives Mason orchestration only:

1. evaluate a package factory and an explicitly selected policy root;
2. resolve sources and package providers through I/O-backed services;
3. freeze and validate one `DerivationPlan`;
4. execute that plan and emit its declared outputs.

Mason must not own a second private representation that can express more
semantic information than `DerivationPlan`. Runtime structs may borrow or
index plan data, but they must not add dependencies, phases, environment,
policy, or outputs.

## Implementation status

Implemented:

- `cast.package.v3` is the only public recipe ABI; the retired Gluon modules,
  evaluator, encoders, and fixtures are removed.
- `stone::relation` is the shared typed relation representation used by Stone,
  Mason, Forge, and `stone_recipe` validation.
- reusable dependency scopes are ordinary imported Gluon records passed to
  factories; missing fields are type errors, local output cycles are rejected,
  and Forge closure cycles report their concrete path;
- typed build policy loads from the explicit `data/policy/policy.glu` manifest,
  which names ordered layers and strict `add`, `replace`, and `modify`
  operations; unlisted neighboring files do not participate;
- `BuildPolicyPatchSpec` covers every top-level field, distinguishes scalar
  `Keep`/`Set` from ordered-array `Keep`/`Replace`/`Prepend`/`Append`, and
  validates every intermediate result;
- CMake, Meson, Cargo, and Autotools produce structural `StepSpec` phases and
  declare their tools without authored `%action` strings;
- phase planning resolves layout, tools, tuning, environment, commands, and
  sources through a finite typed context; explicit `Shell` content is literal,
  and the legacy macro/parser/tuning stack is absent;
- `build.lock.glu` freezes the exact Forge-resolved closure, repository
  snapshots, platforms, and selected policy identities with explicit
  missing/stale/update behavior. Schema v5 also binds every canonical request
  to all typed package, output-runtime, policy, job-executable, and analyzer
  origins collected before request deduplication;
- `cast recipe plan` freezes and validates canonical target-specific jobs,
  phases, environment, layout, execution policy, analysis, collection rules,
  manifest inputs, outputs, and timestamp;
- `recipe explain` exposes each request, its exact package/output resolution,
  every input origin, each policy source, and ordered policy operation;
  canonical mutation tests prove that semantic and origin-only changes alter
  the derivation ID;
- normal `cast build` uses that same plan to verify repository snapshots,
  exact-install locked packages, materialize locked sources, enter a frozen
  container, execute plan steps, package plan-owned outputs, verify manifests
  on the host, and clean plan-owned paths;
- manifest and Stone metadata record the recipe and derivation provenance from
  the executed plan.

Deliberately unsupported:

- mutable local recipe `pkg/` inputs are rejected until a local-source ABI can
  hash their content and destination into the derivation. Supporting that ABI
  is an explicit non-goal of the current contract; frozen containers do not
  mount the recipe directory, so the limitation cannot become an ambient input.

## Layer invariants

### `PackageSpec`

- Is the concrete result of calling `PackageInputs -> PackageSpec` inside the
  restricted Gluon VM. Rust never stores or invokes a Gluon closure.
- Contains authored requests and symbolic references, never resolved package
  IDs, repository snapshots, host paths, fetched content, or current time.
- Uses typed dependency and output references through the shared Stone
  relation model. Authored packages do not carry provider strings.
- Separates native build, target build, check, and output-specific runtime
  relations.
- Declares sources, a structural builder contract, hooks, a typed network request,
  package outputs, and path rules explicitly. Standard builder modules return
  their symbolic tool capabilities, environment marker, ordered phase graph,
  and supported hook surface as ordinary package data.
- Has deterministic defaults in the versioned package ABI, including the
  initial output set. Those defaults are evaluated into the concrete
  `PackageSpec`, can be replaced by the factory, and are not policy-layer
  state. They do not depend on the host, process environment, directory
  contents, or evaluation order.
- Is validated before any source or dependency resolution begins.
- Retains the typed network request for a possible future fixed-output ABI, but
  currently rejects `options.networking = true`; frozen builds admit external
  content only through locked sources.

### `PolicySpec`

- Is reached through one explicit Gluon policy root. Directory enumeration is
  not composition.
- Contains every repository choice that can alter a build: platform data,
  toolchains, standard-builder command and environment templates, base build
  inputs, tuning defaults, source preparation, an explicitly ordered analyzer
  pipeline, analyzer executable capabilities, and fixed guest layout. It does not duplicate module-owned builder
  capabilities or phases. Analyzer kinds are unique and `IncludeAny` is the
  required final fallback.
- Is composed through ordered, one-way transformations with strict `add`,
  `replace`, and `modify` operations. `add` requires absent state; `replace`
  and `modify` require existing state, and each intermediate policy is
  validated.
- Uses a total top-level patch: scalar and structured values distinguish
  `Keep` from `Set`, while arrays distinguish `Keep`, `Replace`, `Prepend`, and
  `Append` without sorting or deduplication.
- Retains provenance and complete fingerprints for the manifest and every
  operation module and import. Manifest/layer/entry order, operation kind, and
  module origin are part of the final policy identity.
- Does not read the machine architecture, CPU count, environment, user home,
  filesystem, network, or clock while being evaluated.
- May provide defaults, but applying those defaults is part of plan
  resolution and is visible in plan provenance.
- Owns a finite sandbox-filesystem contract. Frozen builds always omit proc,
  use an empty `/tmp`, omit `/sys`, and may select absent or minimal `/dev`;
  proc, host `/sys`, and full host `/dev` are not policy values. Minimal
  `/dev` is exactly `null`, `zero`, and `full` and never varies with host
  device availability.
- Owns the sandbox credential selection and the executable capabilities used
  by analyzer handlers. Only capabilities reachable from the frozen handler,
  debug, strip, and compiler-toolchain choices become root requests.

### `DerivationPlan`

- Is concrete and target-specific. No unresolved dependency provider, source
  request, policy lookup, macro invocation, or output-template merge remains.
- Contains the selected package, source lock, exact package/output closure,
  repository snapshot, build/host/target platforms, policy, profile,
  toolchain, builder, phases, hooks, environment, network mode, explicit
  pseudo-filesystems, PGO stages, tuning, outputs, source timestamp, and
  implementation/schema versions. Git sources carry both their complete
  commit and canonical normalized-tree SHA-256.
- Records the fingerprint and provenance of every authored or policy module
  that contributed semantic data.
- Has one stable canonical encoding. Maps are key ordered, sequences preserve
  declared semantic order, optional values have one encoding, and paths and
  identifiers have one normalized representation.
- Defines `derivation_id = hash(canonical_encode(plan))`. No executor option
  that changes an output may remain outside this identity.
- Is validated before execution. Current validation covers schema versions,
  identities, safe package/version/artifact filename components, locked closure references and cycles, source order and identity,
  unique phases/outputs/analyzers, output relations, guest paths, and explicit
  concurrency, disabled networking, the schema-v13 plan and executor identity, explicit
  credentials, locked-closure root materialization, exact analyzer
  program/provider bindings, and the finite sandbox-filesystem contract. The locked
  closure path copies only exact package IDs from `build.lock.glu`, creates the
  fixed build-root ABI links, and never reads package-manager system intent,
  composes a system snapshot, resolves providers, or discovers transaction or
  system triggers. An arbitrary explicit `Shell` escape cannot be statically proven
  to use only declared tools, so every structural builder contract carries an
  explicit `required_tools` list; standard modules populate it automatically.
- Is immutable after freezing. Execution can report observations such as
  output file hashes and analyzer findings, but cannot change the requested
  build semantics.

## Classification rules

Every post-evaluation value belongs to exactly one of these classes:

- **Authored intent** moves into `PackageSpec`.
- **Repository policy** moves into `PolicySpec`.
- **Resolved dependency** is produced by resolution and frozen in
  `DerivationPlan`.
- **Executor-only state** may remain outside the plan only when changing it is
  proven not to change build or package semantics.
- **Forbidden ambient state** currently changes semantics without being an
  explicit input. It must be removed or converted into an explicit plan input.

The filesystem location used to load an explicit value is not itself semantic;
the selected identity, content, fingerprint, and ordering are semantic.

## Phase 1 baseline post-evaluation input inventory

The following tables describe the migration baseline, not the current state.
They remain the audit trail for why each value belongs in a contract. The
implementation status above is authoritative for completed work.

### Policy and package construction

| Baseline value or behavior | Baseline location | Class | Required destination |
| --- | --- | --- | --- |
| Sorted discovery of `data/macros/actions/*.glu` and `data/macros/arch/*.glu` | former build-frontend macro-loader behavior | Repository policy | One explicit `data/policy/policy.glu` manifest with ordered typed operations and complete evaluation fingerprints. Its foundation `add` names `default.glu`; the macro tree is deleted. |
| Base and target action/definition maps | `build/job/phase.rs` | Repository policy | Typed command and environment templates selected by module-owned steps and environment markers; the module-owned phase graph and capabilities are frozen with the resolved values. |
| Architecture package templates | former `package.rs::resolve_packages` behavior | Repository policy | Removed. `PackageSpec.outputs` is the sole typed output declaration and the selected target supplies only artifact architecture and build policy. |
| Root and target-profile phase fallback | `build/job/phase.rs::Phase::script` | Authored intent | Package builder/hooks plus an explicit target override; the chosen phase is frozen in the plan. |
| Root package and subpackage precedence | `package.rs::resolve_packages` | Authored intent | Explicit named outputs in `PackageSpec`; no collision-based merge in the executor. |
| Template collision merge and list sorting | former `package.rs::resolve_packages` behavior | Repository policy | Removed. Named `PackageSpec.outputs` are validated directly and copied deterministically into the plan without a policy-template merge. |
| `%name`, `%version`, and `%release` expansion in package fields | `package.rs::resolve_packages` | Authored intent | Structural fields or typed interpolation resolved before plan freeze. |
| `%action` expansion and action-provided dependencies | former `stone_recipe::script` and `build/job/phase.rs` behavior | Repository policy | Pure builder modules return typed steps and symbolic required capabilities; policy supplies typed command templates only. Explicit `Shell` is literal and performs no macro expansion. |
| `%(definition)` expansion | former `stone_recipe::script` and `build/job/phase.rs` behavior | Repository policy | Replaced by typed builder arguments, paths, and environment values frozen in the plan. |

### Platforms, toolchains, and build environment

| Baseline value or behavior | Baseline location | Class | Required destination |
| --- | --- | --- | --- |
| Host architecture used to choose build targets | `recipe.rs::build_targets` and `architecture.rs` | Forbidden ambient state | Explicit build/host/target platform input, validated against `PolicySpec` and frozen in the plan. |
| Host architecture written to package metadata | `package/emit.rs` | Forbidden ambient state | The plan's resolved target/output architecture. |
| Base root packages | `build/root.rs::BASE_PACKAGES` | Repository policy | Standard environment inputs in `PolicySpec`, resolved to exact package IDs. |
| GNU/LLVM, emul32, Mold, and compiler-cache root packages | `build/root.rs::packages` | Repository policy | Toolchain and feature policy selected by explicit package or invocation inputs, then frozen in the closure. Standard-builder capabilities come from the evaluated builder module instead. |
| Compiler executable names and linker selection | `build/job/phase.rs` | Repository policy | Selected `ToolchainSpec`/builder environment in `PolicySpec`; concrete values in the plan. |
| Guest paths such as `/mason`, build roots, install roots, and cache paths | `paths.rs`, `build.rs`, and `build/job/phase.rs` | Repository policy | A fixed builder-layout policy; all paths visible to scripts are concrete plan values. |
| Root/target environment fallback and `%scriptBase` prefix | `build/job/phase.rs` | Authored intent plus repository policy | Authored environment patch plus builder base environment, resolved with explicit precedence into the plan. |
| CPU-derived `%(jobs)` | `build/job/phase.rs` | Forbidden ambient state | `execution.jobs` is explicit plan data, visible to build scripts, and enforced as PID 1's exact inherited CPU affinity before any build step or analyzer descendant runs. |
| Aggregate process-tree resources | host scheduler and memory pressure | Executor-only safety policy | An explicitly delegated systemd cgroup-v2 root is mandatory. `clone3(CLONE_INTO_CGROUP)` atomically places the derivation in a terminal leaf with finite PID, memory, swap, and CPU ceilings; missing delegation never selects a weaker execution path. |
| Compiler-cache enablement and cache-related definitions | CLI, `build.rs`, and `build/job/phase.rs` | Authored invocation intent | Explicit plan option when visible to the build; cache storage locations remain executor-only. |
| Container networking during frozen builds | `PackageSpec.options.networking` and `container.rs` | Forbidden ambient input | Package validation rejects enabled networking. Fetched content must be declared as typed sources and locked before execution; the field is retained only for a possible future fixed-output ABI. |
| Interactive `cast chroot` shell | `cli/chroot.rs` | Explicit impure development exception | Outside frozen planning and execution guarantees. It can inspect an existing build root but never invokes, validates, or syncs package emission; files created there are not frozen artifacts. |
| Container hostname, mounts, PATH, HOME, and TERM | `container.rs` and `build.rs` | Repository policy | Semantic process environment and mounts are builder policy/plan data; frozen new-network namespaces retain the kernel-default loopback state and never invoke the optional host `ip` utility. Interactive breakpoint TERM is executor-only. |
| PGO stage sequence and LLVM merge actions | `build/pgo.rs` and `build/job/phase.rs` | Repository policy | Builder-generated structured stages in the plan. |
| Default tuning groups, stage tuning, flag deduplication, and Mold flags | `build/job/phase.rs::add_tuning` | Repository policy | Ordered tuning policy and concrete compiler/linker flags in the plan. |

### Sources and time

| Baseline value or behavior | Baseline location | Class | Required destination |
| --- | --- | --- | --- |
| Authored upstream requests | evaluated recipe | Authored intent | Typed source requests in `PackageSpec`. |
| `sources.lock.glu` content | `recipe.rs` and `source_lock.rs` | Resolved dependency | Schema v2 locks archive identities and each Git commit plus normalized-tree SHA-256; the lock and per-source identities enter `DerivationPlan`. |
| Fetched archive/git content | `upstream.rs` | Resolved dependency | Verify archives and canonical Git materializations against the source lock before execution; storage/cache paths are executor-only. |
| Generated unpack/copy prepare script and work directory | `build/job.rs` and `build/job/phase.rs` | Repository policy | Structured source-preparation steps from the builder policy, concretized in the plan. |
| Archive-extension unpacker dependencies | `build/root.rs::packages` | Repository policy | Source-preparation policy declares the tools; exact providers are in the closure. |
| `SOURCE_DATE_EPOCH` process environment | `recipe.rs::resolve_build_time` | Forbidden ambient state | An explicit timestamp input to plan creation. Evaluation never reads the process environment. |
| Git commit timestamp discovery | `recipe.rs::resolve_build_time` | Forbidden ambient state | Resolution may propose a timestamp, but the selected value and provenance must be explicit and locked. |
| `Utc::now()` fallback | `recipe.rs::resolve_build_time` | Forbidden ambient state | Remove without replacement; a plan cannot be frozen until a reproducible timestamp is selected. |

### Dependency and repository resolution

| Baseline value or behavior | Baseline location | Class | Required destination |
| --- | --- | --- | --- |
| Recipe build/check dependency strings | evaluated recipe and `build/root.rs` | Authored intent | Typed native/build/check inputs in `PackageSpec`. |
| Dependencies inferred from expanded actions | `Builder::extra_deps` | Repository policy | Builder requirements in `PolicySpec`, resolved before plan freeze. |
| Profile selection and repository list from user/system configuration | `build.rs` and `profile.rs` | Repository policy | Explicit selected profile identity and fingerprint in the plan; repository contents are resolution inputs. |
| Current repository indexes and provider choices | Forge population | Resolved dependency | Exact repository snapshot plus selected package/output IDs in the plan and `build.lock.glu`. Implemented by planning and enforced by frozen runtime setup. |
| `--update` repository refresh | `cli/build.rs` and `build/root.rs` | Executor-only operation | It may change resolution, but the flag is not identity; the resulting repository snapshot and closure are. |
| Automatic repository initialization | `build/root.rs` | Executor-only operation | Must not change a frozen plan. Resolution occurs before freezing, never during root population. |

### Outputs and invocation controls

| Baseline value or behavior | Baseline location | Class | Required destination |
| --- | --- | --- | --- |
| Output names, path rules, runtime relations, and conflicts | evaluated recipe | Authored intent | Explicit validated `PackageSpec.outputs`, resolved into frozen plan outputs. |
| Package analysis chain and automatic provides/dependencies | `package/analysis` | Repository policy | Analyzer order and executable capabilities are selected into the plan and exact lock requests. Findings are output-derived observations, not permission to select new policy or rediscover tools. |
| Build release number | `cli/build.rs` and `package/emit.rs` | Authored invocation intent | Explicit plan field because it changes emitted package identity/metadata. |
| Cast implementation and recipe fingerprint | `tools_buildinfo`, `package.rs`, and emitted metadata | Resolved dependency | Schema/implementation version plus a source-tree fingerprint bound to the Rust compiler context and effective native compiler, linker, archiver, flags, dependency controls, and tool identities; all recipe, policy, lock, and builder fingerprints are frozen separately in the plan. |
| Output directory, cleanup, progress/timing, terminal handling, process priority, and completion timestamp | CLI and `build.rs` | Executor-only state | Remain outside the plan; none may be visible to build processes or emitted package semantics. |
| Compiler/tool caches | `paths.rs`, `container.rs`, and `executor.rs` | Executor-only state | Enabled cache mounts are namespaced by derivation identity, but `Executor::run` clears all six frozen layout destinations before any build step. Cache contents can be reused only by phases within that execution. Disabled plans neither mount nor touch them, and host locations never enter the plan. |
| Verified source and Forge storage | `env.rs`, `upstream.rs`, and Forge | Executor-only state | Persistent content is admitted only through verified source locks or repository identities; storage locations never enter the plan. |
| Manifest verification path | CLI and `paths.rs` | Executor-only validation | Expected manifest identity/content is a verification request, not a derivation input. |
| Compression worker count | `package/emit.rs` | Executor-only only if proven byte-stable | If it can alter artifact bytes or metadata, make encoding policy explicit in the plan. |

## Freeze boundary

This execution contract is enforced. Planning freezes and validates the
derivation; runtime setup, container entry, phase execution, packaging,
verification, and cleanup receive plan-owned values rather than re-resolving
semantic inputs.

The only permitted transitions are:

```text
PackageSpec + PolicySpec + source/dependency resolution
    -> validate
    -> canonical DerivationPlan
    -> execute
    -> output observations
```

After the plan is frozen, the executor may choose scheduling, cache placement,
temporary host paths, logging, progress presentation, and cleanup. It may not:

- inspect an unrecorded profile, repository, policy directory, or environment
  variable;
- add root packages based on undeclared syntax, file extensions, or toolchain
  branches;
- infer a platform, timestamp, phase, PGO stage, tuning group, or compiler;
- merge hidden package templates, root packages, or subpackages;
- expose the host CPU count or host paths to build steps;
- resolve another provider because the selected package is unavailable.

If execution discovers that a required semantic value is missing, it fails and
the plan must be resolved again.

## Breakpoint locations

Standard builder modules now return their typed phase graph directly; Rust does
not synthesize the sequence. An explicit `Shell` script may still be defined by
a function, record update, or imported module, so scanning the root `stone.glu`
text cannot recover an authoritative authored source line. Cast reports the
stable, one-based line within that evaluated shell script instead.
Interactive breakpoints are rejected when freezing a derivation plan; future
structured steps may carry Gluon provenance directly, but the executor must
never guess a source location from configuration syntax.

## Phase 1 exit audit

Phase 1 tests prove that repeated evaluation with identical explicit source,
imports, ABI modules, and lock bytes produces equal values and fingerprints.
The resolver and planner freeze and validate the canonical plan, and the normal
build path enforces it through execution and emission. The internal recipe and
macro representations are gone; remaining contract work must extend the
local-source ABI and any future typed policy fields without weakening this
boundary.
