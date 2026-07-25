# Full-System Cast: Beginner Primer, bootc Comparison, and Plan

**Status:** Blocked on [`bsd.md`](bsd.md); architecture study and later phased plan
**Priority:** Runs only after the entire accepted `bsd.md` plan and handoff manifest
**Effort:** XL for one production backend; XXL for two
**Risk:** High: boot, persistent data, recovery, and trust are involved
**Planned against:** `feature/feature_plan` at `c6e75d17`
**Execution baseline:** TBD; record the accepted post-`bsd.md` SHA and API map before Phase 0
**Audit date:** 2026-07-25
**bootc documentation baseline:** v1.16.4; recheck before implementation

Sections 1-9 teach normal Linux boot, current Cast, bootc, and the choices between them. Sections
10-14 retain the architecture and implementation reference. This document does **not** decide that
Cast must maintain two deployment engines or claim that proposed behavior exists.

**Ordering constraint:** implement [`bsd.md`](bsd.md) first. This plan must consume its accepted
`cast-core`, platform, sandbox, and native-deployer boundaries and must not create a competing
portability abstraction.

Labels: **CURRENT** means observed in this checkout; **UPSTREAM** means documented bootc behavior;
**PROPOSED** is unimplemented Cast design; **OPEN** requires a decision before dependent work.

## 1. TL;DR — read this first

The confusing part is that “build a package,” “compose packages,” “compose a system,” “deploy,” and
“boot” are five different jobs. Cast currently implements the first two and performs a specialized
`/usr` deployment. bootc starts later: it accepts an already composed Linux image and manages that
image as bootable deployments.

```text
+-----------------------------+
| 1. BUILD ONE PACKAGE        |
| source + recipe -> .stone   |
+--------------+--------------+
               |
               v
+-----------------------------+
| 2. COMPOSE A PACKAGE SET    |
| requests -> exact closure   |
|          -> complete /usr   |
+--------------+--------------+
               |
               v
+-----------------------------+
| 3. COMPOSE A FULL SYSTEM    |
| /usr + kernel + boot +      |
| config/data/lifecycle rules |
+--------------+--------------+
               |
               v
+-----------------------------+
| 4. DEPLOY TO A MACHINE      |
| install/stage on host disk  |
+--------------+--------------+
               |
               v
+-----------------------------+
| 5. ACTIVATE AND PROVE       |
| live swap or reboot, health |
+-----------------------------+
```

Where the projects stop today:

```text
+------------------------+     +------------------------+
| CURRENT CAST           |     | UPSTREAM bootc         |
|                        |     |                        |
| builds Stone packages  |     | does not build/solve   |
| resolves package sets  |     | packages               |
| creates a whole /usr   |     |                        |
| swaps /usr live        |     | consumes a whole Linux |
| publishes boot entries |     | image and stages it    |
|                        |     | as boot deployments    |
| NOT a whole-OS model   |     | NOT a system composer  |
+------------------------+     +------------------------+
```

The missing middle is **full-system composition**: decide exactly what the release owns, including
kernel, initramfs (temporary early-boot userspace), boot assets, service/user declarations,
configuration defaults, persistent-data rules, identity, signing, update, rollback, and health.

“rootc” was shorthand for a possible **whole Cast-owned full-system path**: shared composition plus
native deployment. There is no `rootc` program or `/rootc` path today. This plan calls that idea the
**Cast-native full-system path** and keeps one public product, `cast`. It would reuse Stone and
Cast's platform work, but Cast would maintain deployment, installer, boot, and recovery machinery
that bootc supplies for Linux. bootc offers trust-policy integration points—not signing, promotion,
or a health service.

```text
                         +---------------------------+
                         | CAST SYSTEM INTENT        |
                         | packages + system policy  |
                         +-------------+-------------+
                                       |
                                       v
                         +---------------------------+
                         | LOCKED FULL-SYSTEM PLAN   |
                         | exact, backend-neutral    |
                         +-------------+-------------+
                                       |
                    +------------------+------------------+
                    |                                     |
                    v                                     v
       +---------------------------+         +---------------------------+
       | NATIVE DEPLOYMENT         |         | bootc DEPLOYMENT          |
       | Linux or FreeBSD          |         | Linux only                |
       | Cast owns deployment      |         | bootc deploys the image   |
       +---------------------------+         +---------------------------+
```

**Recommended order:** finish and accept [`bsd.md`](bsd.md), because that defines neutral core,
platform primitives, sandboxes, and native deployment adapters. Then build one safe Linux bootc
image spike. Choose the production Linux backend from evidence, not from naming. FreeBSD remains
Cast-native because bootc is Linux-only.

## 2. The five jobs, from first principles

### 2.1 Package construction: make one reusable artifact

A package recipe says how to turn named inputs into one package. Mason evaluates the Gluon or Lua
declaration, freezes inputs and build policy, runs the build in isolation, analyzes the outputs, and
emits a `.stone` archive.

```text
+----------------------+     +----------------------+     +----------------------+
| PACKAGE DECLARATION  |     | MASON BUILD SANDBOX  |     | ONE STONE ARTIFACT   |
| sources, versions,   | --> | compile, install to  | --> | metadata + layout +  |
| dependencies, phases |     | temporary build root |     | content identities   |
+----------------------+     +----------------------+     +----------------------+
```

The build sandbox contains compilers and temporary dependencies. It is not the machine's future
root filesystem. A Stone is one reusable input, not a bootable operating system.

### 2.2 Package-set composition: combine exact packages without file conflicts

“Package composition” in this document means resolving requested packages and all transitive
dependencies into one exact closure, then materializing their files into a coherent `/usr` tree.
It is not merely concatenating archives: providers are selected, identities are checked, and two
packages that claim incompatible versions of the same path must fail.

