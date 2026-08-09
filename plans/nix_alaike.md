# Plan: Cast Development Environments with a Replaceable `/usr`

> **Executor instructions**: Implement the phases in order. This is not a Nix
> store project. Do not introduce per-package store paths, derivation-addressed
> runtime paths, `ONIX_PATH`, a private home in the default mode, or a new
> system-activation route. Run every verification gate through the Makefile.
>
> **Drift check**:
>
> ```sh
> git diff --stat a393641b..HEAD -- \
>   crates/stone_recipe crates/forge crates/container bin/cast misc/make docs
> ```
>
> If the named current-state contracts have changed, compare their live
> behavior with this plan before editing. Stop rather than silently switching
> to a different architecture.

## Status

- **Priority**: P1
- **Effort**: L, delivered in five reviewable phases
- **Risk**: MEDIUM for root construction, HIGH for namespace activation
- **Category**: development workflow and filesystem isolation
- **Planned at**: commit `a393641b`, 2026-08-09
- **Primary test language**: Lua
- **Supported declaration languages**: Lua and Gluon

## Outcome

`cast develop` builds one exact, merged Cast package closure and exposes its
`usr/` as the development process's real `/usr`.

It has two activation modes:

| Mode | Command | Host home and project | Filesystem activation |
|---|---|---|---|
| Integrated | `cast develop` | Preserved exactly | Replace only `/usr` in a private mount namespace |
| Isolated | `cast develop --isolated` | Hidden except the explicitly admitted project | Pivot into the complete generated root |

Both modes use the same declaration, exact lock, package closure, generated
root, verification, and cache. Only the final namespace activation differs.

## Explicit non-goals

Do not implement:

- a Nix language or Nix evaluator;
- a Nix-style per-package store;
- derivation paths in runtime filenames;
- `/onix/store`;
- package-level profiles or generations;
- environment-only `PATH` and `LD_LIBRARY_PATH` composition;
- global replacement of the host `/usr`;
- system-state activation from development roots;
- multiple versions of one provider inside one closure;
- a private home, `/workspace`, or private `/tmp` in the default integrated
  mode.

Concurrent versions are achieved by two processes mounting two different
complete closure roots in separate mount namespaces.

## Why replacing `/usr` is the correct model

Cast packages already use ordinary merged-`/usr` paths:

- Mason's build environment uses `PATH=/usr/bin:/bin` in
  `crates/mason/src/planner.rs:242`.
- Package analysis accepts payload paths under `/usr` in
  `crates/mason/src/package/analysis/handler.rs:91-104`.
- Build tools, scripts, pkg-config data, CMake data, runtime wrappers, ELF
  interpreters, and shared libraries use absolute `/usr` paths.
- Forge's frozen-root ABI creates:

  ```text
  /bin   -> usr/bin
  /sbin  -> usr/sbin
  /lib   -> usr/lib
  /lib64 -> usr/lib
  /lib32 -> usr/lib32
  ```

  See `crates/forge/src/client/core/root_abi.rs:1-7`.

Changing only environment variables cannot make absolute shebang, interpreter,
library, or resource paths select a different closure. A process-local mount
namespace can.

## Existing foundations to reuse

