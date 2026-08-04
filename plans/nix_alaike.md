# Architecture & Implementation Plan: Nix-Like Store & Development Environments (`onix develop`)

This document details the concrete architectural design and implementation plan to evolve Onix OS (`cast`, `mason`, `forge`) towards Nix-like capabilities, specifically adding an addressable package store (`/onix/store`), multi-version dependency closures, and reproducible development shells (`cast develop` / `onix develop`).

---

## 1. Executive Summary & Design Goals

### Current Cast Model
Cast enforces system correctness at **transition time** via a single atomic `/usr` tree staged from content-addressed file assets (`/var/lib/cast/assets/v2/`) and swapped in one kernel `renameat2(RENAME_EXCHANGE)` operation (backed by the `ActiveReblit` crash journal).

### The Nix Parity Objective
While keeping Cast's atomic crash-safe `/usr` transition for system state, Onix will add:
1. **Isolated Addressable Store Paths (`/onix/store/<derivation_id>-<pname>-<version>`)**
2. **Hash-bound dependency graphs** in package manifests and resolution locks.
3. **Reproducible Development Shells (`cast develop`)** without modifying the global `/usr`.
4. **FHS Parity inside Dev Shells** via unprivileged Linux User Mount Namespaces (allowing standard compilers and third-party binaries to work without `patchelf`).

---

## 2. Comparative Architecture Overview

| Architectural Domain | Current Cast Architecture | Target Nix-Like Architecture |
|---|---|---|
| **Package Storage** | Flat content-addressed assets (`/var/lib/cast/assets/v2/<hash>`) | Addressable store paths (`/onix/store/<derivation_id>-<name>-<ver>/`) hardlinked from asset pool |
| **System State Activation** | Atomic `RENAME_EXCHANGE` of `/usr` under `ActiveReblit` journal | Global `/usr` is a managed projection view of selected `/onix/store/` packages |
| **Dependency Resolution** | Hashed in build plan (`DerivationId`), but provider string based in `.stone` manifests | Manifests bind dependency `derivation_id` hashes directly |
| **Multi-Version Coexistence** | Unsupported in live `/usr` due to FHS collisions | Coexists natively in `/onix/store/` |
| **Dev Environments** | One live system state; no dev shell | Native `cast develop` subshell with environment vector or mount namespace isolation |

---

## 3. Core Architectural Pillars

```text
                               ┌──────────────────────────────────────────────┐
                               │     Content-Addressed Asset Storage          │
                               │   /var/lib/cast/assets/v2/<file_hash>       │
                               └──────────────────────┬───────────────────────┘
                                                      │ (hardlinks)
                      ┌───────────────────────────────┴───────────────────────────────┐
                      ▼                                                               ▼
        ┌───────────────────────────────┐                               ┌───────────────────────────────┐
        │       /onix/store Path 1      │                               │       /onix/store Path 2      │
        │ /onix/store/8f3919-openssl-3.1│                               │ /onix/store/7d89e2-openssl-1.1│
        └──────────────┬────────────────┘                               └──────────────┬────────────────┘
                       │                                                               │
        ┌──────────────┴────────────────┐                               ┌──────────────┴────────────────┐
        │     cast develop (Proj A)     │                               │     cast develop (Proj B)     │
        │ PATH=/onix/store/8f3919.../bin│                               │ PATH=/onix/store/7d89e2.../bin│
        └───────────────────────────────┘                               └───────────────────────────────┘
```

### Pillar 1: The `/onix/store` Layout
Packages are materialized under:
```text
/onix/store/
├── 8f391985a2c4e10b-openssl-3.1.0/
│   ├── bin/
│   ├── include/
│   └── lib/
├── c6a2f104e1990a2b-cmake-3.28.0/
└── 12b4e8990a8871ce-python-3.12.0/
```
* **Derivation Hash**: `8f391985a2c4e10b` is generated directly by `mason`'s v16 `DerivationPlan::derivation_id()`.
* **Zero Storage Overhead**: Files in `/onix/store/...` are hardlinked directly from Cast's existing `/var/lib/cast/assets/v2/<hash>` pool.