```text
+------------------+     +------------------+     +------------------+     +------------------+
| HUMAN REQUEST    | --> | EXACT CLOSURE    | --> | VERIFY + PLAN    | --> | COMPOSED /usr    |
| install/remove/  |     | package IDs, all |     | hashes and safe  |     | one complete     |
| sync + requests  |     | dependencies     |     | path conflicts   |     | release tree     |
+------------------+     +------------------+     +------------------+     +------------------+
```

Cast does this today. Install, remove, and sync begin with different human requests but converge on
a new exact selection and a newly materialized whole `/usr`; Cast does not patch the old tree one
file at a time.

### 2.3 System composition: define a complete bootable release contract

Package-set composition answers “which files are in `/usr`?” System composition answers “what is
the complete OS release, how can it boot, what may vary per machine, and what survives rollback?”

```text
+---------------------------+       +--------------------------------------+
| PACKAGE-SET OUTPUT        |       | SYSTEM-WIDE INPUTS                   |
| exact /usr tree           |       | kernel + initramfs + command line    |
| package identities        |       | boot assets and root/storage rules   |
+-------------+-------------+       | services, users, config defaults     |
              |                     | /var ownership + migration contract  |
              |                     | signatures, update, health, recovery |
              |                     +------------------+-------------------+
              |                                        |
              +--------------------+-------------------+
                                   v
                     +--------------------------------+
                     | COMPOSED SYSTEM RELEASE        |
                     | artifact ID bound to the plan  |
                     | and each declared lifecycle    |
                     +--------------------------------+
```

Composition itself does not modify a host. Its output may be a native Cast release, an Open
Container Initiative (OCI) image, or another exact artifact. A sound model first records intent,
then resolves it into an immutable locked plan, and only then lowers that plan to a backend format.

### 2.4 Deployment: make one release bootable on one host

Deployment imports or installs a composed release onto a machine, records its identity, writes or
updates boot metadata, and should retain enough previous state for recovery.

```text
+----------------------+     +----------------------+     +----------------------+
| SYSTEM RELEASE       | --> | HOST DEPLOYMENT      | --> | BOOTABLE CHOICE      |
| native or OCI        |     | local store + /etc   |     | loader entry +       |
| immutable identity   |     | and /var attachment  |     | kernel/initramfs     |
+----------------------+     +----------------------+     +----------------------+
```

### 2.5 Activation and health: choose what actually runs

Activation is not the same as preparation. Cast can expose a new `/usr` in the current boot; bootc
normally stages a deployment and makes it active on reboot. Neither fact proves application health.

```text
+-----------+     +-----------+
| COMPOSED  | --> | PREPARED  |
| no host   |     | on disk   |
+-----------+     +-----+-----+
                        |
                 +------+------+
                 |             |
                 v             v
       +------------------+  +------------------+     +------------------+
       | TreeExposedNow   |  | AwaitingReboot   | --> | BootedUnverified |
       | restart/reboot   |  +------------------+     +--------+---------+
       | requirements     |                                  |
       +------------------+                                  v
                                                    +------------------+
                                                    | BootedHealthy    |
                                                    +------------------+
```

“Update succeeded” must name one of these states. Downloaded, staged, booted, and healthy are not
synonyms.

### 2.6 One concrete example: add `curl`

```text
+--------------------------+  +--------------------------+  +--------------------------+
| CURRENT CAST             |  | FUTURE CAST-NATIVE      |  | bootc                     |
| resolve curl closure     |  | compose a new native     |  | rebuild image with curl  |
| make complete new /usr   |  | full-system release      |  | publish new OCI digest   |
| exchange /usr live       |  | deploy by OS strategy    |  | stage it, then reboot    |
+--------------------------+  +--------------------------+  +--------------------------+
```

The same package decision can therefore lead to very different host-update semantics.
With pure bootc, persistent production-host `cast install curl` becomes an intent/image-CI change:
rebuild, publish, stage, and reboot rather than mutate the base OS locally.

## 3. Filesystem and boot background

### 3.1 `/`, `/root`, `/usr`, and “rootc” are different words

`/` is the root of the filesystem namespace. `/root` is merely the root user's home directory.
There is no special `/rootc` directory in this design. `rootc` was only a possible nickname for the
whole Cast-native full-system path. BLS below means Boot Loader Specification.

```text
+--------------------------------------------------------------------------+
| ROOT FILESYSTEM: /                                                       |
|                                                                          |
| /usr   release programs, libraries, units, shared data                   |
| /etc   machine configuration and local overrides                         |
| /var   changing data: databases, logs, queues, container state           |
| /boot  loader-visible kernel/initramfs/BLS assets, depending on layout   |
| /run   volatile state recreated for this boot                            |
| /home  ordinary user data                                                |
| /root  root user's home; it is NOT the root filesystem                   |
+--------------------------------------------------------------------------+
```

On a usr-merged Linux system, top-level compatibility paths point into `/usr`:

```text
+------------+       +------------+
| /bin       | ----> | /usr/bin   |
+------------+       +------------+
+------------+       +------------+
| /sbin      | ----> | /usr/sbin  |
+------------+       +------------+
+------------+       +------------+
| /lib*      | ----> | /usr/lib*  |
+------------+       +------------+
```

Replacing `/usr` therefore replaces most userspace programs and libraries, but it does **not** by
itself replace `/etc`, `/var`, the already running kernel, or code already mapped into processes.

### 3.2 The normal Linux boot handoff

EFI is the common modern firmware interface. PID 1 is the first userspace process; on these systems
it is normally systemd.