| Foundation | Location | Required use |
|---|---|---|
| Exact read-only provider closure | `crates/forge/src/client/resolve.rs:7-98` | Resolve development requests without installation |
| Exact content-hash package IDs | `crates/forge/src/repository/manager/source_validation.rs:193-196` | Lock exact package artifacts |
| Cache-only frozen client | `crates/forge/src/client/core/client_facade.rs:44-103` | Avoid system state, triggers, and ActiveReblit |
| Complete frozen-root materialization | `crates/forge/src/client/install.rs:75-108` | Build one merged closure root |
| VFS collision detection | `crates/forge/src/client/core/materialization_facade.rs:62-65` | Reject incompatible package layouts |
| Atomic absent-only publication | `crates/forge/src/client/core/materialization_facade.rs:67-178` | Publish cached roots |
| Independent asset copies | `crates/forge/src/client/core/materialization_facade.rs:140-149` | Avoid asset-pool inode aliasing |
| Exact executable and loader verification | `crates/forge/src/client/core/client_facade.rs:224-253` | Verify the selected shell or command |
| Anchored full-root activation | `crates/container/src/mounts/anchored_root.rs:92-158` | Implement isolated mode |
| Read-only root with selected writable binds | `crates/container/src/lib.rs:203-221` | Protect isolated closure contents |
| Host-network preservation | `crates/container/src/activation.rs:659-668` | Keep normal networking in integrated mode |
| Existing interactive shell path | `crates/mason/src/cli/chroot.rs:29-60` | Reference for TTY and child lifecycle |

## User-visible data locations

### Project inputs

```text
<project>/
├── dev.lua
└── dev.lock.json
```

`dev.glu` is also supported. If both `dev.lua` and `dev.glu` exist in the
same discovery directory, fail and require `--file`.

### Persistent generated data

Resolve the development directory in this order:

1. `cast develop --develop-dir <absolute-path>`
2. `CAST_DEVELOP_DIR`
3. `$XDG_CACHE_HOME/cast/develop`
4. `$HOME/.cache/cast/develop`

The exact layout is:

```text
$CAST_DEVELOP_DIR/
├── schema
├── .cast/
│   ├── assets/
│   └── db/
├── roots/
│   └── <closure-id>/
│       ├── usr/
│       ├── bin -> usr/bin
│       ├── sbin -> usr/sbin
│       ├── lib -> usr/lib
│       ├── lib64 -> usr/lib
│       └── lib32 -> usr/lib32
└── control/
    ├── locks/
    ├── quarantine/
    └── staging/
```

An explicit project-local cache is therefore:

```sh
CAST_DEVELOP_DIR="$PWD/.cast/develop" cast develop
```

Do not discover `$PWD/.cast` automatically.

### Per-invocation runtime data

Use:

```text
$XDG_RUNTIME_DIR/cast/develop/<session-id>/
```

It contains only bounded activation state such as a sealed loader-cache mask and
session control descriptors. It is not the package/root cache. Clean it on
normal exit and bounded failure recovery.

## Declaration contract

Create a separate `cast.develop.v1` domain. Do not add development fields to
`PackageSpec`.

The primary authored format is `dev.lua`:

```lua
return {
    packages = {
        "binary(clang)",
        "binary(cmake)",
        "binary(ninja)",
        "pkgconfig(openssl)",
    },
    shell = "/usr/bin/bash",
}
```

The equivalent `dev.glu` value must lower to the same Rust value.

The language-neutral Rust model is:

```rust
pub struct DevelopmentSpec {
    pub packages: Vec<stone::relation::Provider>,
    pub shell: String,
}
```

Validation requirements:

- package requests are ordered, non-empty, unique after canonical parsing, and
  use `Provider::from_name`;
- `shell` is an absolute normalized path strictly below `/usr/bin`;
- source bytes, request count, request bytes, shell bytes, decoded nodes, and
  total decoded bytes have explicit limits;
- NUL, traversal, duplicate, unsupported relation, and unexpected field inputs
  fail before closure resolution;
- Lua and Gluon adapters share Rust lowering and validation;
- config-language closures or callbacks never cross into the Rust value.

The shell's package provider is part of the exact closure. The planner derives
`binary(<basename>)` from `shell` and adds it when it is not already an
authored request.

## Exact lock

Generate `dev.lock.json` next to the selected declaration.

The lock contains:

- schema version;
- SHA-256 of the normalized language-neutral declaration;
- ordered direct requests;
- selected shell path and shell package ID;
- exact repository snapshot identities;
- exact package hashes;
- exact package-to-package dependency edges;
- root-construction schema version.

