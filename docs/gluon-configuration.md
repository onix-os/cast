<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Gluon configuration

OS Tools uses [Gluon](https://gluon-lang.org/) as its only declarative
configuration language. Boulder recipes, typed build policy, profiles, Moss
repositories, triggers, and system intent all cross a typed Gluon-to-Rust
boundary. YAML and KDL are not compatibility formats and are not used as
intermediate representations.

The public language boundary is versioned independently of the Rust domain
types. Configuration is evaluated into small DTOs, then converted and
semantically validated in Rust. Values such as URLs, paths, dependency
providers, package versions, glob patterns, and repository identifiers are
constructed only during that conversion.

## Entry points

| Purpose | Authored source | Embedded ABI |
|---|---|---|
| Boulder package | `stone.glu` | `boulder.package.v3` and `boulder.builders.*.v2` |
| Boulder build policy | `bin/boulder/data/policy/policy.glu` | `boulder.build_policy.layers.v1` and `boulder.build_policy.v3` |
| Boulder profile | `profile.glu` or `profile.d/*.glu` | `boulder.profile.v1` |
| Moss repository | `repo.glu` or `repo.d/*.glu` | `moss.repository.v1` |
| Packaged Moss trigger | `/usr/share/moss/triggers/{tx.d,sys.d}/*.glu` | `moss.trigger.v1` |
| Moss system intent | `/etc/moss/system.glu` | `moss.system.v1` |

System and user fragment loading is deterministic. Vendor files under
`/usr/share/<program>` load before administrator files under `/etc/<program>`;
the user layer loads last where it applies. Files within a fragment directory
are ordered by logical name. Invalid files are errors rather than silently
ignored.

Runnable examples live in [`docs/examples/gluon`](examples/gluon):

- [`stone.glu`](examples/gluon/stone.glu) is a minimal recipe;
- [`composed-stone.glu`](examples/gluon/composed-stone.glu) applies a function
  from [`package_policy.glu`](examples/gluon/package_policy.glu);
- [`repositories.glu`](examples/gluon/repositories.glu) defines Moss
  repositories;
- [`trigger.glu`](examples/gluon/trigger.glu) defines a packaged trigger;
- [`system.glu`](examples/gluon/system.glu) defines desired system state.

The [package-authoring guide](package-authoring.md) documents factories,
explicit dependency scopes, standard and custom builders, typed phases,
outputs, patches, and lock/plan workflows.

## Restricted evaluator

`crates/gluon_config` is the single VM construction and import-policy boundary.
It starts an empty `RootedThread`, disables the implicit prelude, standard
library, and Gluon I/O execution, clears import search paths, and installs only
the modules required by the selected typed ABI. The recipe ABI explicitly opts
into Gluon's pure array and string primitives for immutable composition; those
imports are closed by default and are included in the fingerprint.

Evaluated configuration cannot read or write the host filesystem, run a
process, access the network or environment, observe a clock, use randomness,
or register arbitrary native Rust functions. Host-capability namespaces such
as `std.fs`, `std.io`, `std.process`, `std.env`, `std.random`, and related
effect, thread, channel, and reference modules are explicitly denied.

### Imports

There are two import classes:

1. Versioned in-memory modules supplied by OS Tools, such as
   `boulder.package.v3` and `moss.system.v1`.
2. Quoted relative modules beneath the explicit source root, for example
   `import! "./package-policy.glu"`.

Relative paths are canonicalized. Parent traversal, absolute paths, symlink
escapes, implicit current-directory lookup, and collisions with embedded module
names are rejected. `GLUON_PATH` is ignored. Embedded modules cannot use a
recipe's source root to import host files.

### Default resource limits

| Resource | Default |
|---|---:|
| Root source | 1 MiB |
| One imported file | 256 KiB |
| Imported modules | 64 |
| Complete source/import graph | 2 MiB |
| VM memory | 32 MiB |
| VM stack | 64 KiB |
| Wall-clock evaluation time | 2 seconds |

The limits are configurable by Rust callers, but every evaluator has bounded
defaults. A watchdog interrupts non-terminating evaluation. Source size, import
size/count, memory, stack, and timeout failures are classified limit errors,
separate from parse, type, import, I/O, conversion, and runtime failures.

Diagnostics retain the logical source name and source span when Gluon provides
one. CLI commands propagate those diagnostics rather than printing from the VM
or terminating the process.

## Typed and versioned ABIs

The shared configuration boundary is version `1`. The canonical Boulder
package ABI is version `3`; the standard-builder modules are version `2`; the
build-policy manifest remains version `1` and the build-policy value is version
`3`.
The embedded modules expose constructors, defaults, explicit option/boolean
variants, and immutable records. Gluon-facing DTOs use only stable language
shapes such as strings, integers, arrays, records, and explicit variants.

Record update syntax makes policy composition ordinary Gluon rather than a
sidecar overlay format:

```gluon
let boulder = import! boulder.package.v3
let add_runtime = import! "./package_policy.glu"

let meta = boulder.meta {
    pname = "hello",
    version = "1.0.0",
    release = 1,
    homepage = "https://example.invalid/hello",
    license = ["MPL-2.0"],
}

{
    outputs = [add_runtime (boulder.output "out")],
    .. boulder.mk_package meta
}
```

Package factories are ordinary functions from an explicit dependency record to
a concrete package value. `boulder.override_attrs` applies a total typed patch;
patch records distinguish keeping an array from replacing it with `[]`.
Standard CMake, Meson, Cargo, and Autotools modules return complete structural
builder records: symbolic required capabilities, an environment marker,
ordered `StepSpec` phases, and supported hooks. Repository policy separately
owns the typed command templates and environment bindings selected by those
values. Rust performs typed lowering only; it neither synthesizes a standard
phase graph nor supplies a second builder-tool list. Builders do not lower
through `%action` strings. Direct `Run` steps bind an absolute guest program to
its dependency capability. `Shell` binds its interpreter and every declared
program the same way; `b.step.shell` remains ergonomic shorthand for a
Gluon-constructed `/usr/bin/bash` capability and an empty declared-program
list. Shell text stays literal and cannot invoke `%action` or `%(definition)`
syntax. The executor receives only the resulting frozen `StepPlan` and
environment values.

The former `boulder.recipe.v1`, `boulder.macros.v1`, and `boulder.policy.v1`
embedded modules, evaluators, and standalone encoders have been removed.
`boulder.package.v3` is the only recipe ABI, repository build policy evaluates
directly as `BuildPolicySpec`, and Boulder plans and packages the concrete
values without a second recipe or macro domain.

Changing an ABI requires a new embedded module namespace or an explicit schema
version change; Rust struct layout is not the public configuration contract.

### Ordered build-policy composition

`bin/boulder/data/policy/policy.glu` is the single repository policy entry
point. It imports `boulder.build_policy.layers.v1` and names every participating
module in semantic order:

```gluon
let layers = import! boulder.build_policy.layers.v1

layers.policy "aerynos" [
    layers.layer "foundation" [
        layers.add "default.glu",
    ],
    layers.layer "site" [
        layers.modify "site.glu",
    ],
]
```

Only modules named by this manifest participate; Boulder does not enumerate a
policy directory or apply neighboring files implicitly. Layer names are
unique, module origins are normalized relative paths beneath the policy source
root, and the array order is preserved exactly.

Composition is a strict state machine. `add` requires no current policy, while
`replace` and `modify` require one. An `add` or `replace` module returns a
complete `BuildPolicySpec`. A `modify` module returns a total
`BuildPolicyPatchSpec`: every top-level policy field is present, scalar and
structured fields use `Keep` or `Set`, and arrays use `Keep`, `Replace`,
`Prepend`, or `Append`. Replacing an array with `[]` is therefore distinct from
keeping it. Every complete value and every patched intermediate value is
semantically validated before the next operation runs.

`BuildPolicySpec.analyzers` is the repository-authoritative analyzer pipeline,
not an implementation-defined registry order. The default policy declares
`IgnoreBlocked`, `Binary`, `Elf`, `PkgConfig`, `Python`, `CMake`,
`CompressMan`, then `IncludeAny`. The list must be non-empty and unique, and
the `IncludeAny` fallback must appear exactly once at the end. Analyzer patches
use the same order-preserving array operations, so reordering analyzers is a
semantic policy and fingerprint change.

`BuildPolicySpec.build_root.analyzer_tools` names the executable capabilities
for pkg-config, Python, and the LLVM/GNU objcopy and strip variants. Planning
selects only tools reachable from the ordered handlers and package switches,
adds those exact capability requests to `build.lock.glu`, and freezes each
canonical guest program together with its typed requirement. Package analysis
uses those frozen paths; it does not rediscover a tool from `PATH` or infer one
from the selected compiler after the freeze boundary.

`BuildPolicySpec.sandbox.filesystems` is explicit repository data. Its finite
contract omits proc unconditionally, requires a fresh empty tmpfs for `/tmp`,
requires `/sys` to be absent, and permits `/dev` as `none` or `minimal`. The
default selects empty `/tmp`, no `/sys`, and minimal `/dev`. Minimal `/dev`
exposes exactly read-only binds for `null`, `zero`, and `full`; it has no
host-dependent optional nodes and a full host `/dev` view is not representable.
These choices are frozen into the execution policy and participate in the
derivation identity.

`BuildPolicySpec.sandbox.credentials` is likewise explicit. The default policy
selects `isolated_root`; the planner freezes that selection into execution
policy, and frozen container entry rejects an unspecified or mismatched value.

Each successful operation records the policy and layer names, layer and entry
positions, global operation order, operation kind, module origin, and the
module's complete evaluation fingerprint. The final policy fingerprint binds
that ordered stream, including imports of every operation module. It feeds the
build-lock request, selected policy identity, and canonical derivation plan.
`boulder recipe explain` prints both `policy_source` provenance and the ordered
`policy_operation` records; transition, evaluation, and patch failures retain
the same policy/layer/operation/origin context.

## Authored source and generated state

Authored programs and generated values have different roles:

| Artifact | Owner | Rule |
|---|---|---|
| `stone.glu` and relative modules | User/package author | May contain functions and imports; never rewritten by Boulder |
| Boulder build-policy root | OS Tools/vendor | `policy.glu` explicitly orders named layers and operations; unlisted files are ignored and invalid manifests, modules, transitions, or intermediate values are visible errors |
| Profile, repository, and trigger modules | Vendor/admin/user | Evaluated as authored source; invalid fragments are visible errors |
| `sources.lock.glu` | Boulder | Canonical schema-v2 source resolution data, written atomically |
| `build.lock.glu` | Boulder planner | Canonical exact package/output closure, repository snapshots, platforms, and selected policy identities; written atomically |
| Generated `profile.d/*.glu` and `repo.d/*.glu` fragments | Boulder/Moss CLI | Canonical standalone literals marked `@generated`; authored files are protected |
| `/etc/moss/system.glu` | System administrator | Desired state; evaluated but never normalized in place |
| `/usr/lib/system-model.glu` | Moss state transaction | Canonical standalone snapshot stored with the state |

`sources.lock.glu` is adjacent to `stone.glu`. It binds archive hashes and Git
requests to resolved data; schema-v2 Git entries contain a complete commit ID
and required lowercase `materialization_sha256` of the normalized exported
tree. Schema v1 is rejected without a compatibility decoder or runtime
fallback. If source resolution creates or changes the lock, Boulder stops and
asks for a rerun so
the new bytes become part of provenance. An unchanged lock is not rewritten,
and a lock which no longer matches the authored upstream list is a visible
error. Running `boulder recipe update ./stone.glu` without `--ver` or
`--upstream` evaluates only the authored expression, fetches moving Git
references, and atomically refreshes the generated lock. Resolution failure
leaves the previous lock intact. Supplying update values prints structured
authored-change suggestions instead; neither update mode rewrites arbitrary
Gluon expressions. `boulder recipe bump` likewise prints an authored release
suggestion.

Git lock refresh and frozen setup use the same export-normalize-hash path.
The digest commits to raw relative path bytes, entry kinds, canonical modes,
regular-file contents, and raw symlink targets after Git administration data
is removed. Hard links, special inodes, Gitlinks, and a frozen digest mismatch
fail closed before execution. Authored `clone_dir` is a validated single
component and is preserved as the frozen materialization destination; the
outer destination name is separately part of derivation identity.

`build.lock.glu` is adjacent to `stone.glu` and is generated only by explicit
planning, including `boulder build --update-lock`. Its request fingerprint
binds the evaluated recipe and source lock, selected target and policy,
profile, toolchain, builder, job count, and requested providers. Schema v3
contains the exact Moss-resolved package/output closure, only the repository
snapshots used by that closure, build/host/target platforms, and independent
policy-root, target, profile, toolchain, and builder identities. It rejects
disconnected packages, unused snapshots, and any reusable lock whose selected
context or request roots differ even when its header fingerprint was retained.
Planning without `--update-lock` requires a current lock; missing and stale
locks are errors with an explicit refresh command. `--refresh-repositories` is
valid only while updating the lock.

The builder identity names the selected structural family for explanation and
fingerprints the complete target-selected `BuilderSpec`, `HooksSpec`, and
package-profile key. It is not the Boulder executable identity. The derivation
schema freezes the executor ABI and implementation fingerprint separately inside
`ExecutionPolicy`, so changing execution compatibility cannot be
mistaken for changing authored builder structure. It also freezes the selected
credential contract and every reachable analyzer program and provider request.

The Boulder implementation fingerprint is produced at compile time from the
production source tree and effective build context. In addition to the Rust
target, profile, features, compiler, and flags, it binds the selected native C
and C++ compilers, linker, assembler, archiver, ranlib, symbol tool, CMake and
generator tools, their stable version output, compiler/linker search paths,
and the curated native-dependency build controls used by the workspace. Build
timestamps, Git metadata, checkout location, and shadowed tool aliases are not
semantic inputs.

The lock is an explicit resolution input, not an authenticated statement from
a remote service. Boulder validates its graph and selected planner context,
and frozen setup verifies the recorded repository snapshots and exact package
metadata. Any other valid lock content changes the lock digest and derivation
identity; cryptographic publisher trust remains the repository/index layer's
responsibility.

Moss similarly keeps desired intent separate from normalized state. `moss sync
--import path/to/system.glu` evaluates an alternate intent, while `moss state
export` emits a standalone generated snapshot. Export, verification,
activation, and archival operate on the normalized snapshot without rewriting
the administrator's program. Snapshots derived from authored intent retain its
evaluation fingerprint in a generated header comment across later state
updates.

Generated files are stable literals with explicit schema versions and field
ordering. Source-lock and configuration-fragment writers use a temporary file,
sync it, and atomically replace the destination. State snapshots are written
inside the transaction's staging root before activation. Configuration
fragment writers refuse to replace or delete a file which does not carry the
generated marker.

## Fingerprints and provenance

Every evaluation returns its typed value and an `EvaluationFingerprint`. The
aggregate SHA-256 commits to:

- the root source's stable logical name and source hash;
- sorted logical names and hashes for every reachable embedded or relative
  module;
- the exact Gluon release (`0.18.3`);
- configuration ABI version `1`;
- evaluator policy version `1`;
- the hash of explicit inputs, including source-lock bytes where applicable.

Stable logical names are used instead of host paths. Identical source and
inputs therefore produce an identical fingerprint, while a changed import,
lock, ABI, runtime version, or evaluator policy changes it.

For repository build policy, the finalized manifest evaluation also receives a
canonical explicit input containing the policy name, ordered layer and entry
positions, operation kinds, module origins, and complete per-operation
fingerprints. Reordering a layer or operation, changing an operation kind or
origin, or changing an operation module or one of its imports therefore changes
the final policy fingerprint even when the resulting `BuildPolicySpec` happens
to compare equal. An undeclared neighboring file contributes nothing.

Boulder freezes a canonical target-specific `DerivationPlan` and hashes it as
the derivation ID. The canonical data includes the recipe/source identities,
build lock, ordered jobs/phases/steps, environment, builder layout, execution
policy (including every pseudo-filesystem selection), tuning, analyzers,
outputs, and explicit source timestamp. Mutation
tests cover each semantic category. Package and binary-manifest `SourceRef`
metadata carry both `recipe-sha256:` and `derivation-sha256:` values, and the
JSONC build manifest has `recipe-fingerprint` and `derivation-id` fields. The
frozen executor and packager carry that validated ID through artifact emission.
Moss records the authored system-intent fingerprint with each normalized state
snapshot.

## CLI workflow

Typecheck and semantically validate a recipe without starting a build:

```sh
boulder recipe check ./stone.glu
```

The command prints the evaluation fingerprint on success. Parse and type errors
identify the `.glu` source and span; semantic conversion errors identify a
field path such as `meta.release` or `sources[0].url`.

Print the concrete normalized package declaration produced by the factory:

```sh
boulder recipe eval ./stone.glu
```

Boulder build, check, update, and evaluation all use `boulder.package.v3`.
There is no automatic legacy-recipe fallback or dual-source precedence.

Freeze a target-specific derivation and create or refresh its generated build
lock:

```sh
boulder recipe plan ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8 \
    --update-lock
```

The target, timestamp, and job count are explicit semantic inputs. Repeat the
command without `--update-lock` to require and consume the current lock. The
command prints the derivation ID, request fingerprint, target, plan counts,
and canonical plan bytes.

Explain the same locked derivation and its provenance:

```sh
boulder recipe explain ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The explanation includes every policy source and every configured policy
operation with its policy, layer, order, operation kind, module origin, and
fingerprint. This is the same ordered composition identity used by planning;
the command does not rediscover policy state after the plan is frozen.

Normal builds use the same frozen plan:

```sh
boulder build ./stone.glu \
    --profile default-x86_64 \
    --target x86_64 \
    --source-date-epoch 1700000000 \
    --jobs 8
```

The build requires a current `build.lock.glu`. `--update-lock` refreshes it;
`--refresh-repositories` requires `--update-lock`. Runtime setup verifies the
repository snapshots and exact-installs locked package IDs, materializes only
locked sources, and enters the plan-defined container. The executor runs only
frozen steps, `FrozenPackager` consumes plan-owned analysis and collection
rules, binary-manifest verification stays on the host, and cleanup is limited
to plan-owned paths.

Mutable local recipe-directory inputs are rejected before freeze. Supporting
them requires a local-source ABI that hashes their bytes and destination into
the derivation.

Create a skeletal recipe from one or more source archives:

```sh
boulder recipe new --output ./package https://example.invalid/source-1.0.tar.xz
```

The output is `./package/stone.glu`. Edit authored values directly or compose
them through imported functions. Boulder deliberately has no general-purpose
Gluon source rewriter.

Refresh source resolution after editing upstream declarations:

```sh
boulder recipe update ./stone.glu
```

The command writes only `sources.lock.glu`; `stone.glu` and its imported
modules remain byte-for-byte unchanged.

## Compatibility policy

OS Tools configuration has no YAML or KDL compatibility loader, fallback, or
dual-write path. The old YAML updater crate, KDL control-file overlay, and Moss
KDL system-model round trip were removed. A file using an old configuration
extension is ignored where fragment discovery applies; it is never preferred
over Gluon.

The exact external-service YAML allowlist is `.github/dependabot.yml`,
`.github/workflows/ci.yaml`, and `.github/workflows/release.yaml`. No KDL files
are tracked. Negative no-fallback tests, package names containing “yaml”, and
the completed historical migration plan are textual audit exceptions rather
than configuration paths. The Makefile `config-formats` target compares tracked
YAML/KDL paths with this exact allowlist, and `make test` runs the target before
Clippy and the test suite.

## Linkage measurement

Measurements use fresh temporary Cargo target directories and debug binaries;
temporary build output is not committed.

| Measurement | Before Gluon linkage | After migration and dependency cleanup |
|---|---:|---:|
| Boulder binary | 122,949,288 bytes | 146,606,848 bytes |
| Moss binary | 111,252,744 bytes | 136,772,392 bytes |
| Combined build wall time | 22.28 s | 27.86 s |

The final debug measurement increases Boulder by 23,657,560 bytes (19.2%),
Moss by 25,519,648 bytes (22.9%), and the clean combined build by 5.58 seconds
(25.0%). This is the cost of linking the restricted Gluon runtime into both
tools; YAML/KDL and their compatibility dependencies are absent from the final
graph.

## Toolchain compatibility

The completed migration is checked with the workspace MSRV and the release
target, not only the developer toolchain:

| Toolchain and target | Validation |
|---|---|
| Rust 1.91.0, `x86_64-unknown-linux-gnu` | `cargo check --workspace` |
| Rust 1.93.0, `x86_64-unknown-linux-gnu` | full formatting, Clippy, and workspace tests |
| Rust 1.93.0, `x86_64-unknown-linux-musl` | linked Boulder and Moss debug binaries |

Gluon is pinned to `0.18.3` for all three lanes. Its default feature set is
disabled; OS Tools does not enable Gluon's async, regex, or random runtime
facilities.
