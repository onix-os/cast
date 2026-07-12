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
snapshots.

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
- **Boulder** builds `.stone` packages in an isolated build environment from
  `stone.glu` recipes.
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

OS Tools does not fall back to YAML or KDL. YAML files under `.github/` belong
to GitHub's own interfaces and are not OS Tools configuration.

Read the [Gluon configuration contract](docs/gluon-configuration.md) for the
typed ABI, evaluator restrictions, generated-state rules, and CLI workflow.
Runnable source examples live in [docs/examples/gluon](docs/examples/gluon).

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