```text
+----------------------+     +----------------------+     +----------------------+
| FIRMWARE             | --> | BOOTLOADER + ENTRY   | --> | LINUX KERNEL         |
| power-on, choose EFI |     | choose kernel,       |     | initialize hardware, |
| executable           |     | initramfs, arguments |     | run initramfs /init  |
+----------------------+     +----------------------+     +-----------+----------+
                                                                       |
                                                                       v
+----------------------+     +----------------------+     +----------------------+
| NORMAL SYSTEM        | <-- | switch_root          | <-- | INITRAMFS            |
| systemd PID 1,       |     | hand final root to   |     | unlock storage,      |
| services and login   |     | real PID 1           |     | select/mount OS root |
+----------------------+     +----------------------+     +----------------------+
```

The bootloader does not run the OS, the initramfs is not the final OS, and bootc is not PID 1. Cast
and bootc influence these handoffs.

### 3.3 One possible disk view

ESP means EFI System Partition.

```text
+----------------------------------------------------------------------------+
| PHYSICAL DISK                                                              |
+--------------------+-----------------------+-------------------------------+
| ESP                | optional /boot        | root/storage area             |
| EFI executables    | BLS, kernel/initramfs | OS store, /etc, /var, history |
| firmware-readable  | bootloader-readable   | exact layout is backend rule  |
+--------------------+-----------------------+-------------------------------+
```

Encryption, disk arrays, volume managers, ZFS, network roots, legacy BIOS, and Unified Kernel Images
change the concrete layout. System composition states requirements; the installer/deployer realizes
them.

## 4. Current Cast: package construction and package-set composition

### 4.1 Which component does what

```text
                              +----------------------+
                              | PUBLIC CLI: cast     |
                              +----------+-----------+
                                         |
                    +--------------------+--------------------+
                    |                                         |
                    v                                         v
       +---------------------------+             +---------------------------+
       | MASON LIBRARY             |             | FORGE LIBRARY             |
       | recipe -> build -> Stone  |             | resolve/cache/compose     |
       | package construction      |             | states, activation, boot  |
       +---------------------------+             +---------------------------+
```

Mason and Forge are libraries behind one public command, not competing products.

### 4.2 What a Stone contains

```text
+----------------------------------------------------------------------------+
| package.stone                                                              |
+----------------+-----------------------------------------------------------+
| metadata       | name/version/arch, dependencies, source/build provenance  |
| layout         | /usr-relative path, inode type, mode, owner, content ref  |
| content        | file bytes addressed by content identity                  |
| format/index   | bounded lookup and extraction records                     |
+----------------+-----------------------------------------------------------+
```

Stone provenance, repository archive SHA-256, and per-file content digests are distinct. Forge adds
`/usr/` once and rejects escapes, conflicts, reserved metadata, char/block devices, FIFOs, sockets,
and unknown types. Hash identity alone does not prove an approved publisher signed it.

### 4.3 How the new tree is produced

```text
+----------------------+     +----------------------+     +----------------------+
| PACKAGE REQUEST      | --> | RESOLVER             | --> | DEPENDENCY GRAPH     |
| install/remove/sync  |     | repos + providers +  |     | explicit + deps      |
|                      |     | dependency rules     |     | selections           |
+----------------------+     +----------------------+     +----------+-----------+
                                                                       |
                                                                       v
+----------------------+     +----------------------+     +----------------------+
| CANDIDATE /usr       | <-- | MATERIALIZER         | <-- | VERIFIED STONES      |
| ordinary independent |     | virtual path tree    |     | expected archive and |
| files + directories  |     | -> one complete tree |     | asset digests        |
+----------------------+     +----------------------+     +----------------------+
```

The stateful materializer uses independent ordinary file copies for the active tree; `/usr` is not
merely a set of hardlinks into an immutable package cache.

### 4.4 What a Cast state records—and what it does not

```text
+----------------------------------+     +----------------------------------+
| CAST STATE N RECORDS             |     | NOT FIRST-CLASS STATE TODAY      |
| local sequential state ID        |     | complete service/user intent     |
| summary and timestamp            |     | kernel/mount/storage policy      |
| exact explicit package IDs       |     | versioned /etc or /var schema    |
| exact transitive package IDs     |     | secrets/provisioning/health      |
+----------------------------------+     +----------------------------------+
```

`/etc/cast/system.glu` currently covers warning policy, repositories, and package/provider strings;
its generated snapshot is `/usr/lib/system-model.glu`. That is useful package-system intent, but it
is not yet a backend-neutral, complete OS specification.

ActiveReblit separately consumes a typed machine-local root locator, EFI System Partition (ESP) or
extended boot loader (XBOOTLDR) topology, kernel command line, and mounted-topology evidence. Those
inputs govern boot publication; they are not versioned release intent, installer policy, or a
complete system model.

## 5. Exactly how current Cast changes `/usr`

This walkthrough covers legacy fresh NewState (install/remove/sync) and archived activation. Active
verification instead uses the durable ActiveReblit coordinator: same exchange primitive, but
different recovery and residue/archive rules.

### 5.1 The on-disk players

```text
+--------------------------------------------------------------------------+
| INSTALLATION ROOT                                                        |
|                                                                          |
| /usr/                         live tree N; contains .stateID             |
| /etc/                         one retained machine-local tree            |
| /var/                         ambient; not a Cast-managed contract       |
| .cast/assets/v2/<digest>      verified package-content cache             |
| .cast/root/staging/usr/       complete candidate, then old tree          |
| .cast/root/<state-id>/usr/    archived rollback trees                    |
| .cast/quarantine/             retained failure/recovery evidence         |
+--------------------------------------------------------------------------+
```

Frozen composition is different: `materialize_frozen_root` consumes a caller-supplied complete list
of exact archive identities; it neither solves dependencies nor adds missing ones. It creates `/usr`
but no state, triggers, `/etc`, `/var`, or boot work, so it is not a bootable image by itself.

