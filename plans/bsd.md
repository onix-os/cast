# Splitting Cast into core, platform primitives, and deployment backends — Architecture and Implementation Plan

Based on a four-track audit of the workspace (the `container` crate, `forge`, `mason`/`bin/cast`/support
crates, and a repo-wide marker sweep), 2026-07-20. No code was changed as part of this analysis.

**Ordering:** this plan is implemented before [`bootc.md`](bootc.md). It must separate two axes:
`cast-platform-*` supplies OS primitives, while deployment engines own state, activation, rollback,
and boot publication. Otherwise Cast-native `cast.fstx` semantics become a false universal platform
contract and the later Linux-only bootc adapter has no honest sibling seam.

## 1. Executive summary

- **The workspace is unconditionally Linux by construction.** There is no `cfg(target_os = "linux")`
  gating anywhere (only 7 trivial `#[cfg(unix)]` test sites). Coupling is expressed directly through
  `nix` (~2,900 refs), `libc` (~3,500 refs), and raw syscall numbers, deliberately pinned to a
  documented **Linux 5.6 syscall baseline** (`crates/forge/src/linux_fs.rs`,
  `crates/mason/src/linux_fs.rs`).
- **Coupling is concentrated, not diffuse.** Three crates hold nearly all of it: `forge`
  (block/GPT/sysfs, boot, transaction primitives), `container` (namespaces, mounts, seccomp,
  cgroups), `mason` (build-sandbox glue). Twelve of eighteen crates are already neutral or one shim
  away.
- **Forge's transaction core is neutral in logic but Linux in expression** — install planning,
  resolution, journaling, and activation are portable ideas implemented directly in
  `openat2`/`renameat2`/procfs idioms rather than through any abstraction. ~70 files import
  `linux_fs`.
- **Two genuine architecture gaps for FreeBSD:** rootless builds (user namespaces have no jail
  analog) and the `RENAME_EXCHANGE`-centered atomic `/usr` activation (FreeBSD has no `renameat2`).
- **Service-manager coupling is nearly zero.** The only runtime D-Bus/systemd use is one logind
  `Inhibit` call in `crates/forge/src/signal.rs:36-62`. Everything else "systemd" is systemd-boot
  (BLS) data or deployment units.

## 2. Current dependency graph

Internal edges (from manifests), with coupling verdicts — **red** = deep Linux, **amber** =
shallow/extractable, **green** = neutral:

```
cast (bin) ──► forge, mason, container, tui, tools_buildinfo, tracing_common
mason      ──► forge, container, gitwrap, config, gluon_config, stone,
               stone_recipe, version_parse, tools_buildinfo, tui
forge      ──► container, config, dag, stone, triggers, tui, vfs,
               fnmatch, gluon_config
triggers   ──► dag, fnmatch, gluon_config
config     ──► gluon_config
libstone   ──► stone

External platform deps:  forge ──► blsforme, zbus, nix
                         container ──► nix, nc (raw syscalls)
                         mason ──► elf, nix
```

- **Red:** `forge` (7,769 pattern hits — linux_fs alone ~3,000), `container` (1,765), `mason` (1,418)
- **Amber:** `config`, `gluon_config` (raw `openat2`/`renameat2`/`__errno_location`), `gitwrap`
  (POSIX-mostly), `bin/cast` (broker mode)
- **Green:** `stone`, `stone_recipe`, `libstone`, `vfs`, `dag`, `triggers`, `fnmatch`, `astr`,
  `version_parse`, `tui`, `tracing_common`, `tools_buildinfo`