### Pillar 2: Hashed Dependency Provenance
Extend `.stone` manifests and `DerivationPlan` to bind dependency `derivation_id` values:
```lua
-- In build.lock.glu / stone.glu manifest
build_inputs = {
  { name = "openssl", version = "3.1.0", derivation_id = "8f391985a2c4e10b" },
  { name = "zlib", version = "1.3.0", derivation_id = "7d89e210a4..." }
}
```

### Pillar 3: Development Shell Engine (`cast develop`)
Introduce `cast develop` (or `onix develop`) in the `cast` CLI:
- Evaluates `stone.lua` / `stone.glu` / `dev.lua`.
- Realizes required dependency store paths in `/onix/store/`.
- Constructs environment variables (`PATH`, `PKG_CONFIG_PATH`, `LD_LIBRARY_PATH`, `CPATH`, `CMAKE_PREFIX_PATH`).
- Launches the interactive subshell with `shell_hook` execution.

---

## 4. Phase-by-Phase Implementation Roadmap

### Phase 1 — Store Path Abstraction (`crates/forge/src/store.rs`)
- Implement `StorePath` and `StoreManager` in `forge`.
- Add `materialize_package_to_store(installation, package_meta, derivation_id)`:
  - Creates `/onix/store/<derivation_id>-<pname>-<version>/`.
  - Hardlinks individual files from `/var/lib/cast/assets/v2/<content_hash>`.

### Phase 2 — Hashed Dependency Provenance in `.stone` Manifests
- Update `crates/mason/src/package/emit/manifest.rs` to record dependency `derivation_id` entries alongside provider relations (`binary`, `soname`, `pkgconfig`).
- Update `crates/stone_recipe/src/derivation/mod.rs` to expose dependency store mappings to planners.

### Phase 3 — `dev_shell` Declaration Schema
- Extend `declarative_config`, `lua_config`, and `gluon_config` with a `dev_shell` schema:
```lua
return {
  name = "my-project",
  version = "0.1.0",

  dev_shell = {
    packages = {
      "sys-libs/openssl@3.1.0",
      "dev-util/cmake@3.28.0"
    },
    env = {
      RUST_BACKTRACE = "1"
    },
    shell_hook = [[
      echo "Entering Onix Dev Shell..."
    ]]
  }
}
```

### Phase 4 — `cast develop` Subcommand (`bin/cast`, `crates/forge`)
- Add CLI subcommands for `cast develop` (`--file`, `--command`, `--export-env`).
- Environment Vector Builder:
  - `PATH`: prepends `/onix/store/<drv_id>-<name>/bin`
  - `PKG_CONFIG_PATH`: prepends `/onix/store/<drv_id>-<name>/lib/pkgconfig`
  - `CPATH` / `C_INCLUDE_PATH`: prepends `/onix/store/<drv_id>-<name>/include`
  - `LIBRARY_PATH` / `LD_LIBRARY_PATH`: prepends `/onix/store/<drv_id>-<name>/lib`
  - `CMAKE_PREFIX_PATH`: prepends `/onix/store/<drv_id>-<name>`
- Spawns target shell (`$SHELL` or `/bin/bash`) executing declared `shell_hook`.

### Phase 5 — FHS Mount Namespace Isolation (Optional Sandbox Mode)
- Support `--sandbox` / `--fhs` flag for `cast develop`:
  - Uses unprivileged Linux user mount namespaces (`unshare -m -U`) to mount an ephemeral `/usr` tree (via OverlayFS / tmpfs) populated only with the project's declared dev dependencies.
  - Allows standard compilers and tools (`gcc`, `clang`, `cargo`, `npm`) to work natively in `/usr` inside the shell without needing `patchelf` or RPATH modifications!

### Phase 6 — Garbage Collection & Shell Tooling
- `cast store gc`: Extends `forge/src/client/prune.rs` to sweep unreferenced `/onix/store/` paths while preserving pinned system states and active dev shell roots.
- `cast direnv dump`: Generates `.envrc` compatible environment exports for seamless IDE integration.