### 5.2 Prepare the replacement without touching live `/usr`

```text
+----------------------------+           +----------------------------+
| LIVE NAME                  |           | PUBLISHED STAGING NAME     |
| /usr                       |           | .cast/root/staging/usr     |
|                            |           |                            |
| complete old tree N        |           | complete new tree N+1      |
| .stateID = N               |           | .stateID = N+1             |
+----------------------------+           +----------------------------+
```

Cast resolves/verifies the closure and builds the candidate under a random private
`.cast-usr-<random>.tmp`, then publishes it no-replace as fixed `staging/usr`. It writes state/model
metadata and runs transaction triggers with candidate `/usr` writable and `/etc` read-only.

### 5.3 One atomic directory-entry exchange

Linux `renameat2(RENAME_EXCHANGE)` exchanges the names in one syscall; both parents must be on the same filesystem:

```text
BEFORE THE SINGLE EXCHANGE
+----------------------------+           +----------------------------+
| /usr                       |           | .cast/root/staging/usr     |
| points to OLD TREE N       |           | points to NEW TREE N+1     |
+----------------------------+           +----------------------------+
                 \                                      /
                  +---------- atomic exchange ----------+
                 /                                      \
AFTER THE SINGLE EXCHANGE
+----------------------------+           +----------------------------+
| /usr                       |           | .cast/root/staging/usr     |
| points to NEW TREE N+1     |           | points to OLD TREE N       |
+----------------------------+           +----------------------------+
```

This replaces the **whole directory tree visible at `/usr`**, not only the files belonging to the
newly requested package. There is no instant where `/usr` names half of each tree.

Afterward Cast maintains `/bin`, `/sbin`, and `/lib*` links into `/usr`, runs system triggers with
live `/usr` and retained `/etc` writable, archives the displaced old tree under its state ID, and
synchronizes boot assets/entries.

### 5.4 What changed immediately, and what did not

```text
+--------------------------------------+  +--------------------------------------+
| FRESH LOOKUPS VIA /usr SEE N+1       |  | STILL RUNNING OLD EXECUTION STATE    |
| exec a new /usr/bin/tool             |  | kernel and already-used initramfs    |
| open a not-yet-opened library/file   |  | mapped executable/library pages      |
| start a new service process          |  | existing daemon and shell processes  |
+--------------------------------------+  +--------------------------------------+
```

A running process does not transform into the new executable. Fresh lookup beginning through public
`/usr` sees N+1, but an old cwd, directory fd, open fd, or mapping can retain/traverse tree N. A
future full-system API must report restart/reboot requirements; current Cast has no generic service
restart set. “The whole running system changed atomically” would be false.

The exchange itself is atomic, but the entire transaction is not one indivisible syscall. Triggers,
database records, archiving, and boot publication happen around it. Arbitrary external effects from
a trigger cannot be undone merely by exchanging `/usr` back.

### 5.5 `/etc`, `/var`, and rollback remain separate

```text
+----------------+     +----------------+     +----------------+
| STATE N        | --> | STATE N+1      | --> | ROLLBACK TO N  |
| /usr = N       |     | /usr = N+1     |     | /usr = N       |
+----------------+     +----------------+     +----------------+
          |                    |                     |
          +--------------------+---------------------+
                               v
             +--------------------------------------+
             | SAME retained /etc; SAME ambient     |
             | /var; neither follows /usr history   |
             +--------------------------------------+
```

- `/etc` is one machine-local tree. Transaction triggers see it read-only; system triggers see it
  read-write. Cast does not version, merge, or roll it back.
- `/var` is not exchanged, but current Cast also does not model, snapshot, migrate, or promise its
  compatibility. “Ambient/unmanaged” is more accurate than “Cast-persistent.”
- Static `tmpfiles.d` and `sysusers.d` declarations can live in `/usr`; runtime directories and data
  are created later and need an explicit full-system lifecycle contract.

### 5.6 Booting or rolling back a Cast state

Legacy boot sync considers the active ID plus up to four rollback IDs and emits BLS entries only for
eligible roots with discoverable kernels; it may emit multiple entries per state or no-op when no
recognized boot assets/usable entries exist. Each emitted entry carries `cast.fstx=<state-id>`.

```text
+----------------------+     +----------------------+     +----------------------+
| BOOT ENTRY FOR N     | --> | KERNEL + INITRAMFS   | --> | MOUNT PHYSICAL ROOT  |
| contains cast.fstx=N |     | selected by loader   |     | as /sysroot          |
+----------------------+     +----------------------+     +----------+-----------+
                                                                       |
                                                        +--------------+-------------+
                                                        |                            |
                                                        v                            v
                                           +----------------------+     +----------------------+
                                           | /usr IS ALREADY N    |     | /usr IS ANOTHER ID   |
                                           | continue switch_root |     | initramfs runs Cast  |
                                           +----------------------+     | to exchange N live   |
                                                                        +----------+-----------+
                                                                                   |
                                                                                   v
                                                                        +----------------------+
                                                                        | switch_root, PID 1   |
                                                                        +----------------------+
```

Thus Cast rollback mutates the single physical root's `/usr` during initramfs. It is not yet a set
of independent immutable roots. If IDs differ, the script requires Cast in the initramfs and runs
`cast -D /sysroot state activate -y --skip-triggers <id>`: both trigger scopes are skipped, boot sync
is not, and prior trigger effects in `/etc` or elsewhere are neither replayed nor reversed.

### 5.7 Honest maturity boundary

| Capability | Current status |
|---|---|
| Stone build/index/fetch/hash checks, dependency transactions | implemented |
| whole `/usr` staging/exchange, triggers, legacy BLS/initramfs rollback | implemented |
| ActiveReblit coordinator | production-dispatched; complete live-client Ready-branch regression missing |
| durable coordinator as default for fresh NewState | **not default** |
| complete startup boot repair and real power-loss evidence | incomplete/open |
| typed full-system composition | not implemented |

