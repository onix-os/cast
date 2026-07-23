# ActiveReblit mounted boot topology

## Status and purpose

This document defines the authority boundary for publishing
ActiveReblit boot assets to an already-mounted EFI System Partition (ESP) and,
when present, an already-mounted XBOOTLDR partition. It is an architecture
contract for the implemented descriptor-retained capture foundation and the
later publisher, not a claim that real ESP/BOOT publication is enabled.

The design keeps two kinds of information separate:

- `/etc/cast/boot-topology.glu` declares which machine-local partitions and
  mount points the administrator intends to use; and
- retained kernel-backed evidence proves what those declarations refer to in
  the current mount namespace immediately before publication.

An authored path or PARTUUID is never mutation authority by itself. The new
path must fail closed rather than discover a plausible host partition, mount a
partition, or fall back to the legacy boot manager.

## Version 2 authored intent

The only accepted module for this design is `cast.boot_topology.v2`. Its
Gluon-facing value has this closed shape:

```gluon
type PartitionSelector = {
    partuuid: String,
    mount_point: String,
}

type BootTarget =
    | AliasEsp
    | DistinctXbootldr PartitionSelector

type BootTopologyIntent = {
    esp: PartitionSelector,
    boot: BootTarget,
}
```

`cast.boot_topology.aliases_esp selector` uses one `PartitionSelector` for
both logical destinations. `cast.boot_topology.distinct esp xbootldr` uses two
selectors. The constructors do not infer any field.

`partuuid` remains a canonical, lowercase, non-nil UUID. `mount_point` is a
mandatory, bounded absolute lexical selector. Its components are explicit input:
empty components, `.`, `..`, repeated separators, a trailing separator, NUL,
and the current task's filesystem root are rejected. A bounded implementation
must preserve the accepted authored UTF-8 as exact bytes for later comparison;
it does not canonicalize the selector.

### Selectors are not authority

A `PartitionSelector` is an untrusted lookup hint. In particular:

- it does not prove that the path exists or is a mount point;
- it does not prove a mount's device, partition identity, GPT role, filesystem
  type, parent disk, or durability behavior;
- it is never passed to `canonicalize` or resolved through an ambient current
  directory;
- it has no default and is not supplemented by ESP, `/boot`, fstab, udev, or
  block-device discovery; and
- failure to authenticate its exact target is terminal for that attempt. There
  is no alternative path, device, mount, or legacy fallback.

The selector exists because an opaque directory descriptor and a mount ID can
show that a directory resides somewhere on a mount, but cannot establish that
the descriptor names the declared mount attachment at its namespace-visible
mount root.

### Version 1 requires a manual rewrite

`cast.boot_topology.v1` contains PARTUUIDs but no mount-point selectors. There
is therefore no safe mechanical migration: filling the missing paths would
require inspecting the running machine or inventing defaults, and either
choice would turn ambient host state into configuration.

Administrators must rewrite v1 files as explicit v2 values. A v1 source is a
visible schema error; it is not upgraded in memory and does not select a
compatibility path. This narrow migration rule does not make Nix compatibility
or incompatibility a project goal. It only refuses to guess missing local
storage authority.

## Retained authority acquisition

The production capture aggregate authenticates an already-mounted topology
without mounting, unmounting, creating, formatting, or partitioning anything.
It owns the following non-cloneable, lifetime-bound evidence.

### Current mount-namespace epoch

At the beginning of acquisition, retain a descriptor for the current thread's
mount namespace and authenticate it as the mount-namespace type on `nsfs`.
Bind its device/inode witness to the aggregate. Separately retain the current
task's filesystem-root descriptor as the traversal origin. These descriptors
establish one task-relative filesystem epoch; they do not grant permission to
switch namespaces or change the task root.

