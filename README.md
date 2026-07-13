<!--
# SPDX-FileCopyrightText: 2023 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# OS Tools

OS Tools is the package-building and system-state toolkit used by Onix OS. The
workspace contains Moss, Boulder, the `.stone` libraries, and the restricted
Gluon evaluator shared by their declarative interfaces.

This repository is an intentional hard fork of
[AerynOS OS Tools](https://github.com/AerynOS/os-tools). It keeps the original
Git history and a great deal of the original architecture, but it is no longer
a drop-in configuration-compatible AerynOS client.

## Why this is a hard fork

Onix is being built as a declarative Linux userspace. Package recipes, Boulder
policy and profiles, Moss repositories, transaction triggers, and desired
system state are all authored in one language: Gluon.

This is a breaking architecture decision, not a file-extension change. YAML
and KDL loaders, fallbacks, dual writes, and intermediate representations have
been removed. Gluon programs cross a typed and versioned ABI, run inside a
restricted evaluator, and produce fingerprints used for provenance. Authored
programs remain separate from generated source locks and normalized state
snapshots. Boulder additionally freezes exact Moss-resolved closures and
canonical derivation plans whose SHA-256 identity is carried by the package
metadata path.

Keeping the old configuration paths would leave two sources of truth and two
different composition models. It would also make compatibility promises
unclear for both projects. The hard fork makes ownership explicit: AerynOS can
develop OS Tools for its own system and release cycle, while Onix can accept
the breakage required by its Gluon-only model.

This is not a claim that the inherited work has been replaced. Moss, Boulder,
the `.stone` format, and most of the package and state-management foundation
came from the Serpent OS and AerynOS contributors. Please read
[ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md) for the full credit.

## Components

- **Moss** manages packages and system states. It builds content-addressed
  states and activates them atomically.
- **Boulder** evaluates `stone.glu` package factories, resolves exact build
  closures, and executes target-specific frozen plans in an isolated build
  environment.
- **stone** and **libstone** provide Rust and C interfaces for the `.stone`
  package format.
- **gluon_config** is the common evaluator boundary. It controls imports,
  capabilities, resource limits, diagnostics, and evaluation fingerprints.

The workspace is organized by role:

```text
bin/       Moss and Boulder
crates/    shared Rust libraries
docs/      contracts, examples, and design notes
tests/     repository-wide fixtures
misc/      boot integration, MIME data, scripts, and notices
```

## Declarative configuration

Gluon is the only OS Tools configuration language. The main authored entry
points are:

- `stone.glu` for Boulder recipes;
- `profile.glu` and `profile.d/*.glu` for Boulder profiles;
- `repo.glu` and `repo.d/*.glu` for Moss repositories;
- `*.glu` modules for packaged transaction triggers;
- `/etc/moss/system.glu` for desired system state.

OS Tools does not fall back to YAML or KDL. The only YAML allowlist is
`.github/dependabot.yml`, `.github/workflows/ci.yaml`, and
`.github/workflows/release.yaml`; these belong to GitHub's interfaces, not OS
Tools configuration. There are no tracked KDL files. `make test` runs the
`config-formats` allowlist gate so new owned YAML/KDL paths fail validation.

Read the [Gluon configuration contract](docs/gluon-configuration.md) for the
typed ABI, evaluator restrictions, generated-state rules, and CLI workflow.
Read the [package-authoring guide](docs/package-authoring.md) for factories,
dependency scopes, standard builders, typed phases, outputs, locks, and
derivation planning.
Runnable source examples live in [docs/examples/gluon](docs/examples/gluon).

### Package locks and plans

Authored `stone.glu` modules are never rewritten by Boulder. Two adjacent,
generated files freeze I/O-backed resolution:

- `sources.lock.glu` records resolved archives and full Git commits;
- `build.lock.glu` schema v2 records the exact reachable package/output
  closure, used repository snapshots, platforms, and independent
  policy/target/profile/toolchain/builder identities.

After refreshing source resolution, create the build lock and plan with
explicit target, timestamp, and concurrency inputs:

```sh
boulder recipe update ./stone.glu
boulder recipe plan ./stone.glu \
  --profile default-x86_64 \
  --target x86_64 \
  --source-date-epoch 1700000000 \
  --jobs 8 \
  --update-lock
```

Run the same command without `--update-lock` to require the current lock. Use
`boulder recipe explain` with the same arguments to inspect recipe, lock,
policy, profile, and package-closure provenance.

Normal builds use the same planner and lock contract:

```sh
boulder build ./stone.glu \
  --profile default-x86_64 \
  --target x86_64 \
  --source-date-epoch 1700000000 \
  --jobs 8
```

Add `--update-lock` to resolve and atomically refresh `build.lock.glu` before
building. `--refresh-repositories` is accepted only with `--update-lock`.
Boulder exact-installs the locked closure, materializes locked sources, runs
the frozen jobs in the isolated container, packages from plan-owned analysis
and collection rules, optionally verifies a binary manifest on the host, and
cleans only plan-owned paths.

Mutable local files under the recipe `pkg/` directory are currently rejected
before plan freeze. They require a future local-source ABI that hashes their
content into the derivation instead of exposing an untracked host input.

## Development

The tracked Nix shell contains Rust, Clang, CMake, Diesel, Valgrind, and the
tools used by the Makefile.

```sh
git clone https://github.com/onix-os/os-tools.git
cd os-tools
direnv allow

make check
make test
```

Without direnv:

```sh
nix develop
make check
make test
```

`make test` runs Clippy, the formatting check, typos, and all Cargo tests.
Use `make help` to list the other supported targets.

## Local installation

```sh
make get-started
```

This builds Moss and Boulder, fetches the SPDX license list used by Boulder,
and installs the binaries and shared data below `$HOME/.local`. Override the
prefix when needed:

```sh
PREFIX=/opt/onix-tools make get-started
```

Make sure the selected `bin` directory is in `PATH`.

## Safety

Moss uses `/` when no alternate root is provided. Do not experiment against
your host system. Create a disposable root and pass it explicitly:

```sh
mkdir -p aosroot
moss -D "$PWD/aosroot" list installed
```

Full Boulder builds also depend on Linux user namespaces and configured
`subuid`/`subgid` ranges.

## License

OS Tools is available under the [Mozilla Public License 2.0](LICENSE).