`FUTURE_PLAN.md` tracks those gaps. Coordinator scaffolding does not prove every ordinary transition
has bootc-grade durability.

## 6. What a full-system Cast—or “rootc”—would add

A full system assigns every resource an owner, lifecycle, identity, and activation rule:

```text
+----------------------------------------------------------------------------+
| LOCKED FULL-SYSTEM PLAN                                                    |
|                                                                            |
| RELEASE-OWNED       exact packages, binaries, units, kernel modules        |
| BOOT-OWNED          kernel, initramfs/Unified Kernel Image, loader entries |
| MACHINE DEFAULTS    config shipped by release but locally overridable      |
| PERSISTENT DATA     owners, migrations, N/N-1 compatibility, backup        |
| SECRETS             references and injection; never public artifact bytes  |
| RUNTIME EPHEMERAL   /run, sockets, recreated state                         |
| DELIVERY            identity, signatures, channel, health, retention       |
+----------------------------------------------------------------------------+
```

The hypothetical “rootc” is not “Cast but with a cooler name” and not the current `/usr` swap. It
would be the Cast-owned implementation of those contracts:

```text
+---------------------------+     +---------------------------+
| CURRENT CAST              | --> | CAST-NATIVE FULL SYSTEM   |
| Stone + exact /usr state  |     | exact native release      |
| one local /etc            |     | explicit /etc policy      |
| ambient /var              |     | explicit /var contract    |
| partial boot/recovery     |     | installer + boot + trust  |
|                           |     | proven update/recovery    |
+---------------------------+     +---------------------------+
```

Its `/usr` behavior is still **OPEN**; a name cannot decide between two real designs:

```text
+------------------------ NATIVE OPTION 1 -------------------------+
| Extend today's live whole-/usr exchange.                         |
| Report exact service-restart and reboot requirements.            |
| Finish durability, boot repair, /etc, and /var contracts.        |
+------------------------------------------------------------------+
                                OR
+------------------------ NATIVE OPTION 2 -------------------------+
| Prepare an independent complete deployment or boot environment.  |
| Select the whole release at reboot, more like bootc semantics.   |
| Cast still owns the storage, boot, recovery, and evidence stack. |
+------------------------------------------------------------------+
```

FreeBSD uses the strategy proven by `bsd.md`; Linux may choose either later. Therefore nobody can
yet truthfully say “rootc changes `/usr` this way.” Shared intent need not mean identical effects.

A backend that cannot implement a declared lifecycle returns a typed unsupported capability rather
than silently approximating it. The public CLI can remain `cast`; creating another executable named
`rootc` would not solve any architecture problem.

## 7. What bootc actually does

### 7.1 bootc starts after system composition

bootc consumes a complete Linux OS image. A Containerfile, distro package workflow, or future Cast
composer must decide and build the image first.

```text
+---------------------------+     +---------------------------+
| SYSTEM IMAGE BUILDER      | --> | BOOTC-COMPATIBLE OCI      |
| packages + root defaults  |     | root tree + kernel        |
| kernel + units + metadata |     | exact manifest digest     |
+---------------------------+     +-------------+-------------+
                                                |
                                                v
                                  +---------------------------+
                                  | PULL / IMPORT / STAGE     |
                                  | local boot deployment     |
                                  +-------------+-------------+
                                                |
                                                v
                                  +---------------------------+
                                  | REBOOT INTO NORMAL LINUX  |
                                  | systemd is ordinary PID 1 |
                                  +---------------------------+
```

The installed OS is not a Docker/Podman application container around PID 1. Container runtime
metadata such as `ENTRYPOINT`, `CMD`, `ENV`, `USER`, `EXPOSE`, and Docker `HEALTHCHECK` is not the
deployed-host contract; persistent behavior belongs in filesystem content, units, and provisioning.

The stable image contract includes `/sysroot`, label `containers.bootc=1`, a kernel at
`/usr/lib/modules/$kver/vmlinuz`, and distro initramfs/boot integration. On the split-kernel path,
normal payload does not belong in image `/boot`; bootc publishes the required boot files.

Package changes normally happen when building image N+1. There is no persistent `bootc install
<package>` operation. `bootc usroverlay` is a temporary development overlay that disappears on
reboot and cannot replace the running kernel. Persistent rpm-ostree layering is a separate hybrid
authority, not the pure bootc model proposed here.

### 7.2 Installation and local storage

OSTree is a content-addressed filesystem-tree and deployment store. This plan uses its stable
deployment path, with composefs recommended for mounting the read-only image root.

```text
                    +---------------------------+
                    | OCI IMAGE + TARGET DISK   |
                    +-------------+-------------+
                                  |
                                  v
                    +---------------------------+
                    | bootc install             |
                    +-------------+-------------+
                                  |
                                  v
+------------------------------------------------------------------+
| LOCAL INSTALL RESULT                                             |
| bootc -> bootupctl backend install; bootable OSTree deployment   |
| recorded OCI origin                                              |
+------------------------------------------------------------------+
```

`to-disk` is opinionated; `to-filesystem` lets another installer own storage layout. Complex
encryption, RAID/LVM, cloud-image production, and installer UX remain separate work. `bootc install`
invokes `bootupctl backend install`; normal `bootc upgrade` does not run separate day-two
`bootupctl update` automatically.

“A/B” is logical, not two required root partitions.