The lock must reject unknown schema versions, invalid hashes, missing packages,
missing dependencies, cycles, unreachable packages, duplicate requests, and
snapshot substitution.

Lock behavior:

- first invocation creates the lock atomically;
- an unchanged declaration reuses it without rewriting;
- a changed declaration fails and directs the user to `--update-lock`;
- `--locked` prohibits creation or replacement;
- `--update-lock` performs provider resolution again;
- lock reuse resolves exact package hashes from the recorded immutable
  repository generations and never calls provider selection.

Use deterministic JSON field order and a trailing newline. The lock is generated
data; Lua remains the primary authored and functional-test language.

## Closure identity and cached root

Compute `closure-id` with SHA-256 over a domain-separated canonical encoding:

```text
cast-develop-root-v1\0
```

Encode:

- root schema version;
- the sorted exact package hashes;
- frozen-root ABI version;
- fixed normalization epoch.

Use a fixed normalization epoch of 1. Repository snapshot identity is part of
the lock but not the root identity: identical exact package hashes produce the
same root bytes regardless of which authenticated index selected them.

The root is one complete closure, not one directory per package.

### First realization

1. Open `CAST_DEVELOP_DIR` descriptor-relatively.
2. Resolve exact locked packages.
3. Fetch and verify missing Stone files through the frozen cache.
4. Use `materialize_frozen_root` for the complete package set.
5. Retain its `MaterializedFrozenRoot` proof.
6. Verify the configured shell and its recursive ELF/shebang interpreter chain.
7. Publish only after complete normalization.

### Reuse

Add a production API that opens an existing cached frozen root and verifies:

- the public name still identifies the retained inode;
- every expected VFS entry exists with exact kind, content, target, mode, and
  normalized timestamp;
- no unexpected entry exists;
- root ABI links have their exact targets;
- the selected shell and recursive interpreter chain match the locked closure.

Reuse only after complete verification. On mismatch, take the closure lock,
verify again, detach the complete root into private quarantine, and rebuild it.
Never repair individual cached files in place.

Do not hardlink frozen roots to the asset pool.

## Mode A: Integrated `/usr` replacement

This is the default:

```sh
cast develop
cast develop -- /usr/bin/cmake --version
```

### Visible filesystem

```text
/usr              generated closure, read-only
/home/<user>      normal host home
current directory normal host path
/tmp              normal host /tmp
/etc              normal host /etc, except loader-cache handling below
```

The mode is intentionally not a security sandbox. It preserves normal developer
access to Git, SSH, editors, caches, credentials, project files, networking, and
the rest of the host tree.

### Activation sequence

1. Resolve and verify the cached closure root.
2. Verify that host `/bin`, `/sbin`, `/lib`, `/lib64`, and `/lib32`
   follow Cast's merged-`/usr` ABI. Fail on incompatible host layouts.
3. Spawn a child with only new user and mount namespaces. Do not create new PID,
   network, IPC, or UTS namespaces in integrated mode.
4. Make mount propagation private.
5. Reopen and authenticate the retained `root/usr` descriptor inside the child
   mount namespace.
6. Clone and attach that exact mount over `/usr`.
7. Mark the attached `/usr` recursively read-only.
8. Mask the host `/etc/ld.so.cache` with a sealed empty regular file so the
   selected loader cannot use cached host library paths. Verify on the
   disposable VM that supported Cast loaders fall back to their closure default
   paths.
9. Preserve the caller's UID/GID, supplementary-group policy, `HOME`, current
   directory, terminal, host network, and ordinary environment.
10. Execute the verified closure shell, or the exact argv following `--`.
11. Propagate the child exit status and signals.

The mounted `/usr` disappears automatically when the last process in the
namespace exits. The host mount namespace is never modified.

### Known integrated-mode boundary

Executables under the real home, such as `~/.local/bin/tool`, remain visible
but run against the selected closure's `/usr`. They may fail when they require
an incompatible host library set. This is an explicit convenience-mode
trade-off, not a bug to hide with host-library fallback.

