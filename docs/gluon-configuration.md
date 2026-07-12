<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Gluon configuration

OS Tools uses [Gluon](https://gluon-lang.org/) as its only declarative
configuration language. Boulder recipes, macro policy, profiles, Moss
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
| Boulder recipe | `stone.glu` | `boulder.recipe.v1` |
| Boulder macro policy | `boulder/data/macros/**/*.glu` | `boulder.macros.v1` |
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
   `boulder.recipe.v1` and `moss.system.v1`.
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

The current configuration ABI version and each consumer ABI are version `1`.
The embedded modules expose constructors, defaults, explicit option/boolean
variants, and immutable records. Gluon-facing DTOs use only stable language
shapes such as strings, integers, arrays, records, and explicit variants.

Record update syntax makes policy composition ordinary Gluon rather than a
sidecar overlay format:

```gluon
let boulder = import! boulder.recipe.v1
let add_runtime = import! "./package_policy.glu"

let source = boulder.source {
    name = "hello",
    version = "1.0.0",
    release = 1,
    homepage = "https://example.invalid/hello",
    license = ["MPL-2.0"],
}

add_runtime (boulder.recipe source)
```

`boulder.compose` provides typed append, prepend, and override operations for
build phases, dependency arrays, packages, profiles, and subpackages. Patch
records distinguish an omitted change from overriding an array with `[]`.

Changing an ABI requires a new embedded module namespace or an explicit schema
version change; Rust struct layout is not the public configuration contract.

## Authored source and generated state

Authored programs and generated values have different roles:

| Artifact | Owner | Rule |
|---|---|---|
| `stone.glu` and relative modules | User/package author | May contain functions and imports; never rewritten by Boulder |
| Macro, profile, repository, and trigger modules | Vendor/admin/user | Evaluated as authored source; invalid fragments are visible errors |
| `sources.lock.glu` | Boulder | Canonical standalone source resolution data, written atomically |
| Generated `profile.d/*.glu` and `repo.d/*.glu` fragments | Boulder/Moss CLI | Canonical standalone literals marked `@generated`; authored files are protected |
| `/etc/moss/system.glu` | System administrator | Desired state; evaluated but never normalized in place |
| `/usr/lib/system-model.glu` | Moss state transaction | Canonical standalone snapshot stored with the state |

`sources.lock.glu` is adjacent to `stone.glu`. It binds archive hashes and Git
requests to resolved data; Git entries contain a complete commit ID. If source
resolution creates or changes the lock, Boulder stops and asks for a rerun so
the new bytes become part of provenance. An unchanged lock is not rewritten,
and a lock which no longer matches the authored upstream list is a visible
error. Running `boulder recipe update ./stone.glu` without `--ver` or
`--upstream` evaluates only the authored expression, fetches moving Git
references, and atomically refreshes the generated lock. Resolution failure
leaves the previous lock intact. Supplying update values prints structured
authored-change suggestions instead; neither update mode rewrites arbitrary
Gluon expressions. `boulder recipe bump` likewise prints an authored release
suggestion.

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

Boulder records the aggregate recipe fingerprint in package and binary-manifest
`SourceRef` metadata and in the JSONC build manifest. Moss records the authored
system-intent fingerprint with each normalized state snapshot.

## CLI workflow

Typecheck and semantically validate a recipe without starting a build:

```sh
boulder recipe check ./stone.glu
```

The command prints the evaluation fingerprint on success. Parse and type errors
identify the `.glu` source and span; semantic conversion errors identify a
field path such as `source.release` or `upstreams[0].url`.

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

YAML required by external services, notably files under `.github`, is outside
this configuration contract and remains in the repository.

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
