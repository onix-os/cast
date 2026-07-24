<!--
# SPDX-FileCopyrightText: 2023 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Cast

Cast is the declarative package-building and system-state toolkit used by
Onix OS. It ships one command: `cast`.

Cast is backed by two internal Rust libraries:

- `mason` evaluates package declarations, freezes build plans, builds, and
  emits Stone packages;
- `forge` manages repositories, package transactions, and system state.

Mason and Forge are implementation boundaries, not commands or public
configuration namespaces. See the [Cast architecture](docs/architecture/cast.md).

## Atomic by design

Cast's core promise is that changing the system is a transaction: it either
happens completely or it did not happen at all, and either way the system can
prove which.

- **Immutable states.** Every install, removal, or sync produces a new
  numbered state — an immutable snapshot of the selected package set. States
  are never mutated, only created and pruned.
- **One kernel-atomic switch.** A new state's `/usr` is fully staged and
  fsynced first, then activated with a single `renameat2(RENAME_EXCHANGE)`
  of the real `/usr` directory. There is no symlink indirection, no script
  window where the system is half-old and half-new: the visible tree changes
  in one atomic kernel operation.
- **A crash journal, not hope.** Activation runs under a phase-ladder
  transition journal with named fsync barriers, boot-epoch and
  mount-namespace evidence, and a single-in-flight invariant. If power dies
  mid-transition, startup reconciliation rolls the journal forward or back
  deterministically.
- **Content-addressed storage by construction.** Every unique file is stored
  once in a content-addressed asset store and hardlinked into each state's
  tree. Deduplication is the storage model, not an optimisation pass.
- **Bootable rollback.** Retained states get their own boot-loader entries;
  rolling back is selecting an older state at boot or re-activating it live
  through the same atomic exchange.
- **Hermetic, locked builds.** Package builds run in namespaced,
  seccomp-filtered sandboxes with **no network access at all** — every input
  enters through hash-pinned source locks and a frozen dependency closure.
  Each build is identified by a SHA-256 derivation identity that covers the
  recipe, locks, policies, environment, and content-hashed toolchain
  binaries.
- **A real, standard filesystem.** All of this happens on an ordinary merged
  `/usr` FHS tree. Unmodified third-party binaries, proprietary software, and
  language package managers work as-is.
- **Typed, bounded configuration.** Every declaration — authored in Gluon or
  Lua — is evaluated once under strict resource limits in a capability-
  restricted sandbox and committed to a SHA-256 evaluation identity. Both
  engines decode into the same neutral Rust values with intentionally distinct
  identities. No configuration language runs in the install path — resolution
  reads binary indexes.

## How Cast compares to Nix

Cast and Nix answer the same question — how to make a system safe to change
and possible to roll back — with the purity boundary in opposite layers. Nix
moves correctness to *evaluation time*: the system is a value computed by a
lazy functional language, outputs are addressed by their inputs, and
activation is a profile flip plus activation scripts. Cast moves correctness
to *transition time*: packages are resolved from binary indexes, files are
content-addressed, and the moment of change is one journaled, kernel-atomic
exchange of the real `/usr`.

| | Cast | Nix / NixOS |
|---|---|---|
| Activation | Single atomic `RENAME_EXCHANGE` under a crash journal | Symlink flip + activation scripts |
| File dedup | Content-addressed store, dedup by construction | Opt-in `nix store optimise` |
| Build network access | Unsupported in the current model; hash-pinned source locks only | Allowed in fixed-output derivations |
| Config language | Typed, resource-bounded, fingerprinted; not in the install path | Untyped, lazy, evaluated on every rebuild |
| Filesystem | Real FHS `/usr`; foreign binaries work unmodified | `/nix/store` paths; patchelf/FHS shims needed |
| Declarative scope | Package set + repositories | Entire system (services, users, kernel) |
| Multi-version / dev shells | One live tree; no shell story (yet) | Native store-path coexistence, `nix develop` |
| Ecosystem | One implementation, young repository | ~140k packages, multiple implementations |

The current boundary is deliberate. Cast does not yet offer per-machine
composed closures, side-by-side package versions, or a whole-system module
language; those directions remain in the [future backlog](FUTURE_PLAN.md)
rather than being rejected permanently. The present work first secures a
compatible filesystem, solver-free installs, and an activation step strong
enough to carry a crash journal. In exchange, Cast keeps most of
the operational value people run NixOS for — atomic updates, bootable
rollback, reproducible locked builds — while looking and behaving like a
normal package manager on a normal Linux tree.

## Components

- **cast** is the sole CLI and public product identity.
- **mason** is the internal build library.
- **forge** is the internal package and system-state library.
- **stone** and **libstone** provide Rust and C interfaces for `.stone`
  packages.
- **declarative_config** is the engine-neutral declaration core: the evaluator
  trait, typed decoders, resource limits, and the shared evaluation identity.
- **gluon_config** and **lua_config** are the two engine adapters, each
  providing restricted evaluation, import policy, and diagnostics behind the
  neutral core.

Both engines are **permanent and security-critical**: Cast supports Gluon and
Lua at full parity as the recorded endpoint (permanent dual support). Every
declaration domain registers both adapters through the shared registry, and
per-domain parity tests pin that equivalent Gluon and Lua sources normalize to
the same value with intentionally distinct evaluation identities. Neither is a
compatibility shim — there is no fallback, cross-language import, dual write, or
same-slot collision (each is a test-pinned error), and each generated slot has
exactly one active language authority. Shipped authorities and documentation
examples are Lua-canonical; Gluon remains fully evaluated, tested, and
documented, and its dependencies are retained.