## Mode B: Isolated full development tree

This mode is explicit:

```sh
cast develop --isolated
cast develop --isolated -- /usr/bin/cmake --version
```

### Visible filesystem

- the cached closure root becomes `/` through the existing anchored
  `pivot_root` path;
- the project is the only ordinary host directory admitted, mounted read-write
  at the same absolute path as the caller's current directory;
- `HOME` retains the same pathname but points to a new empty private directory;
- `/tmp` is a fresh bounded tmpfs;
- `/etc` contains only generated minimal passwd, group, hosts, resolver, and
  loader-cache files;
- host `/usr`, host home contents, host `/etc`, and unrelated host paths are
  unavailable;
- networking is disabled unless `--network` is passed.

Use `Container::new_anchored`, `RootFilesystemPolicy::ReadOnly`, authenticated
project binding, and existing pseudo-filesystem policies. Prepare every mount
target before issuing the frozen executable guard.

Do not add `/workspace`. Preserve the project's original absolute pathname so
build diagnostics and generated paths match the integrated mode.

## CLI contract

```text
cast develop [OPTIONS] [-- COMMAND...]

Options:
  --file <PATH>          dev.lua, dev.glu, or their directory
  --develop-dir <PATH>   persistent root/cache directory
  --update-lock          resolve and atomically replace dev.lock.json
  --locked               prohibit lock creation or replacement
  --isolated             pivot into the complete generated root
  --network              allow host networking in isolated mode
```

Rules:

- default discovery checks `./dev.lua`, then `./dev.glu`;
- both files present is an error;
- integrated mode always keeps host networking; reject `--network` without
  `--isolated` as redundant;
- direct commands preserve argv exactly and never pass through a shell string;
- with no command, execute the configured verified shell interactively;
- no shell hook exists in v1;
- no environment export mode exists because the feature depends on a mount
  namespace, not environment-vector construction.

## Lua-first testing mandate

This requirement applies to every phase:

1. All happy-path fixtures are authored as `dev.lua`.
2. All CLI integration tests invoke `--file dev.lua` or default-discover
   `dev.lua`.
3. All real namespace and disposable-VM tests use `dev.lua`.
4. Gluon remains supported and receives focused decoding and differential parity
   coverage.
5. Do not use a Gluon fixture as the only proof of any functional behavior.
6. Extend `make lua-domain-parity-test` with the development declaration
   domain.

## Phase 1: Development declaration and lock

### Files

Create or modify:

- `crates/stone_recipe/src/lib.rs`
- `crates/stone_recipe/src/development/mod.rs`
- `crates/stone_recipe/src/development/lua.rs`
- `crates/stone_recipe/src/development/gluon.rs`
- `crates/stone_recipe/src/development/validation.rs`
- `crates/stone_recipe/src/development/lock.rs`
- Lua-first fixtures below
  `crates/stone_recipe/tests/fixtures/development/`
- `misc/make/lua-tests.mk`

### Work

1. Add the shared `DevelopmentSpec`.
2. Implement validation and budgets in Rust.
3. Implement Lua evaluation first.
4. Implement Gluon evaluation against the same Rust contract.
5. Implement deterministic `dev.lock.json` encoding, decoding, identity, and
   validation.
6. Add the domain to `lua-domain-parity-test`.

Do not change `PackageSpec`, `BuildLock`, or derivation schema v16.

### Verify

```sh
make test TEST_ARGS="-p stone_recipe development::lua"
make lua-domain-parity-test
make check
```

Expected: all commands exit 0. The primary fixture and validation coverage run
through Lua; Gluon produces the same normalized Rust value in parity tests.

### Commit

```text
feat(develop): add lua declaration
```

## Phase 2: Exact closure locking and root cache

### Files

Create or modify:

- `crates/forge/src/development/mod.rs`
- `crates/forge/src/development/lock.rs`
- `crates/forge/src/development/root.rs`
- `crates/forge/src/client/resolve.rs`
- narrowly required repository snapshot modules
- frozen-root open/verification modules
- focused Forge tests

### Work

1. Resolve the ordered Lua requests with `resolve_available_closure`.
2. Add the configured shell provider.
3. Freeze exact package edges and repository snapshots into the lock.
4. Reopen exact immutable snapshots on lock reuse.
5. Resolve exact package hashes without provider selection.
6. Implement `CAST_DEVELOP_DIR` resolution and controlled directories.
7. Compute `closure-id`.
8. Materialize or completely verify one cached full closure root.
9. Quarantine and rebuild invalid roots.

### Lua-first tests

- Lua request order reaches the lock unchanged.
- Repeated Lua evaluation does not rewrite the lock.
- Changed Lua declaration requires `--update-lock`.
- Active repository advancement does not replace a locked snapshot.
- Two Lua projects selecting different exact package hashes create different
  closure IDs and roots.
- Cache tampering causes complete quarantine and rebuild.

### Verify

```sh
make test TEST_ARGS="-p forge development::lock"
make test TEST_ARGS="-p forge development::root"
make check
```

Expected: all commands exit 0; test setup evaluates `dev.lua`.

### Commit

```text
feat(develop): cache exact usr roots
```

## Phase 3: Integrated `/usr` overlay activation

### Files

Create or modify:

- `crates/container/src/usr_overlay.rs`
- `crates/container/src/activation.rs`
- narrowly required mount and process-runtime modules
- `crates/forge/src/development/integrated.rs`
- focused container and Forge tests

### Work

1. Add a descriptor-anchored `UsrOverlay` activation API.
2. Use only user and mount namespaces.
3. Preserve host network, PID, IPC, UTS, home, cwd, and environment.
4. Attach the exact retained `usr` mount read-only.
5. Validate merged-`/usr` host aliases.
6. Mask host loader cache.
7. Execute the verified shell or direct argv.
8. Preserve TTY, signals, foreground process group, and exit status.
9. Prove failure before mount activation leaves the host namespace unchanged.

### Tests

- parent and sibling processes continue to see host `/usr`;
- the Lua-configured shell sees only closure `/usr`;
- real home, current directory, `/tmp`, and host network remain accessible;
- host `/usr/bin` tools absent from the closure disappear;
- closure dynamic ELF uses its exact interpreter and libraries;
- host loader-cache entries cannot select a host library;
- incompatible host root ABI aliases fail before payload execution;
- direct argv metacharacters are not reparsed;
- exit status and signals propagate.

### Verify

```sh
make test TEST_ARGS="-p container usr_overlay"
make test TEST_ARGS="-p forge development::integrated"
make check
```

Expected: all commands exit 0. Capability-dependent live tests use the
repository's established precise skip policy.

### Commit

```text
feat(develop): overlay closure usr
```

## Phase 4: Isolated full-root activation

### Files

Create or modify:

- `crates/forge/src/development/isolated.rs`
- narrowly required container policy modules
- focused Forge and container tests

### Work

1. Reuse the same cached root and exact executable guard.
2. Prepare the exact project-path, private-home, `/tmp`, and minimal-`/etc`
   targets before issuing the guard.
3. Enter with `Container::new_anchored`.
4. Mount the project only, at its original absolute path.
5. Keep the root read-only outside declared writable mounts.
6. Disable networking by default and enable it only with `--network`.
7. Clean private per-session state on normal and interrupted exit.

### Lua-first tests

- the same `dev.lua` and lock activate in both modes;
- isolated mode cannot read a sentinel in the real home;
- integrated mode can read that sentinel;
- isolated mode can modify only the admitted project and private home/tmp;
- host `/usr`, host `/etc`, and unrelated host paths are absent;
- project diagnostics retain the original absolute path;
- networking follows the explicit flag.

### Verify