```text
+---------------------------+     +---------------------------+
| OCI MANIFEST + LAYERS     | --> | LOCAL OSTREE IMAGE TREE   |
+---------------------------+     +-------------+-------------+
                                                |
                               +----------------+----------------------+
                               |                                       |
                               v                                       v
                 +---------------------------+           +---------------------------+
                 | BOOTED DEPLOYMENT N       |           | STAGED / ROLLBACK         |
                 | current root and kernel   |           | inactive choices          |
                 +---------------------------+           +---------------------------+
```

The OCI digest, imported OSTree commit, and local deployment ID are related but distinct identities.
The OS store is not simply the application-image store used by Podman.

### 7.3 Upgrade, reboot, and rollback

```text
+----------------------+     +----------------------+     +----------------------+
| BOOTED N             | --> | STAGE N+1            | --> | SHUTDOWN FINALIZE    |
| N still runs         |     | pull/import; N runs  |     | prepare /etc + boot  |
+----------------------+     +----------------------+     +----------+-----------+
                                                                       |
                                                                       v
+----------------------+     +----------------------+     +----------------------+
| HEALTHY N+1          | <-- | BOOTED N+1           | <-- | REBOOT SELECTS N+1   |
| external policy OK   |     | initially unverified |     | new root + kernel    |
+----------------------+     +----------------------+     +----------------------+
```

`bootc upgrade` fetches and stages; `--download-only` separates download from application;
`--from-downloaded` later unlocks that exact download. `bootc switch` changes the tracked image
reference. `bootc rollback` changes boot ordering so reboot selects the previous deployment. Use
`bootc status --format=json --format-version=1` for automation and `--verbose` for humans.

bootc/OSTree exposes deployment and limited finalization/boot-completion evidence, not application
or policy health. A controller must observe the new boot and decide whether it is healthy. A moving
tag is a channel; an exact digest is immutable content identity, not publisher authorization.

### 7.4 `/etc` and `/var` under bootc

```text
+----------------------+     +----------------------+     +----------------------+
| OLD IMAGE DEFAULTS   |     | LOCAL ADMIN DELTA    |     | NEW IMAGE DEFAULTS   |
+----------+-----------+     +-----------+----------+     +-----------+----------+
           |                             |                            |
           +-----------------------------+----------------------------+
                                         |
                                         v
                              +---------------------------+
                              | NEW DEPLOYMENT /etc       |
                              | OSTree three-way result   |
                              +---------------------------+

+----------------------+     +----------------------+     +----------------------+
| DEPLOYMENT N         |     | DEPLOYMENT N+1       |     | ROLLBACK N           |
+----------+-----------+     +-----------+----------+     +-----------+----------+
           |                             |                            |
           +-----------------------------+----------------------------+
                                         |
                                         v
                              +---------------------------+
                              | ONE SHARED CURRENT /var   |
                              +---------------------------+
```

Forward update merges local `/etc` changes onto new defaults, normally during shutdown finalization.
Rollback exposes the older deployment's existing `/etc`; it is not a new forward merge. Image
`/var` seeds first install only, and later image changes do not update host `/var`. OS rollback does
not rewind databases; compatibility, migration, backup, and data recovery remain external policy.

### 7.5 Trust and ownership boundaries

```text
+----------------------+-----------------------------------------------------+
| TLS                  | protects registry transport                         |
| registry credentials | authorize pull/push                                 |
| OCI digest           | identifies exact manifest bytes                     |
| image signature      | proves publisher approval under configured policy   |
| Secure Boot          | verifies executable boot-chain artifacts            |
| composefs verity     | can verify immutable filesystem blocks              |
+----------------------+-----------------------------------------------------+
```

These layers do not replace one another. At v1.16.4, `enforce-container-sigpolicy` defaults to false;
merely using bootc does not enforce signatures, and composefs does not authenticate `/etc` or `/var`.

| bootc owns | another system owns |
|---|---|
| pull/import/stage/select/rollback OS deployments | package solving and root-image composition |
| basic install and local deployment status | registry, software bill of materials, provenance/signing |
| local image lifecycle | fleet rollout, application health, secrets, data migrations |

The separate native-composefs storage backend and sealed Unified Kernel Image path are experimental
at this document's v1.16.4 baseline; do not confuse them with stable OSTree deployments mounted
through composefs.

## 8. The same update through all three models

Assume current Cast state A contains `/usr` A and kernel payload K1, while state B contains `/usr` B
and payload K2. Only the proposed native and bootc columns model these as full releases.

```text
+--------------------------- CURRENT CAST ---------------------------+
| +----------------+    +----------------+    +-------------------+  |
| | Stone closure B| -> | stage /usr B   | -> | exchange /usr live|  |
| +----------------+    +----------------+    +---------+---------+  |
|                fresh /usr opens see B; old mapped code remains     |
|                boot sync may publish K2; K1 runs until reboot      |
+--------------------------------------------------------------------+

+-------------------- CAST-NATIVE FULL SYSTEM -----------------------+
| +----------------+    +----------------+    +-------------------+  |
| | locked plan B  | -> | native release | -> | OS adapter applies|  |
| +----------------+    +----------------+    +---------+---------+  |
|              exact effect is platform policy proven by bsd.md      |
+--------------------------------------------------------------------+

+----------------------------- bootc --------------------------------+
| +----------------+    +----------------+    +-------------------+  |
| | OCI B with K2  | -> | stage B; A runs| -> | reboot into B + K2|  |
| +----------------+    +----------------+    +-------------------+  |
+--------------------------------------------------------------------+
```