The distinction is required by Linux: mountinfo's `mount_point` field is
relative to the reader's filesystem root. Production opens the exact current
thread's root through authenticated `/proc/<pid>/task/<tid>/root`; it does not
substitute the thread-group leader's `/proc/<pid>/root`. Mount-namespace
identity alone therefore cannot detect an observed `chroot` or equivalent
task-root mismatch. Acquisition and revalidation must bind both witnesses. See
[`proc_pid_mountinfo(5)`](https://man7.org/linux/man-pages/man5/proc_pid_mountinfo.5.html)
and [`proc_pid_root(5)`](https://man7.org/linux/man-pages/man5/proc_pid_root.5.html).

All attachment, mountinfo, sysfs, rendering-input, and publication checks occur
on the acquiring thread. Both the mount-namespace and current-task-root
witnesses must be checked around every complete evidence pass and again at the
terminal rebind. Moving a prepared value across threads or accepting a
caller-supplied pathname descriptor is not allowed.

### Current-task-rooted attachment chain

For each selector, start at the retained current-task root and open every exact
raw component descriptor-relatively. Retain:

- the raw component names;
- every directory descriptor and its inode/type witness;
- every descriptor mount ID; and
- the final parent descriptor, final raw name, and final directory descriptor.

The walk rejects symlink components and non-directories. It does not use
`canonicalize`, `/proc/self/fd` pathname round trips, a current working
directory, or a pathname reopen from the host root. Mount crossings are
observed rather than followed implicitly and forgotten.

Rebinding the complete chain from the retained current-task root must reproduce
every component witness. A final parent-plus-name rebind must reproduce the
destination inode and mount ID. Possessing only the final directory descriptor
is insufficient.

### Exact mountinfo attachment

Read the current thread's mountinfo through the authenticated procfs boundary
under fixed byte, entry, work, descriptor, and time limits, then require:

1. exactly one entry has decoded `mount_point` bytes equal to the selector's
   exact authored bytes;
2. that sole selector-matching entry has the final descriptor's mount ID;
3. the selected entry's mount root is exactly `/`;
4. the selected mount ID occurs exactly once in the snapshot;
5. the entry's `major:minor` equals the final descriptor's device identity;
6. the selected filesystem type is exactly `vfat`;
7. the selected per-mount options contain exactly one `rw`, no `ro`, and
   exactly one each of `nosuid`, `nodev`, `noexec`, and `nosymfollow`, without
   any positive inverse or duplicate required token;
8. the selected superblock options contain exactly one `rw` and no `ro`; and
9. descriptor revalidation still yields the same mount ID and attachment
   chain.

The selector-wide uniqueness rule rejects stacked attachments with the same
namespace-visible mount point; mount-ID uniqueness alone would not detect that
ambiguity. The exact `/` root rule excludes a bind of a subdirectory as
partition-root authority. Capture retains the admitted filesystem kind and
required option facts as a closed policy value and compares that value across
every observation. It does not retain the raw option lists or mount source as
authority.

This is exact **mountinfo policy evidence**, not independent filesystem
authentication. Commit `b8acd3d4` provides a separate bounded retained-
descriptor `fstat`/`fstatfs` sandwich which proves stable directory identity
and the Linux MSDOS magic family without claiming that magic alone is exact
`vfat`. Commit `a93efe70` composes both claims inside every target observation
and exact later-pass comparison. That composition does not prove a GPT
partition type, authorize a write, or establish file, directory, filesystem,
or device durability. Requiring `nosymfollow`
also makes admission to the future boot publisher effectively Linux 5.10 or
newer. The reusable `linux_fs` descriptor and mountinfo primitives keep their
Linux 5.6 compatibility baseline; this stricter policy belongs only to mounted
boot targets. This follows the [Boot Loader Specification](https://uapi-group.org/specifications/specs/boot_loader_specification/);
the kernel-version boundary is documented by [`mount(2)`](https://man7.org/linux/man-pages/man2/mount.2.html).

### Sysfs partition identity

Use the authenticated mount `major:minor` to prepare and revalidate the
descriptor-retained sysfs partition snapshot. The snapshot's canonical
PARTUUID must equal the selector's PARTUUID exactly. The snapshot proves only
the kernel block identity and block-parent observations made during its
authenticated capture and revalidation passes. It also retains bounded kernel
`DEVNAME` values for the partition and parent plus canonical partition `start`
and positive `size` values in Linux's fixed 512-byte sectors. It is not a
continuously live view and does not claim call-time or simultaneous residency.

Commit `5ed70923` separately provides a strict bounded pure parser for mirrored
GPT headers and entry arrays, a non-hybrid protective MBR, and an exact expected
ESP or XBOOTLDR partition role. Commit `c2539d7f` makes that authentication two
complete passes under one cumulative ledger and deadline. The passes must agree
on image length, logical block size, the full protective-MBR block, both full
header blocks, the already mirrored entry array, and selected semantics. The
returned closed evidence adds one role-independent, domain-separated table
fingerprint; it retains no table bytes, image, descriptor, path, or reusable
read authority. Commit `215b9032` additionally retains the selected partition
number, logical-block size, and complete image length. Its private inter-pass
hook runs exactly once after the first stable table pass and before any second
source observation, under the original deadline, so a future descriptor owner
can rebind live identity without weakening the two-pass schedule.

Commit `f8a5da34` adds path-free read-only block-descriptor observations and a
bounded positional image over a caller-retained descriptor. Commit `1f9d578a`
then orders opening sysfs-parent preflight, GPT pass one, a caller-owned
same-deadline name rebind, exact inter-pass descriptor re-observation, GPT pass
two, closing observation, and pure reconciliation. Its distinct closed result
therefore proves that both GPT passes used the retained descriptor; it carries
no descriptor or reusable read capability.

Commit `24d82abf` seals the parent `DEVNAME`, device number, partition number,
PARTUUID, fixed-512-sector geometry, and optional disk sequence to one freshly
revalidated sysfs view. Commit `78b87df9` separately validates only an exact
`/dev` root attachment reported as `devtmpfs`, including consistent access and
device-option semantics. That mountinfo policy is not descriptor authority and
cannot alone prove whole-filesystem bind provenance. Commit `2eeaa22c` adds a
bounded pure reconciliation of exact opening and closing block-node identity,
access, geometry, and capacity observations with the GPT role and sealed sysfs
expectation. It rejects geometry overflow and disagreement, but is explicitly
non-authoritative because injected observations prove neither a live descriptor
nor the provenance of GPT reads. Commit `dceab6cc` separately sandwiches
`fstat`, authenticated mount-ID, and `fstatfs` observations for a borrowed
directory and requires stable `TMPFS_MAGIC` evidence to agree with the exact
devtmpfs mountinfo policy. Because tmpfs shares that magic, this proves only
same-mount descriptor evidence by itself. Commit `bfa3a0c2` now composes it
with the exact retained `/dev` attachment, opens the sealed parent `DEVNAME`
beneath the same private destination, and owns the complete opening-preflight,
GPT-pass-one, private same-deadline name rebind, exact inter-pass observation,
GPT-pass-two, closing-observation, and reconciliation schedule. The closed
result proves neither whole-root non-bind provenance nor ongoing currentness;
same-thread `setns` must be caught by outer aggregate revalidation. Physical
disk flushes, restart persistence, and VM-backed evidence therefore remain
separate open claims. The selected mountinfo `vfat` policy and Linux
MSDOS-family descriptor witness are jointly retained per topology pass, but
the magic family alone is not exact `vfat`.

## Topology invariants

The aggregate admits only one of two closed results.

### Aliased ESP

`aliases_esp` has one selector and therefore one attachment. ESP and BOOT must
refer to that same retained attachment: the same namespace chain, destination
inode, mount ID, device, sysfs partition snapshot, and PARTUUID. Equality of
only PARTUUIDs or only device numbers is not enough.

### Distinct XBOOTLDR

`distinct` must prove all of the following at the same observation boundary:

- two different namespace attachments and mount IDs;
- two different mounted device numbers;
- two different canonical PARTUUIDs matching their respective selectors; and
- the paired sysfs snapshots in each topology pass reporting the same retained
  authenticated block-parent witness.

Bootstrap, Pass1, Pass2, and Terminal must agree on that paired comparison.
Each pass revalidates and compares both snapshots under the same caller-owned
absolute deadline. The result is an observation over those bounded passes,
not a continuously live or simultaneous-residency claim. It is not a general
proof that firmware, multipath, or storage hardware will preserve the
relationship.

## Repeated capture and terminal rebind

One successful observation is not enough. A complete pass is a sandwich:

```text
mount namespace
  -> current task root
  -> current-task-rooted attachment chain
  -> destination descriptor mount ID
  -> bounded mountinfo snapshot and exact selected entry
  -> retained sysfs partition snapshot
  -> destination mount ID again
  -> attachment chain again
  -> current task root again
  -> mount namespace again
```

Preparation records one Bootstrap observation, then requires exact Pass1,
Pass2, and Terminal observations before returning the retained aggregate.
Every later revalidation repeats Pass1, Pass2, and Terminal against the exact
Bootstrap facts. Each pass opens the retained intent and mount context, reads
exactly one authenticated mountinfo snapshot for all targets, derives any
distinct-target paired sysfs comparison directly, reverse-checks the
attachments, closes the mount context and intent, and checks the same absolute
deadline both before and after scalar validation. Immediately before a future publication effect,
the publisher must require another successful aggregate revalidation rather
than treating preparation as a lasting lease. Any replacement, disappearance,
ambiguity, namespace or task-root mismatch, identity mismatch, or limit
exhaustion observed at one of these checks invalidates the entire aggregate.
These endpoint sandwiches deliberately do not claim to detect a transient
change that returns to the exact retained identity between checks. The same
attempt must not recapture a new authority and continue.

## Rendering and publication boundary

Commit `aa341706` implements the pure deterministic BLS renderer. It consumes
the lifetime-bound semantic input aggregate, emits bounded deterministic
desired content, and combines it only with a revalidated authenticated
topology view carrying the identical absolute deadline. The resulting
non-detachable publication plan retains both views and the exact sealed source
catalog.

The pure renderer never receives destination file descriptors, namespace
descriptors, mutation leases, or a function capable of writing. Consequently,
rendering cannot discover storage or mutate a mounted partition.

The renderer retains the exact input and topology views, their identical
caller-owned deadline, and the exact sealed systemd-boot, kernel, and initrd
asset views. Generated loader and Type 1 entry bytes, payload ordering,
case-insensitive collision rules, FAT-safe relative paths, and finite
request/path/generated-byte/work limits are deterministic and covered by
synthetic golden tests. No coordinate can be detached from the retained asset
catalog and treated as source authority.

The implemented input stages preserve one caller-owned absolute deadline
through the exact state/layout database and Stone projection -> bounded asset
plan -> sealed CAS snapshot -> Stone binding chain. The lifetime-bound
render-input aggregate retains those exact owners, state-root authority,
schemas, local policy, root intent, and exact systemd-boot/kernel/initrd
coordinates through terminal revalidation. Its successful view retains the
absolute deadline rather than exposing a fresh timeout boundary.

Package-owned command-line files now cross a separate semantic preparation
boundary before rendering. That value remains lifetime-bound to the exact
non-cloneable Stone input owner, rebinds every state, role, path, index, digest,
and length coordinate, and reads each sealed source only by bounded explicit
offset under one caller-owned deadline. It retains normalized printable-ASCII
text, not a destination descriptor or write capability.

The dedicated `/etc/cast/root-filesystem.glu` producer now authenticates a
closed `cast.root_filesystem.v1` value containing one explicit locator and can
release exactly one injection-safe `root=<value>` token only after terminal
revalidation. It does not infer that value from ESP/XBOOTLDR topology,
`/proc/cmdline`, fstab, udev, or the legacy disk probe, and it does not prove
that the named storage exists.

The aggregate establishes global single-root ownership before concatenating
any command-line source. It grammar-audits every package and local append before
scope or masking, rejects authored `root` and `cast.fstx` keys, and emits exactly
one authenticated root token and one state token per kernel under the retained
caller-owned deadline.

The implemented pure publication plan consumes the authenticated topology
layout only to scope destination collisions: aliased ESP/Boot share one domain,
while a distinct XBOOTLDR uses a separate domain. The revalidated topology view
now retains its caller-owned deadline so the renderer can require an exact
match with its input aggregate. The plan performs a terminal deadline check
after complete materialization, but neither authorizes a target identity nor
grants a destination descriptor or mutation capability.

Commit `9ac34286` adds the separate pure destination-namespace classifier. It
keeps request order and returns only `Absent`, `Exact`, or `Different`; complete
opening and closing inventories bind kernel lookups to raw names so ASCII case
aliases, FAT short-name aliases, cross-mount entries, type changes, and content
races fail closed. Independent hard ceilings cover requests, 4095-byte paths,
8 MiB aggregate path bytes, directory entries, raw names, reads, allocations,
descriptors, sort work, and the caller-owned deadline. Commit `2eeaa22c` adds
its bounded syscall-free raw
`getdents64` chunk parser, including strict record and name validation,
dot-entry filtering, and an explicitly budgeted terminal EOF probe. It returns
only a closed raw-name inventory and does not trust inode or type hints. Commit
`f8a5da34` supplies one syscall adapter which consumes a caller-owned fresh,
offset-zero directory description without paths or offset reset. Commit
`71ee5e95` reserves descriptor capacity before ownership-transfer callbacks and
guarantees LIFO release on success and failure. Commit `365e0ae5` completes the
bounded retained production observer, including fresh descriptor-relative
inventories, kernel lookup binding, positional content reads, metadata
sandwiches, and scalar-only results. Commit `8620986a` adds the exact observed
root device, inode, and mount ID. Commit `3f8309b1` then authenticates the boot
filesystem, assesses through the same private destination `File`, authenticates
the filesystem again, and requires that root triple to match before returning
closed attachment evidence. Commit `97fb33b3` closes the bounded
expected-source bridge by streaming generated slices and sealed asset
descriptors without materializing the roughly 10-GiB publication ceiling;
later bindings retain exact source identities and a canonical desired
publication inventory without granting mutation authority. The complete
authority-free receipt layer now maps that exact bound plan into one bounded
canonical body without retaining descriptors, accessing the database, or
granting effect authority. The body binds its transition, optional committed
predecessor, canonical predecessor-record and desired-inventory SHA-256 values,
exact alias/distinct destination identities with historical witnesses, and
every ordered output with a keyed inert provenance claim. Journal payload v3
carries only the compact immutable pair. One exclusive SQLite transaction
inserts the immutable body and stages its pending singleton head, with strict
body/head validation. Commit `5acba0ba` makes startup load and retain the strict
full receipt state, admitting v3 only when its compact journal pair correlates
exactly; production forward staging remains unwired. Existing v1/v2 records already at `BootSyncStarted` retain a
conservative journal-only route. This is not a publisher and grants no boot
mutation or deletion authority. Authenticated claim derivation, exact durable
predecessor-record binding, pending promotion, and real-publication VM evidence
remain open.

A separate durable publisher combines:

- the frozen render plan and its identity;
- the still-retained, terminally revalidated topology aggregate;
- retained destination descriptors;
- a one-attempt mutation authority; and
- the transition journal state that explains whether publication is starting,
  completing, or being recovered.

The publisher performs one bounded attempt. It does not remount, rediscover,
choose another target, or retry by reacquiring authority. Durable boundaries
and restart reconciliation belong to the journaled protocol, so a crash cannot
silently turn one publication decision into a different one.

The existing `blsforme::Manager::mount_partitions()` route is categorically
inadmissible here. It combines discovery and mount effects behind an API that
cannot satisfy the explicit selector, retained attachment, no-fallback, and
one-attempt authority contracts. New code must not call it as a convenience or
recovery path.

## Evidence deliberately deferred

Mounted topology is necessary but not sufficient for real boot publication.
Later layers must independently authenticate:

- authenticated derivation of every keyed inert provenance claim and exact
  durable predecessor-record binding in production forward staging;
- the exact partition/disk relationship beyond the retained observations;
- publication ordering, file replacement, directory synchronization, and
  device flush durability; and
- restart reconciliation at every durable boundary.

Until those layers are implemented and composed, production must not interpret
the mounted-topology aggregate as permission to publish to a real ESP/BOOT.

## Test boundary

Default and focused local boot/storage-topology tests are synthetic only. They
may use bounded mountinfo byte fixtures, impossible device numbers, ordinary
temporary directories, injected sysfs trees, and effect recorders. They do not
inspect the host's ESP, `/boot`, `/efi`, `/esp`, raw devices, live mount
topology, or storage tools, and they never perform a storage mutation.

At exact commit `58c87a5db50bec7a5ac00978455841c7d2402689`, disposable
UEFI guest `test` was observed with `/` on `/dev/vda2`, its live ESP on
`/dev/vda1`, and a separate untouched `/dev/vdb` of exactly 34359738368 bytes.
The operation-specific atomic suffix passed ActivateArchived complete 17/17
and finalization 24/24; ActiveReblit dispatch 62/62, complete 11/11, and
finalization 24/24; shared RootLinks terminal-process 3/3 and delete-residue
recovery 13/13; synthetic boot namespace 40/40; and all receipt/startup
boot-repair Make lanes exited zero. No disk, ESP, mount, reboot, or live-`/usr`
mutation occurred. This is same-boot synthetic/component evidence only, not
publisher, ESP-publication, reboot, or power-loss proof.

Real ESP/BOOT publication and durability testing must run only in a disposable
virtual machine explicitly supplied by the user. A dedicated VM target must be
absent from every default or local validation dependency, require the reviewed
guest marker and per-run challenge, and fail closed if its exact disposable
disk is missing or ambiguous. It must never fall back to the host, choose a
hypervisor automatically, or attach a physical host disk.

The repository-wide boundary is documented in
[`host-storage-test-safety.md`](host-storage-test-safety.md).
