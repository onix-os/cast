<!--
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# Cast: one external tool

OS Tools ships one executable: `cast`.

The implementation remains split into two internal library crates:

- `mason` owns package authoring, planning, building, and Stone emission;
- `forge` owns repositories, package transactions, and system state.

The retired split executables are not aliases. They do not remain as
compatibility binaries, Cargo targets, symlinks, or multicall entry points.
`mason` and `forge` are implementation names, not public command namespaces.

## Command ownership

Cast exposes one flat command tree:

- Mason provides `build`, `chroot`, `profile`, and `recipe`.
- Forge provides `boot`, `extract`, `fetch`, `index`, `info`, `inspect`,
  `install`, `list`, `remove`, `repo`, `search`, `search-file`, `state`, and
  `sync`.
- Cast owns the shared `cache` and `version` commands, global options, aliases,
  diagnostics, logging startup, manpage generation, and completions.

`cast cache clean` and `cast cache size` operate on selected Cast caches;
`cast cache prune` removes unreferenced package assets through Forge. The
ambiguous historical `up` shortcut is removed: use `cast recipe update` or
`cast sync` explicitly.

## Public naming boundary

Anything authored, installed, invoked, or reported to a user uses the `cast`
name. This includes the executable and package, `cast.*` Gluon modules,
`/etc/cast` and `/usr/share/cast`, `CAST_*` build variables,
generated-file markers,
boot integration, manpages, completions, and release archives. Rust crate and
module paths alone use `mason` and `forge`.

This is a hard boundary. New code must not add fallback lookup, dual writes,
retired executable aliases, or compatibility imports.

## Validation

The repository must prove that:

1. Cargo metadata contains exactly one binary target, named `cast`.
2. Cast's command tree has no duplicate command or option identifiers.
3. Every Mason and Forge command is reachable through Cast.
4. Help, version output, completions, manpages, services, and release artifacts
   use the Cast name.
5. No production path can launch a retired executable or load a retired public
   namespace.