| Question | Cast today | Cast-native full system | bootc |
|---|---|---|---|
| exists? | yes | proposed | upstream project |
| update unit | complete selected `/usr` | explicit native release | complete image deployment |
| package build/solve | Mason/Forge | Mason/Forge | external; could use Mason/Forge |
| normal activation | live `/usr` exchange | OS-specific, must be explicit | stage then reboot |
| kernel | not changed by exchange | release-owned | image-owned, selected on reboot |
| `/etc` | one local tree | must design | per deployment, forward merge |
| `/var` | ambient/unmanaged | must design | one shared current tree |
| rollback | exchange archived `/usr` | Cast-owned and OS-specific | select prior deployment, reboot |
| ecosystem | Cast-specific | Cast-specific | OCI registry/build/scanning |

The decisive difference is not `.stone` versus OCI. It is **what constitutes a release**, **who
owns the host deployment**, and **whether activation mutates live `/usr` or selects a prepared root
at boot**. “rootc” has no separate behavior; it was only shorthand for the whole proposed
Cast-native full-system path.

## 9. Architecture choices

```text
+------------------------------------------------------------------+
| Must the newly composed Linux /usr be visible before reboot?     |
+--------------------------------+---------------------------------+
| YES: keep live-exchange option | NO: answer the next question    |
+--------------------------------+---------------------------------+
                                   |
                                   v
+------------------------------------------------------------------+
| Are image CI, an OCI registry, and scheduled reboot acceptable?  |
+--------------------------------+---------------------------------+
| YES: bootc is a strong option  | NO: requirements conflict first |
+--------------------------------+---------------------------------+
```

| Choice | Shape | Main benefit | Main cost |
|---|---|---|---|
| A. Linux bootc + native FreeBSD | LockedSystemPlan -> bootc OCI; BSD adapter on FreeBSD | reuse Linux OCI/OSTree deployment stack | Linux becomes image-build/reboot workflow |
| B. native on both | locked plan -> OS-native releases | maximum control; host package flow can remain | Cast owns installer, boot, recovery, trust, `/etc`, `/var` |
| C. both on Linux + native FreeBSD | one plan, two explicit Linux deployers | preserves choice | largest testing, migration, and support burden |

Always reject two runtime owners on one host:

```text
+---------------------------+       +---------------------------+
| CAST OWNS /usr + BOOT     | --X-- | bootc OWNS ROOT + BOOT    |
+---------------------------+       +---------------------------+
```

Mason/Stone can participate at image-build time without owning the bootc-managed host. Choose dual
Linux support only for funded users of both activation models.

## 10. Proposed neutral architecture

Only introduce this layer after `bsd.md` has landed and its acceptance manifest records the real
crate/API map. System composition and deployment remain separate axes:

```text
                       +---------------------------+
                       | GLUON / LUA SYSTEM INTENT |
                       +-------------+-------------+
                                     |
                                     v
                       +---------------------------+
                       | cast-core                 |
                       | LockedSystemPlan + ID     |
                       | exact closure + root      |
                       +-------------+-------------+
                                     |
                 +-------------------+-------------------+
                 |                   |                   |
                 v                   v                   v
      +--------------------+ +--------------------+ +--------------------+
      | NATIVE LINUX       | | NATIVE FREEBSD     | | LINUX bootc        |
      | Cast deployer      | | Cast deployer      | | OCI deployer       |
      +---------+----------+ +---------+----------+ +--------------------+
                |                      |
                v                      v
      +--------------------+ +--------------------+
      | Linux primitives   | | FreeBSD primitives |
      +--------------------+ +--------------------+

      +--------------------------------------------------------------+
      | Mason build execution -> separate platform Sandbox contract  |
      +--------------------------------------------------------------+
```

`cast-core` owns neutral intent, exact Stone closure, and frozen-root composition; it never activates
a host. Native adapters own Cast state/journal/activation/boot. The bootc adapter does not call the
native deployer: bootc/OSTree owns its host deployment. `cast install` remains explicitly native.

```text
Target = { os, arch, abi }
AdmittedTarget = LinuxNative(Target) | FreeBsdNative(Target) | LinuxBootc(Target)
LockedSystemPlan = { plan_id, admitted_target, target-bound Stone closure,
                     lifecycle resources, kernel/data/update policy }
ActivationEffect = TreeExposedNow { restart_required, reboot_required }
                 | AwaitingReboot | BootedUnverified | BootedHealthy
```

| Pair | Result |
|---|---|
| Linux + Cast-native | admitted through Linux native adapter |
| FreeBSD + Cast-native | admitted through FreeBSD native adapter |
| Linux + bootc | admitted through bootc adapter |
| FreeBSD + bootc | typed rejection before network/filesystem effects |

Plain `Ok(())` cannot hide activation differences. Current sequential state ID, semantic plan ID,
native release ID, OCI digest, local deployment ID, and observed boot evidence remain distinct.

The central bootc composition decision is whether Cast overlays an exact Stone `/usr` onto a locked
base digest with explicit conflict/deletion rules, or authors the compatible root itself with a tiny
locked bootstrap. An untracked base would leave undeclared files and is not acceptable.

## 11. Decisions before implementation

| Area | Decision that must be explicit |
|---|---|
| backend | Linux bootc/native/both; reboot policy; whether host-side package installs remain |
| composition | locked base overlay versus Stone-authored root; services/users/kernel/initramfs |
| mutable state | `/etc` policy; `/var` owners, migrations, N/N-1 compatibility, backup/rollback |
| trust/install | Stone and OCI signing, promotion, Secure Boot/verity, storage and bootloader owner |
| operations | channel versus pinned digest, offline retention, reboot/health/rollback-loop controller |
| migration | how `/etc`, `/var`, home, boot assets, and Cast history cross backend boundaries |

## 12. Phased implementation

Use the root Makefile for every build/test lane; each row waits for the preceding proof.

