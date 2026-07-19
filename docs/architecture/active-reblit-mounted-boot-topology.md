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
`vfat`. The two foundations are not yet composed; neither proves a GPT
partition type, authorizes a write, or establishes file, directory,
filesystem, or device durability. Requiring `nosymfollow`
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
authenticated capture and revalidation passes. It is not a continuously live
view and does not claim call-time or simultaneous residency.

It does not prove the GPT type GUID, ESP/XBOOTLDR role, composition with the
standalone destination-descriptor filesystem evidence, physical-disk identity,
flush behavior, or persistence across reboot. Those claims require later,
separately retained evidence. The selected mountinfo `vfat` policy and Linux
MSDOS-family descriptor witness are separate inputs to that later composition.

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

- the on-disk GPT partition type as ESP or XBOOTLDR;
- composition of the retained destination descriptor's Linux MSDOS-family
  evidence with the admitted mountinfo `vfat` policy and required features;
- the exact partition/disk relationship beyond the retained sysfs observations;
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

Real ESP/BOOT discovery and publication tests run only in a disposable virtual
machine explicitly supplied by the user. A future dedicated VM target must be
absent from every default or local validation dependency, require the reviewed
guest marker and per-run challenge, and fail closed if its exact disposable
disk is missing or ambiguous. It must never fall back to the host, choose a
hypervisor automatically, or attach a physical host disk.

The repository-wide boundary is documented in
[`host-storage-test-safety.md`](host-storage-test-safety.md).