```mermaid
graph LR
  cast["cast (bin)"] --> mason
  cast --> forge
  cast --> container
  cast --> tui
  cast --> tools_buildinfo
  cast --> tracing_common
  mason --> forge
  mason --> container
  mason --> gitwrap
  mason --> config
  mason --> stone
  mason --> stone_recipe
  mason --> version_parse
  mason --> gluon_config
  forge --> container
  forge --> config
  forge --> dag
  forge --> stone
  forge --> triggers
  forge --> vfs
  forge --> fnmatch
  forge --> gluon_config
  triggers --> dag
  triggers --> fnmatch
  triggers --> gluon_config
  config --> gluon_config
  libstone --> stone
  forge -.-> B[(blsforme · zbus · nix)]
  container -.-> N[(nix · nc raw syscalls)]
  mason -.-> E[(elf · nix)]
  classDef ok fill:#2b8a3e,stroke:#1d5f2b,color:#fff
  classDef warn fill:#e08a00,stroke:#9c6100,color:#fff
  classDef hot fill:#c92a2a,stroke:#8f1d1d,color:#fff
  classDef ext fill:none,stroke-dasharray:4 3
  class stone,stone_recipe,version_parse,dag,vfs,fnmatch,triggers,tui,tools_buildinfo,tracing_common,libstone ok
  class cast,config,gluon_config,gitwrap warn
  class forge,mason,container hot
  class B,N,E ext
```

## 3. Subsystem inventory and classification

| # | Subsystem | Lives in | Verdict | Key Linux dependencies |
|---|---|---|---|---|
| 1 | Package format & archive I/O | `stone`, `libstone`, `stone_recipe`, `version_parse`, `astr` | **Neutral** | None; `stone_recipe`'s sandbox schema is Linux-*shaped* but code is pure |
| 2 | Dependency resolution & registry | `dag`, `vfs`, `forge::{registry,package,dependency}` | **Neutral** | None; architecture is a repo-index property, never host-detected (no `uname` anywhere) |
| 3 | CLI & UX | `bin/cast`, `tui`, `tracing_common` | **Neutral** | Thin dispatcher; one hidden Linux mode (`--private-device-broker`) |
| 4 | State & metadata DBs | `forge::db` | **Partial** | Diesel/SQLite neutral; connections anchored via `/proc/self/fd/<n>` paths (`db/meta/mod.rs:41` etc.) |
| 5 | Repository mgmt & fetch | `forge::repository` | **Partial** | Fetch is reqwest/tokio; storage uses `openat2` + proc-fd capability paths; `flock` portable |
| 6 | Configuration & triggers | `config`, `gluon_config`, `triggers`, `forge::system_model` | **Partial** | Gluon eval pure; file access on raw `openat2`/`RESOLVE_*`, `SYS_renameat2`, glibc-only `__errno_location` |
| 7 | Git/upstream materialization | `gitwrap`, parts of `mason::upstream` | **Partial** | Mostly POSIX (rlimits, setpgid, *at); one raw `renameat2` |
| 8 | Transaction core & activation | `forge::client` core, `tree_marker`, `transition_journal`, `installation` | **Tight** | Neutral logic, Linux expression: `openat2`, `RENAME_EXCHANGE` /usr hot-swap, `O_TMPFILE`+linkat, xattr ACL guards, authenticated procfs, `boot_id`, mount-ns identity |
| 9 | Block devices & mount topology | `forge::linux_fs` | **Tight (by charter)** | sysfs block parsing, `BLKSSZGET`/`BLKGETSIZE64` ioctls, mountinfo grammar, devtmpfs evidence, ns-file identity |
| 10 | Boot management | `forge::client/boot*` + `blsforme`, `misc/boot` | **Tight** | systemd-boot + BLS + UKI only (no GRUB); EFI vars, ESP/XBOOTLDR, `cast.fstx` cmdline, dracut initramfs early activation |
| 11 | Sandboxing & supervision | `container` | **Tight (deepest)** | clone3 + 8 namespace types, new mount API (`open_tree`/`move_mount`/`fsmount`/`mount_setattr`), pivot_root, seccomp-BPF (x86_64-hardcoded), cgroup v2, uid_map rootless model, pidfd, memfd seals, device broker |
| 12 | Build orchestration | `mason` | **Mixed** | Planner/recipe/ELF analysis largely neutral; executor needs PID-ns-init reaping, `execveat`, `sched_setaffinity`, `/proc/self/cgroup` discovery |
| 13 | Service/session integration | `forge::signal`, `misc/systemd` | **Partial** | One logind Inhibit call; broker relies on systemd socket activation + `CAP_MKNOD` |
| — | Deployment & test harness | `Makefile` (~100 suites), `misc/`, CI | **Tight** | `linux-*`-named suites, `systemd-run` fixtures, dracut module, Ubuntu-only CI |

