<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Gluon package examples

The recipes under [`packages`](packages) exercise the public
`cast.package.v3` interface as ordinary, pure Gluon programs. They are
deliberately small enough to study, but together cover the package shapes
needed by a declarative userspace.

| Example | What it demonstrates |
|---|---|
| [`minimal`](packages/minimal/stone.glu) | A source-less package using only versioned defaults. |
| [`cmake`](packages/cmake/stone.glu) | CMake flags, checks, and typed build dependencies. |
| [`meson`](packages/meson/stone.glu) | Meson configuration and pkg-config dependencies. |
| [`cargo`](packages/cargo/stone.glu) | An offline Cargo build with features and explicit binaries. |
| [`autotools`](packages/autotools/stone.glu) | Autotools flags, tests, and architecture selection. |
| [`conditionals`](packages/conditionals/stone.glu) | A pure package function driven by typed feature values. |
| [`factory-override`](packages/factory-override/stone.glu) | Dependency-argument overrides followed by a typed attribute patch. |
| [`dependency-roles`](packages/dependency-roles/stone.glu) | Native, target, check, runtime, output, binary, library, and interpreter relations. |
| [`custom-steps`](packages/custom-steps/stone.glu) | Explicit `Run` and declared-program `Shell` steps. |
| [`hooks`](packages/hooks/stone.glu) | Structural pre/post hooks around a standard builder. |
| [`multiple-sources`](packages/multiple-sources/stone.glu) | Archives, locked Git, renamed files, unpack policy, and destinations. |
| [`split-outputs`](packages/split-outputs/stone.glu) | Runtime, development, documentation, and root output rules. |
| [`conflicts`](packages/conflicts/stone.glu) | Typed conflicts, exclusions, provides, and path kinds. |
| [`options-tuning`](packages/options-tuning/stone.glu) | Toolchain choice, hardening, LTO, optimization, and package switches. |
| [`profiles-emul32`](packages/profiles-emul32/stone.glu) | Profile-specific builders and 32-bit dependency roles. |
| [`meta-package`](packages/meta-package/stone.glu) | A source-less package that declaratively composes a userspace. |
| [`realistic-daemon`](packages/realistic-daemon/stone.glu) | A larger daemon with hooks, services, multiple outputs, dependencies, and tuning. |

Run the complete checked-in proof lane from the repository root:

```sh
make examples
```

That target:

1. discovers every package directory and runs public `cast recipe check` and
   `cast recipe eval`, requiring deterministic repeated evaluation and no
   source-tree mutation;
2. freezes every example with hermetic local source and repository fixtures,
   writes and reuses its exact `build.lock.glu`, and requires identical plan
   bytes and derivation IDs; and
3. proves that the synthetic metadata-only providers used for planning cannot
   cross the frozen executable boundary or publish a derivation.

The example URLs intentionally use `example.invalid`; `make examples` never
depends on those remote endpoints. The planner proof substitutes
content-addressed local fixtures. This lane deliberately does not claim that
the fictional upstream projects can be built. Real compilation and packaging
belong to the contentful execution-fixture lane below.

## Representative execution fixtures

Six separate fixtures contain small, real source trees for the Autotools,
Cargo, CMake, custom-step, Meson, and split-output package shapes. Run their
proof lanes from the repository root:

```sh
make execution-fixtures
make bootstrap-fixtures
make fixtures-ci
```

`make execution-fixtures` is the offline lane: it byte-checks the deterministic
source archives, validates the pinned Stone index and closure declaration, and
proves that all six recipes resolve to that exact closure. `make
bootstrap-fixtures` fetches and verifies any missing pinned Stone files,
materializes the production-format root mirror, then builds, packages, and
reproduces every fixture. It may skip execution when the host cannot create the
required namespaces; pass `REQUIRE_EXECUTION=1` to reject that skip. `make
fixtures-ci` runs both lanes and always requires execution.

Execution requires Linux user and mount namespaces. For an unprivileged caller,
the current mapper specifically requires `/usr/bin/newgidmap` and at least one
delegated GID in `/etc/subgid`; the usual `uidmap` package provides the helper.
The UID map is written directly, so `/usr/bin/newuidmap` and `/etc/subuid` are
not currently consumed. Check the basic namespace capability with:

```sh
unshare --user --map-root-user --mount true
```

Some hosts disable unprivileged namespaces through
`kernel.unprivileged_userns_clone`. Ubuntu hosts may additionally set
`kernel.apparmor_restrict_unprivileged_userns=1`; the required CI lane enables
the former and temporarily sets the latter to `0`. Changing either setting is
a host security-policy decision and may require an administrator.
