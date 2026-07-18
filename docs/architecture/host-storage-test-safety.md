# Host-storage test safety

Boot and storage code has a larger blast radius than ordinary filesystem code.
A test that accidentally discovers the developer's EFI System Partition (ESP),
opens a real block device, or runs a storage administration command can damage
the machine on which the test suite is being developed. This document makes the
safe boundary explicit before host topology work grows beyond pure parsers.

## Non-negotiable local boundary

Default and focused local **boot/storage** tests operate only on test-owned
ordinary files and directories. They must not:

- inspect, mount, unmount, or modify the host ESP, `/boot`, `/efi`, or `/esp`;
- open a raw block-device node, including persistent `/dev/disk/by-*` aliases;
- invoke mount, unmount, loop-device, filesystem-creation, partitioning,
  device-mapper, swap, discard, or wipe tools;
- invoke host storage-discovery or inspection tools such as `lsblk`, `blkid`,
  `findmnt`, `udevadm`, `smartctl`, `nvme`, or `hdparm`;
- redirect output into `/sys` or use `/sys` as a mutation target;
- infer a safe device from the running host and then act on it; or
- require root, `sudo`, `doas`, a host mount namespace, or a host device passed
  into a container.

This applies to every target used by `make test`, `make check`, and focused
parser or boot-topology test targets. Adding an opt-in environment variable to
a local target does not make host storage safe.

The existing container conformance lane is a separate substrate test: it may
exercise mount syscalls only inside its private namespace and only against
test-owned temporary roots. It is not a boot/storage integration mechanism and
must never receive a host ESP or block device. New boot/storage tests may not
use that lane to bypass this policy.

## Safe local harness

Local storage tests are built from pure, injected inputs:

1. Sysfs and mount-information parsers receive bounded byte strings or a
   test-owned directory tree. Synthetic identities use deliberately impossible
   major/minor values and never resolve through the host `/dev` tree.
2. Partition-table and filesystem-image tests use ordinary files below a
   temporary directory. They may parse bytes, offsets, checksums, and labels,
   but must not attach those files to a loop device or run a host storage tool.
3. Boot-topology policy receives declared fixture records. It never discovers
   the running machine's ESP or boot mount as part of a unit test.
4. Mutation is represented by an injected effect recorder. Tests assert the
   proposed operation and ordering without carrying it out.
5. Every external validation process is timeout-bounded. Fixture cleanup may
   remove only the temporary root that the test created.

Read-only parser literals such as `/sys/dev/block/<major>:<minor>`, synthetic
mountinfo text, `/dev/null`, `/dev/zero`, and package payload paths such as
`usr/lib/systemd/boot/efi` are not host-storage authority. They are allowed
only when no operation opens or mutates host storage. The existing
`udevadm verify` fixture check is also allowed because it validates an explicit
test-owned rule file; no other `udevadm` subcommand is admitted locally.

## Static admission gate

`make host-storage-safety-test` inspects repository-owned local test and
operational harness sources:

- the root `Makefile` and `misc/make/*.mk`;
- `misc/scripts/**/*.sh`; and
- Rust test files, test directories, and Rust modules containing
  `#[cfg(test)]`.

The gate rejects literal host ESP/BOOT paths, concrete raw block-device paths,
direct shell or `Command::new` invocations of storage discovery and
administration tools, and literal shell redirections into `/sys`. Its matcher
has positive and negative self-probes, so changes must continue to reject
unsafe examples without rejecting harmless parser text, pseudo-devices, help
text, or container test names.

The gate deliberately excludes documentation, generated artifacts, and its own
matcher definition. Documentation has to name the prohibited operations, and
the matcher has to contain its own rejection fixtures. Production boot assets
under `misc/boot` are not a local test harness. A future real integration
harness belongs in a separately reviewed disposable-VM-only directory and must
not become a dependency of any local/default target.

This is a static admission check, not a shell parser or a sandbox. Code review
must reject indirection intended to hide a prohibited command, and runtime
tests must retain the injected-input design above.

## Real integration boundary

Testing real kernel discovery or storage mutation is allowed only inside a
disposable virtual machine explicitly supplied by the user. Such a harness
must fail closed unless all of the following are true:

1. the user invokes a dedicated integration target that is absent from every
   default, check, lint, and local test dependency chain;
2. the guest presents a repository-defined disposable-VM marker and a fresh
   per-run challenge value;
3. the target disk is created for that VM run, is not a host-passed physical
   disk, and is identified by an exact expected size and identity inside the
   guest;
4. every destructive step and cleanup step is timeout-bounded and records the
   selected guest device before mutation; and
5. missing, ambiguous, or additional candidate devices abort the run without
   attempting cleanup on them.

The user supplies and destroys the VM. The repository must not auto-select a
hypervisor, attach a host disk, probe the host ESP, or fall back from a missing
VM to local execution.
