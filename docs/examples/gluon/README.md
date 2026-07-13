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
3. runs the source-less minimal example through the frozen executor and Stone
   packager twice when the host permits an unprivileged build namespace.

The example URLs intentionally use `example.invalid`; `make examples` never
depends on those remote endpoints. The planner proof substitutes
content-addressed local fixtures. The execution proof is limited to `minimal`,
so this corpus does not claim that the fictional upstream projects themselves
can be built.