## 4. Proposed crate structure

```
cast (composition root)
 ├─► cast-core                  resolve exact Stone closures; compose frozen roots;
 │                              package/system planning and neutral identity
 ├─► cast-platform              OS-PRIMITIVE TRAITS ONLY (no deployment policy)
 ├─► cast-deploy-native         common native deployment contracts
 ├─► mason-core ───────────────► cast-sandbox (build-host contract)
 ├─[cfg(linux)]──► cast-platform-linux
 │                 cast-deploy-native-linux ──► blsforme + Linux activation/boot policy
 │                 cast-sandbox-linux (today: container)
 └─[cfg(freebsd)]► cast-platform-freebsd
                   cast-deploy-native-freebsd ─► strategy chosen at D0
                   cast-sandbox-freebsd (jails/rctl)
 later, Linux only: cast-deploy-bootc ──► OCI + upstream bootc (see bootc.md)
 (stone, vfs, dag, triggers, stone_recipe, … remain neutral building blocks)
```

```mermaid
graph TD
  bin["cast (binary)"] --> core[cast-core]
  bin --> native["cast-deploy-native (contracts/common)"]
  bin --> mcore[mason-core]
  mcore --> sapi["cast-sandbox (build-host API)"]
  bin -- "cfg(linux)" --> pl[cast-platform-linux]
  bin -- "cfg(freebsd)" --> pf[cast-platform-freebsd]
  bin -- "cfg(linux)" --> nl[cast-deploy-native-linux]
  bin -- "cfg(freebsd)" --> nf[cast-deploy-native-freebsd]
  bin -- "cfg(linux)" --> sl[cast-sandbox-linux]
  bin -- "cfg(freebsd)" --> sf[cast-sandbox-freebsd]
  mcore --> core
  core --> papi["cast-platform (OS primitives only)"]
  mcore --> papi
  pl -- implements --> papi
  pf -- implements --> papi
  nl --> native
  nf --> native
  nl --> core
  nf --> core
  nl --> papi
  nf --> papi
  sl -- implements --> sapi
  sf -- implements --> sapi
  sl --> papi
  sf --> papi
  nl --> bfl["blsforme · Linux activation/boot"]
  nf --> bff["D0-selected FreeBSD activation/boot"]
  core --> neutral["stone · vfs · dag · triggers · stone_recipe · ..."]
  future["cast-deploy-bootc (later, Linux only)"] -.-> core
  future -.-> upstream["OCI · upstream bootc"]
```

- `cast-core` owns reusable exact-closure and frozen-root composition as well as planning; it never
  activates a host. Concrete native adapters own Cast state semantics and consume injected platform
  primitives. bootc will be their sibling, not a `cast-platform-linux` implementation and not a
  client of the native deployer.
- Keep **sandbox crates separate** from `cast-platform-*`: `container` is already a standalone unit
  with its own harness and two consumers, and FreeBSD sandboxing ships on a different schedule than
  FreeBSD fs primitives. Mason consumes the sandbox API; the composition root injects the selected
  sandbox and platform implementations.
- The existing `Error::execution_capability_unavailable()` probe pattern
  (`crates/container/src/lib.rs:562`) is the natural front door for platform capability
  negotiation — consumers already degrade gracefully through it.

## 5. Trait boundaries

Contracts must be stated as **guarantees, not syscalls**, or Linux semantics leak into core and the
FreeBSD backend becomes an emulation layer.

### `PlatformFs` — kernel interaction / filesystem primitives

Guarantee: confinement-checked primitives with explicit durability and capability reporting. It
must not promise a deployment algorithm that one supported OS cannot implement.