| Phase | Deliverable | Critical proof | Make gate to add |
|---|---|---|---|
| 0 | re-baseline after completed `bsd.md` | `bsd-acceptance.md` has P0-P4/F1-F4, SHA/API map, VM evidence, FreeBSD effect | rerun BSD aggregates + `make verify` |
| 1 | neutral `SystemSpec` and `LockedSystemPlan` | Gluon/Lua parity, canonical ID, target-bound closure, mismatch fails before effects | `make system-plan-test` |
| 2 | explicit Cast-native release adapter | existing Linux semantics and accepted FreeBSD effect preserved; core calls no OS syscall | `make system-native-backend-test` |
| 3 | host-safe Linux bootc image spike | exact plan -> Stone IDs -> inventory -> OCI digest; lint; no secrets or mutable inputs | `make system-bootc-image-test` |
| 4 | typed Linux bootc host adapter | machine-readable status; tag/digest policy; `AwaitingReboot` never means running | `make system-bootc-adapter-test` |
| 5 | trust and publication | approved signatures pass; wrong signer/repo, moved tag, revoked credential fail | `make system-release-policy-test` |
| 6 | disposable-VM lifecycle proof | real install, reboot, `/etc` merge, shared `/var`, rollback, interruption and recovery | harness plus explicit `make system-bootc-vm-campaign` |
| 7 | production-backend ADR | Linux native/bootc/both, owners, minimum version, migration and deprecation named | documentation gate + `make verify` |
| 8 | product CLI and runbooks | exact effects; no “active” for staged; no claim rollback rewound `/var` | public CLI tests + `make examples` |

The destructive VM campaign stays outside `make test`/`make verify` and requires exact guest/disk
identity, confirmation, logs, and recovery. Native Linux production also requires closing NewState,
boot repair, selected-payload bootability, and power-loss gaps in `FUTURE_PLAN.md`.

## 13. Verification, completion, and stops

| Layer | Host-safe proof | Disposable-VM proof |
|---|---|---|
| spec/plan | parity, canonical identity, capability tests | none |
| native Linux/FreeBSD | exact closure plus BSD/native gates | selected payload, activation, reboot/power loss |
| Linux bootc | lint, inventory, signatures, typed status | install, stage, reboot, rollback |
| state/trust/health | lifecycle fixtures and policy negatives | `/etc`/`var`, Secure Boot, failed health/rollback |

Every phase reruns the BSD Make gates plus `make source-loc`, `make check`, `make test`, and `make
verify`; run `make examples` when public examples change. bootc targets are Linux-only. Focused gates
belong in `misc/make/full-system-tests.mk`; default checks require no root, network, disk, or reboot.

**STOP** if any condition holds:

1. `bsd.md` is incomplete/un-re-audited, or parallel platform traits appear.
2. Cast and bootc can both mutate the base OS/boot authority, or APIs hide live versus staged effects.
3. lowering silently changes `/etc`, `/var`, users, kernel, services, or introduces undeclared inputs.
4. tags/digests/signatures are conflated, secrets enter artifacts, or frozen `/usr` is called bootable.
5. same-boot simulation is called reboot proof, host disks are reachable, or dual support has no owner.

## 14. Evidence map and upstream sources

These are pre-`bsd.md` paths; Phase 0 replaces them with landed ownership.

| Current Cast topic | Source |
|---|---|
| Stone authoring/build/records | `crates/stone_recipe/src/package/{authored,builder_lowering}.rs`; `crates/mason/src/{cli/build,package/emit}.rs`; `crates/stone/src/payload/{meta,layout}.rs` |
| exact closure and frozen root | `crates/forge/src/client/{install,remove,sync,fixed_staging}.rs`; `crates/forge/src/client/core/materialization_facade.rs` |
| state and present system schema | `crates/forge/src/{state,system_model/spec}.rs` |
| exchange and previous-tree archive | `crates/forge/src/client/core/{state_planning,stateful_transition}.rs`; `crates/forge/src/transition_identity/{tree_lifecycle,retained_usr_exchange_syscall,previous_tree_move}.rs` |
| `/etc`, triggers, boot rollback | `crates/forge/src/client/{transaction_root,boot}.rs`; `crates/forge/src/client/postblit/{retained_transaction,system_trigger_container}.rs`; `misc/boot/cast-fstx.{sh,service}` |
| ActiveReblit and known gaps | `crates/forge/src/client/{verify,active_reblit_transition,new_state_boot_transition}.rs`; `FUTURE_PLAN.md` |

The bootc website tracks development; recheck version-specific behavior during implementation.
Baseline: [bootc v1.16.4](https://github.com/bootc-dev/bootc/releases/tag/v1.16.4).

- [overview](https://bootc.dev/bootc/), [runtime model](https://bootc.dev/bootc/building/bootc-runtime.html), [package integration](https://bootc.dev/bootc/package-managers.html), [image contract](https://bootc.dev/bootc/bootc-images.html), [build guidance](https://bootc.dev/bootc/building/guidance.html)
- [install](https://bootc.dev/bootc/bootc-install.html), [bootloaders](https://bootc.dev/bootc/bootloaders.html), [storage](https://bootc.dev/bootc/filesystem-storage.html), [filesystem](https://bootc.dev/bootc/filesystem.html), [upgrades](https://bootc.dev/bootc/upgrades.html), [status](https://bootc.dev/bootc/man/bootc-status.8.html), [rollback](https://bootc.dev/bootc/man/bootc-rollback.8.html)
- [offline](https://bootc.dev/bootc/registries-and-offline.html), [secrets](https://bootc.dev/bootc/building/secrets.html), [signature policy](https://bootc.dev/bootc/man/bootc-install-config.5.html), [experimental composefs](https://bootc.dev/bootc/experimental-composefs.html), [Linux initramfs](https://docs.kernel.org/filesystems/ramfs-rootfs-initramfs.html), [BLS](https://uapi-group.org/specifications/specs/boot_loader_specification/)
