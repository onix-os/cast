<!--
# SPDX-FileCopyrightText: 2023 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# OS Tools

OS Tools is the declarative package-building and system-state toolkit used by
Onix OS. It ships one command: `cast`.

Cast is backed by two internal Rust libraries:

- `mason` evaluates package declarations, freezes build plans, builds, and
  emits Stone packages;
- `forge` manages repositories, package transactions, and system state.

Mason and Forge are implementation boundaries, not commands or public
configuration namespaces. See the [Cast architecture](docs/architecture/cast.md).

## Why this is a hard fork

This repository intentionally hard-forks
[AerynOS OS Tools](https://github.com/AerynOS/os-tools). It retains the
original Git history and much of the inherited package and state-management
foundation, but it is not a drop-in configuration-compatible client.

Onix is building a declarative Linux userspace. Package recipes, build policy,
profiles, repositories, transaction triggers, and desired system state are all
authored in Gluon. YAML and KDL loaders, fallbacks, dual writes, and compatibility
representations have been removed.

This is an architectural break, not a file-extension change. Gluon programs
cross typed, versioned, capability-restricted ABIs and produce evaluation
fingerprints. Authored programs remain separate from generated locks and
normalized state. Cast freezes exact dependency closures and canonical
derivation plans before execution.

Keeping the inherited configuration paths would preserve two sources of truth
and two incompatible composition models. The hard fork makes that break
explicit while retaining full credit for the work it builds on. See
[ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md).

## Components

- **cast** is the sole CLI and public product identity.
- **mason** is the internal build library.
- **forge** is the internal package and system-state library.
- **stone** and **libstone** provide Rust and C interfaces for `.stone`
  packages.
- **gluon_config** provides restricted evaluation, import policy, resource
  limits, diagnostics, and fingerprints.

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

Gluon is the only OS Tools configuration language. The main authored entry
points are:

- `stone.glu` for packages;
- `profile.glu` and `profile.d/*.glu` for build profiles;
- `repo.glu` and `repo.d/*.glu` for repositories;
- `/usr/share/cast/triggers/{tx.d,sys.d}/*.glu` for packaged triggers;
- `/etc/cast/system.glu` for desired system state.

Public modules use only the `cast.*` namespace, including
`cast.package.v3`, `cast.builders.*.v2`, `cast.profile.v1`,
`cast.repository.v1`, `cast.trigger.v1`, and `cast.system.v1`.

OS Tools does not fall back to YAML or KDL. The only YAML allowlist belongs to
external GitHub interfaces under `.github/`. `make test` runs the
`config-formats` gate so owned YAML or KDL paths fail validation.

Read the [Gluon configuration contract](docs/gluon-configuration.md) and the
[package-authoring guide](docs/package-authoring.md). Runnable examples live in
[docs/examples/gluon](docs/examples/gluon).

### Package locks and plans

Cast never rewrites authored `stone.glu` modules. Adjacent generated files
freeze I/O-backed resolution:

- `sources.lock.glu` schema v2 records archive hashes and binds each Git
  request to a commit and canonical normalized-checkout digest;
- `build.lock.glu` schema v5 records the exact reachable package/output
  closure, repository snapshots, platforms, policy identities, and typed input
  provenance.

The derivation-plan schema is v13. It binds the Cast implementation identity,
recipe and policy provenance, locks, resolved commands, environment, outputs,
and reproducibility inputs into one SHA-256 derivation identity.

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

## License

OS Tools is available under the [Mozilla Public License 2.0](LICENSE).