- Scoped open: resolve beneath an anchor, no symlinks, no device/magic-link escape
- Anonymous file → link-into-place publication; no-replace rename; advertise atomic pairwise
  exchange only when the platform actually supplies it
- Durable sync of file *and* parent directory; descriptor re-open capability (the current
  `/proc/self/fd` idiom)
- Metadata guards: xattr/ACL rejection, noatime open, secure random

Linux: `openat2 RESOLVE_*`, `O_TMPFILE`+`linkat`, `renameat2` `EXCHANGE`/`NOREPLACE`, fsync+dirsync,
`fsetxattr`, `/proc/self/fd`.
FreeBSD: `O_RESOLVE_BENEATH` (exists), tmpfile+linkat, fdescfs re-open, extattr — but no
rename-exchange. Directory exchange versus boot-environment promotion is selected by the native
deployment strategy, not emulated as a universal `PlatformFs` guarantee (see R2).

### `RuntimeEvidence` — process & boot-epoch authority

Guarantee: authenticated facts about "which boot, which mount view, which process state am I in."

- Boot epoch identifier; mount-namespace/view identity; single-thread audit; child fd-leak audit
- Supervised child handle: race-free signal + reap on a first-class handle

Linux: `/proc/sys/kernel/random/boot_id`, ns-file `st_dev/ino`, authenticated procfs walk, pidfd +
`waitid(P_PIDFD)`.
FreeBSD: `kern.boottime`/boot UUID sysctl, no mount namespaces (trait returns a constant identity),
`sysctl kern.proc`, `pdfork`/`pdkill` + kqueue `EVFILT_PROCDESC` (arguably a cleaner fit than pidfd).

### `DiskTopology` — block devices & mounts

Guarantee: identify the device and partition backing a mounted tree, and verify partition-table
identity.

- Enumerate block devices/partitions; GPT table identity (the parser itself is pure and stays in
  core)
- Device-number canonicalization; filesystem-type identity; bounded mount-table snapshot

Linux: `/sys/block` uevent/links, `BLKSSZGET`/`BLKGETSIZE64` ioctls, `/proc/*/mountinfo`, devtmpfs +
`f_type` magics.
FreeBSD: GEOM confxml sysctl / libgeom, `DIOCGSECTORSIZE`/`DIOCGMEDIASIZE`, `getfsstat`,
`f_fstypename` strings.

### `NativeActivationStrategy` and `NativeBootBackend`

These contracts belong to `cast-deploy-native` and its OS adapters, not the universal platform API.
They select an activation algorithm, then publish native Cast states so each is boot-selectable and
the newest is default. A later bootc deployer bypasses them because upstream bootc owns its
deployments and boot.

- Discover kernels from the layout DB (already neutral); render entries; publish/sync boot assets;
  bounded rollback set; encode the selected native state using the D0-proven OS mechanism

Linux native: directory exchange plus blsforme BLS/UKI publication, EFI variables, and dracut
early-boot activation using `cast.fstx=<id>`.
FreeBSD native: D0 must choose and prove its activation strategy before extraction starts. ZFS boot
environments with loader.conf/bectl/rc.d are one leading candidate, not an assumed universal
filesystem primitive.

### `Sandbox` — isolated execution

Guarantee: run a payload in a confined root with declared binds, pseudo-filesystems, network
policy, and enforced resource ceilings; kill/drain reliably.