```sh
make test TEST_ARGS="-p forge development::isolated"
make test TEST_ARGS="-p container live_activation"
make check
```

Expected: all commands exit 0.

### Commit

```text
feat(develop): add isolated mode
```

## Phase 5: CLI, documentation, and real proof

### Files

Create or modify:

- `crates/forge/src/cli/develop.rs`
- `crates/forge/src/cli/mod.rs`
- `bin/cast/src/lib.rs`
- `misc/make/develop-tests.mk`
- `misc/make/project.mk`
- `docs/development-environments.md`
- `README.md`
- paired Lua/Gluon examples, with Lua used in commands

### Work

1. Register and dispatch `cast develop` as a rootless command.
2. Implement the CLI and data-directory precedence exactly as specified.
3. Add command help, completion, and manpage coverage.
4. Add a Makefile target for the disposable-VM integrated and isolated proof.
5. Document Lua first, followed by the equivalent Gluon declaration.
6. Document the integrated-mode compatibility boundary for home executables.
7. Document that cached roots may be deleted when no development shell is
   using them; lock inputs recreate them.

### Real proof

Use `dev.lua` in the disposable VM:

1. Enter integrated mode and prove the same home and project are visible.
2. Prove only `/usr` changed for the child namespace.
3. Run a dynamically linked closure executable.
4. Enter isolated mode from the same lock.
5. Prove the real home and unrelated host paths are absent.
6. Run the same executable.
7. Start two integrated shells with different closure roots concurrently.
8. Prove each sees its own `/usr`.

Do not reboot the disposable VM automatically.

### Verify

```sh
make lua-domain-parity-test
make cast-develop-test
make check
make test
make verify
```

Expected: every command exits 0. The real proof target uses `dev.lua`, not
`dev.glu`.

### Commit

```text
feat(develop): expose dual modes
```

## Global acceptance criteria

- [ ] `cast develop` replaces only `/usr` in its child mount namespace.
- [ ] The default mode preserves the real home, cwd, `/tmp`, environment, and
      host network.
- [ ] `cast develop --isolated` pivots into the complete cached root.
- [ ] Both modes use the same exact lock and cached closure root.
- [ ] `CAST_DEVELOP_DIR` and `--develop-dir` select persistent data.
- [ ] `dev.lua` is the primary documented and functional-test format.
- [ ] `dev.glu` remains supported with differential parity coverage.
- [ ] No per-package Nix-like store exists.
- [ ] No `PATH`/`LD_LIBRARY_PATH` emulation is used as the activation model.
- [ ] No host or system `/usr` mount is modified.
- [ ] Cached roots use independent asset copies.
- [ ] Existing cached roots are completely verified before reuse.
- [ ] Direct commands preserve argv exactly.
- [ ] ActiveReblit and system state code are unchanged.
- [ ] `make check`, `make test`, and `make verify` pass.

## STOP conditions

Stop and report rather than improvising if:

1. Integrated mode requires a global mount or any mutation of the parent mount
   namespace.
2. A supported host does not use the required merged-`/usr` aliases.
3. Masking `/etc/ld.so.cache` does not make supported Cast loaders resolve
   only from the closure's default paths.
4. Exact locked repository generations cannot be reopened without weakening
   repository authentication.
5. Cached root reuse cannot apply the same VFS expectations as first
   materialization.
6. A proposed optimization requires hardlinking the asset pool.
7. Lua and Gluon require different Rust semantics.
8. Integrated activation unexpectedly requires new PID, network, IPC, or UTS
   namespaces.
9. Isolated activation requires exposing the complete real home or host root.
10. Any phase requires modifying ActiveReblit, system states, boot publication,
    or rollback.
11. A focused verification fails twice after a reasonable correction.

## Commit rules

Use title-only Conventional Commits with no signature, body, trailer, or
generated-by text. Keep each title at or below 50 characters. Do not push unless
the operator explicitly requests it.