```text
bin/cast/       external CLI
crates/mason/   internal build library
crates/forge/   internal package/system library
crates/         shared libraries
docs/           contracts, examples, and architecture
tests/          repository-wide fixtures
misc/           boot integration, MIME data, scripts, and notices
```

## Declarative configuration

Cast has two registered declaration languages, Gluon and Lua, selected by file
extension (`.glu` or `.lua`) with no content sniffing, fallback, or
cross-language import. An authored source in either language decodes into the
same neutral Rust value. The main authored entry points are:

- `stone.{glu,lua}` for packages;
- `profile.{glu,lua}` and `profile.d/*.{glu,lua}` for build profiles;
- `repo.{glu,lua}` and `repo.d/*.{glu,lua}` for repositories;
- `/usr/share/cast/triggers/{tx.d,sys.d}/*.{glu,lua}` for packaged triggers;
- `/etc/cast/system.{glu,lua}` for desired system state.

Public Gluon modules use only the `cast.*` namespace, including
`cast.package.v3`, `cast.builders.*.v2`, `cast.profile.v1`,
`cast.repository.v1`, `cast.trigger.v1`, and `cast.system.v1`. Lua declarations
are self-contained, using a uniform tagged encoding rather than imported ABI
modules.

Cast admits only its registered declaration languages. It does not fall back to
YAML or KDL; the only YAML allowlist belongs to external GitHub interfaces under
`.github/`. `make test` runs the `config-formats` gate so owned YAML or KDL
paths fail validation, and the config loader dispatches only `.glu`/`.lua`.

Read the [Gluon configuration contract](docs/gluon-configuration.md), the
[Lua configuration guide](docs/lua-configuration.md), and the
[package-authoring guide](docs/package-authoring.md). Runnable examples live in
[docs/examples/gluon](docs/examples/gluon).

The checked package corpus includes standard builders and deliberately more
Nix-inspired composition patterns: an explicit
[kernel-package specialization](docs/examples/gluon/packages/kernel-module-factory),
[ordered package layers](docs/examples/gluon/packages/layered-overrides), and
fully admitted offline dependency closures for
[Node.js](docs/examples/gluon/packages/nodejs-vendored-application) and
[Maven](docs/examples/gluon/packages/maven-application). `make examples` checks
and evaluates every package root through the public Cast CLI, then freezes each
one twice to prove deterministic lock reuse without network access.

### Package locks and plans

Cast never rewrites authored `stone.glu` modules. Adjacent generated files
freeze I/O-backed resolution:

- `sources.lock.glu` schema v2 records archive hashes and binds each Git
  request to a commit and canonical normalized-checkout digest;
- `build.lock.glu` schema v6 records the exact reachable package/output
  closure, repository snapshots, platforms, policy identities, and typed input
  provenance.

The derivation-plan schema is v16. It binds the Cast implementation identity,
recipe and policy provenance, locks, resolved commands, built-in archive
extraction, environment, outputs, and reproducibility inputs into one SHA-256
derivation identity.

Unpacked sources are limited to tar streams which are plain, gzip-compressed,
xz-compressed, or zstd-compressed with standard frame magic. Cast extracts them
itself through the frozen plan; unsupported compression and container formats
fail closed without an external unpacker fallback. The complete archive
contract is documented in the
[package-authoring guide](docs/package-authoring.md#archive-extraction-contract).

```sh
cast recipe update ./stone.glu
cast recipe plan ./stone.glu \
  --profile default-x86_64 \
  --target x86_64 \
  --source-date-epoch 1700000000 \
  --jobs 8 \
  --update-lock

cast recipe explain ./stone.glu \
  --profile default-x86_64 \
  --target x86_64 \
  --source-date-epoch 1700000000 \
  --jobs 8

cast build ./stone.glu \
  --profile default-x86_64 \
  --target x86_64 \
  --source-date-epoch 1700000000 \
  --jobs 8
```

Run planning without `--update-lock` to require the current lock.
`--refresh-repositories` is accepted only with `--update-lock`.

## Development

```sh
git clone https://github.com/onix-os/os-tools.git
cd os-tools
direnv allow

make check
make test
```

Without direnv, enter `nix develop` first. Use `make help` for the supported
targets.

## Local installation

```sh
make get-started
```

This installs the Cast executable and its shared data below `$HOME/.local`,
with profiles under `$HOME/.config/cast`. Override `PREFIX` when needed:

```sh
PREFIX=/opt/onix-tools make get-started
```

## Safety

System commands use `/` when no alternate root is provided. Test package and
state operations against a disposable root:

```sh
mkdir -p aosroot
cast -D "$PWD/aosroot" list installed
```

Full builds require Linux user namespaces. Unprivileged callers also need
`/usr/bin/newgidmap` and a delegated `/etc/subgid` entry.

Frozen builds must additionally run as the sole process in a systemd cgroup-v2
delegation with this unit policy:

```ini
[Service]
Delegate=cpu memory pids
DelegateSubgroup=cast-supervisor
```

Cast accepts exactly one unified `/proc/self/cgroup` entry ending in
`/cast-supervisor`, authenticates its parent below `/sys/fs/cgroup`, and fails
before cloning a build process if that contract is absent. Each derivation is
then placed atomically in its own leaf with executor-owned ceilings of 4096
PIDs, 32 GiB memory, no swap, and CPU bandwidth equal to the frozen
`execution.jobs` value. These are operational safety limits, not recipe inputs.

## Acknowledgments

Cast builds on prior open-source work; the people and projects it grew
from are credited in [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md).

## License

Cast is available under the [Mozilla Public License 2.0](LICENSE).