- Builder: root anchor, ro/rw binds, proc/tmp/sys/dev policy, hostname, networking, loopback
- Run payload; run in resource domain (limits: pids/memory/cpu); group kill + drain-until-empty
- Capability probe: "can this host sandbox at all, and rootlessly?" (the existing
  `execution_capability_unavailable` pattern becomes the trait's front door)

Linux: clone3 + namespaces, new mount API, pivot_root, seccomp, caps drop, cgroup v2 domains,
uid_map rootless model.
FreeBSD: `jail_set`/`jail_attach` (root-required), nullfs binds, per-jail devfs rulesets (which also
make the private-device broker unnecessary), VNET, rctl + cpuset, `jail_remove` as group-kill (a
*stronger* primitive than `cgroup.kill`), `procctl(PROC_NO_NEW_PRIVS_CTL)`.

### `SessionServices` — host service integration

Guarantee: optional, degradable host niceties; absence must never fail a transaction.

- Transaction inhibitor (block sleep/shutdown); privileged device provisioning; early-boot
  activation hook registration

Linux: logind Inhibit over D-Bus; socket-activated device broker (`CAP_MKNOD`); dracut module.
FreeBSD: no-op inhibitor (or rc shutdown hook); devfs rulesets; rc.d script.

## 6. High-risk areas

1. **R1 — Rootless sandboxing has no FreeBSD analog** *(architectural)*. The whole build model
   assumes unprivileged user namespaces (`idmap.rs`, `newgidmap`, `/etc/subgid`). Jails require
   root. Decision needed: root-only builds, setuid helper, or privileged daemon. The idmap layer
   gets deleted on FreeBSD, not ported.
2. **R2 — Atomic activation is built on `RENAME_EXCHANGE`** *(architectural, most safety-critical
   code in the tree)*. The `/usr` hot-swap, transition journal, and startup crash-recovery lean on
   atomic exchange + fsync ordering. FreeBSD's idiomatic substitute is ZFS boot environments —
   stronger, but it inverts the design (activation = BE promotion, not directory swap). Put that
   choice behind `NativeActivationStrategy`, not `PlatformFs`, and re-derive each crash matrix.
3. **R3 — Security-posture parity is impossible 1:1.** Seccomp is a hand-built x86_64-only BPF
   filter; Capsicum is a different model entirely (fd capabilities, not syscall filtering). The
   Sandbox trait must state per-platform guarantees honestly.
4. **R4 — Native boot stack is single-vendor.** blsforme fuses "compute what entries should exist"
   (portable) with "write them for systemd-boot" (Linux) — no seam today. The newer
   `active_reblit_*` renderer family partially creates one; put the native-deployer boundary there.
   Dracut early activation (`misc/boot/cast-fstx.*`) must be redesigned for loader/rc.
5. **R5 — procfs-as-capability idiom hides in "neutral" code.** `/proc/self/fd` anchors SQLite,
   downloader targets, and O_PATH re-opens across db/repository/installation. Each site needs the
   `PlatformFs` re-open capability; fdescfs is not mounted by default on FreeBSD.
6. **R6 — Test substrate is Linux-shaped end to end.** ~100 make suites (many named `linux-*`),
   `systemd-run` fixtures, procfs-stat-parsing harness scripts, Ubuntu-only CI. Budget a FreeBSD CI
   substrate as its own workstream, done *first*.
7. **R7 — Long tail of sharp ABI details.** glibc-only `__errno_location` in `config`; raw
   `SYS_openat2`/`SYS_renameat2`/`SYS_getrandom` in three independent crates; `execveat` on O_PATH
   in mason (`fexecve` differs); `f_type` magic numbers throughout. Individually trivial,
   collectively weeks.

## 7. Effort estimates and sequencing

| Workstream | Size | Rough effort | Notes |
|---|---|---|---|
| P0 · cfg scaffolding, CI matrix stub, FreeBSD cross-check build | S | 1–2 wk | Makes coupling visible as compile errors |
| P1 · OS-primitive traits; relocate linux_fs/signal and create native-deployer boot seams | L | 6–10 wk | No generic `cast.fstx` or exchange requirement; zero Linux behavior change |
| P2 · Carve `cast-core` and `cast-deploy-native` out of Forge | L–XL | 8–12 wk | Package planning stays core; R2 activation/journal policy stays native |
| P3 · Shallow shims: config, gluon_config, gitwrap, cast bin | S | 1–2 wk | errno, openat2, renameat2 call sites |
| P4 · mason split (executor behind Sandbox trait) | M | 3–5 wk | Planner/analysis untouched |
| F1 · FreeBSD fs primitives + disk topology + evidence | L–XL | 2–4 mo | Contract follows the D0 activation decision without embedding its policy |
| F2 · FreeBSD sandbox backend (jails/nullfs/devfs/rctl) | XL | 3–6 mo | Includes the R1 privilege-model decision and security-posture doc (R3) |
| F3 · FreeBSD native activation + boot adapter | L | 4–8 wk | Concrete mechanism and activation effect are fixed by D0 evidence |
| F4 · FreeBSD test/CI substrate | M–L | 4–8 wk | Do early — before F1, not after |

**Sequencing:** D0 → P0 → P1 → (P3 ∥ P4) → P2, keeping Linux behavior byte-identical throughout
(existing suites stay green — P1 is a pure relocation). On the FreeBSD side: F4 first, then a
**read-only milestone** (query/resolve/fetch/install-into-image-root with sandbox and boot stubbed
via capability probes) before F1/F2, F3 last.

Totals: **~4–6 engineer-months** for the split, **~8–12 more** to a first functional FreeBSD
backend — with R1 and R2 decided up front, since both change trait contracts, not just
implementations.

## 8. Executable gates and the bootc handoff

All implementation and validation goes through the root Makefile. D0 records two decisions before
P1: the FreeBSD build privilege model (R1), and one native activation/boot strategy (R2), including
its exact `ActivationEffect`, storage prerequisites, recovery path, and rejected alternatives. No
later workstream may silently substitute bectl, directory exchange, or another mechanism.

Each workstream adds its named target before it can be called complete:

| Workstream | Required Make gate | Done evidence |
|---|---|---|
| P0 | `make bsd-cfg-ci-test` | Linux and FreeBSD cfg/feature graph; unsupported pairs fail at compile/admission boundaries |
| P1 | `make bsd-platform-contract-test` | only primitive traits in `cast-platform`; Linux behavior preserved; capability negatives covered |
| P2 | `make bsd-core-native-boundary-test` | core owns exact closure/frozen-root composition; native adapters own state/journal/activation/boot; forbidden dependency checks pass |
| P3 | `make bsd-portability-shim-test` | config, Gluon, git, errno, descriptor re-open, and rename shims pass on both CI hosts |
| P4 | `make bsd-sandbox-contract-test` | Mason uses only Sandbox API; both implementations prove their documented capability and failure modes |
| F4 | `make freebsd-ci-contract-test` | maintained FreeBSD runner executes host-safe gates and archives version/capability evidence |
| F1 | `make freebsd-platform-test` | filesystem, topology, runtime-evidence, durability, and negative-confinement cases pass |
| F2 | `make freebsd-sandbox-test` | jail/nullfs/devfs/rctl lifecycle, cleanup, privilege, and security-difference cases pass |
| F3 | `make freebsd-native-boot-harness-test` | selected strategy renders and validates install/update/rollback/recovery without touching the host boot disk |

Add `make bsd-split-test` as the host-safe P0-P4 aggregate and `make freebsd-host-test` as the
host-safe F1-F4 aggregate. Keep `make freebsd-native-vm-campaign` explicit and destructive: it must
require a disposable VM/disk identity and prove firmware-to-userspace install, activation, reboot,
update, rollback, interrupted transition, and recovery. It must never be a dependency of `make
test` or `make verify`. At every workstream, also run `make check`, `make test`, and `make verify` on
Linux; on the FreeBSD runner run every target that is admitted there.

The implementation creates `plans/bsd-acceptance.md` as an append-only handoff manifest containing
D0 outcomes, accepted commit SHA, actual crate/API/dependency map, OS/toolchain versions, every Make
result, CI/VM evidence locations, known capability differences, and the exact FreeBSD activation
effect. `bsd.md` is complete only when P0-P4 and F1-F4 are checked in that manifest and both the
host-safe aggregates and VM campaign pass. Only then may `bootc.md` Phase 0 re-baseline against that
SHA. OCI is a transport format, not a way to make bootc support FreeBSD.
