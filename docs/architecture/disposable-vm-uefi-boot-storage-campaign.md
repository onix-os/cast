# Disposable-VM UEFI boot-storage campaign

This is the explicit boundary between host-safe tests and real storage effects.
The repository does not contact a hypervisor, open an SSH connection, select a
machine, select a disk, or reboot anything. An operator must enter an already
snapshotted disposable VM and invoke the dedicated Make targets there.

The current campaign is a filesystem-substrate proof. It formats one admitted
whole disposable disk as FAT32, mounts it only below
`/run/cast-vm-boot-storage`, creates one declared publication-parent directory,
syncs and unmounts it, then remounts it to prove that directory persisted. It
explicitly disables `mkfs.fat`'s fake whole-device MBR, then repeats the
partition-free target admission before mounting. The result remains a
whole-device filesystem rather than a partition table. It does **not** publish
a boot payload, mutate the guest's live ESP, change boot
entries, reboot, simulate power loss, or prove a GPT ESP/XBOOTLDR topology.
It is deliberately a cooperative disposable-guest harness, not production
descriptor authority: guest root must remain exclusive, the admitted device
must not be hot-unplugged or rebound, and no competing storage actor may race
the pathname-based formatter or mount calls. Production publication must retain
its own descriptor-backed authority and cannot cite this harness as a substitute.

## Required operator facts

Every value is mandatory. There are no device, identity, size, mountpoint, or
command fallbacks.

- `VM_EXPECTED_HOSTNAME`: exact guest hostname.
- `VM_EXPECTED_MACHINE_ID`: exact 32-character lower-case machine ID.
- `VM_EXPECTED_BOOT_ID`: exact boot UUID for this still-running guest boot.
- `VM_EXPECTED_VIRTUALIZATION`: exact value reported by
  `systemd-detect-virt --vm`.
- `VM_EXPECTED_COMMIT`: exact clean checkout commit containing the harness.
- `VM_TARGET_DISK`: canonical, non-symlink whole-disk device node.
- `VM_TARGET_STABLE_PATH`: explicit root-owned stable alias below the narrow
  `/dev/disk/by-id` or `/dev/disk/by-path` allowlist, resolving to that node.
- `VM_TARGET_DISKSEQ`: exact positive kernel disk sequence for that target.
- `VM_TARGET_DISK_BYTES`: exact decimal byte size using the kernel's fixed
  512-byte sector count.
- `VM_EXPECTED_ROOT_DEVICE`: canonical device owning the live `/` mount.
- `VM_EXPECTED_LIVE_ESP_DEVICE`: canonical partition owning the live ESP mount.
- `VM_EXPECTED_LIVE_ESP_MOUNTPOINT`: exactly `/boot`, `/boot/efi`, `/efi`, or
  `/esp`, whichever is already the guest's live ESP mount.
- `VM_FILESYSTEM_LABEL`: 1 through 11 upper-case FAT label bytes.
- `VM_PUBLICATION_PARENT`: safe relative directory, such as `EFI/Linux`.
- `VM_SNAPSHOT_CONFIRMATION`: exactly
  `snapshot-ready:$VM_EXPECTED_BOOT_ID:$VM_EXPECTED_COMMIT`.
- `VM_REMOTE_CONFIRMATION`: exactly `disposable-vm-remote-only`.
- `VM_COOPERATIVE_ROOT_CONFIRMATION`: exactly
  `cooperative-guest-root-no-hotplug`; this explicitly accepts the disposable
  harness's no-competing-root and no-device-rebinding assumption.

The snapshot confirmation is an operator assertion. The guest cannot
authenticate hypervisor snapshot state, and the repository intentionally does
not run `virsh` or another host command to do so.

All three runtime targets require root, a UEFI boot, a persistent clean checkout
outside volatile directories, an active SSH session, and the exact same SSH
connection for challenge and consumption. If privilege escalation discards
`SSH_CONNECTION`, admission fails rather than weakening that binding. Enter a
root SSH session directly or explicitly preserve that single variable through
the guest's configured privilege boundary; do not fabricate another value.
Git is invoked with an invocation-local `safe.directory` for the exact checkout,
so a checkout owned by the remote user does not require global Git policy.
The harness replaces inherited `PATH` with the fixed guest-system path
`/usr/sbin:/usr/bin:/sbin:/bin`, uses the C locale, and rejects effect commands
that are not root-owned or are group/other-writable.

## Three-step protocol

1. Run the non-destructive challenge target with all common variables:

   ```sh
   make disposable-vm-uefi-boot-storage-challenge
   ```

   It authenticates guest identity, the clean commit, root and live-ESP
   ownership, and the separate unmounted target. It then creates the root-owned
   mode-0600 marker `/run/cast-vm-boot-storage/authorization-v1` and prints a
   fresh `VM_BOOT_STORAGE_CHALLENGE`. The challenge expires after five minutes.
   Expired or abandoned marker state is never replaced automatically; inspect
   it inside the VM and remove it explicitly before rearming.

2. Export that exact challenge and run the non-consuming dry admission:

   ```sh
   export VM_BOOT_STORAGE_CHALLENGE=the-printed-64-character-value
   make disposable-vm-uefi-boot-storage-admission
   ```

   This reauthenticates the marker and repeats disk admission without changing
   either the marker or the disk. A successful dry run is not effect authority;
   the campaign repeats every check after consuming the marker.

3. Construct the exact destructive confirmation and invoke the separately
   named campaign target:

   ```sh
   export VM_DESTRUCTIVE_CONFIRMATION="erase:$VM_TARGET_STABLE_PATH:$VM_TARGET_DISK_BYTES:$VM_TARGET_DISKSEQ:$VM_EXPECTED_BOOT_ID"
   make disposable-vm-uefi-boot-storage-campaign
   ```

   This is the only target that formats or mounts the target. It atomically
   consumes the challenge before a second complete admission pass. Filesystem
   creation, mount, sync, and unmount children run under non-foreground GNU
   `timeout` process-group deadlines. Mount and unmount use util-linux
   internal-only mode so filesystem helpers are not launched. Make, Git, and
   the surrounding SSH session are not externally timeout-wrapped.

## Fail-closed disk admission

The selected target must be a root-owned canonical whole-disk node whose exact
kernel device number, sysfs identity, disk sequence, stable alias, writable
state, and byte size agree. Admission rejects:

- a target partition or a whole disk with any partition;
- any holder or slave relationship;
- any target mount or active swap relationship;
- the live root device, the live ESP device, or either one's parent disk;
- an absent, replaced, multiply linked, or non-root-owned stable alias;
- an absent or ambiguous live root/ESP mount; and
- identity drift between the opening and closing target observations.

Immediately before effects, the script also requires its mount namespace to be
exactly PID 1's guest init mount namespace. This deliberately makes the mount
visible to the disposable guest while keeping it inside the VM boundary; it is
not a private production mount-namespace proof. Mounts hidden in another guest
namespace and non-holder multi-device membership are outside this cooperative
harness's observation, so the VM must have no competing root/storage actor.

The campaign records the exact admitted target, device number, size, guest boot
ID, and repository commit before the first formatting child starts. It never
searches for a substitute disk when any supplied fact fails.
Diskseq and stable-path checks are repeated at the closing observation, and the
consumed marker's exact binding and freshness are the literal last check before
the formatter starts. These scalar/path checks protect against accidental disk
selection in the fixed disposable VM; they do not create retained block-device
authority across hostile hotplug or guest-root races.

The VFAT attachment is admitted only with `rw`, `nosuid`, `nodev`, `noexec`,
`nosymfollow`, effective root ownership, file mask `0133`, and directory mask
`0022`. Linux may omit default `uid=0,gid=0` strings from mountinfo, so the
harness proves their effective result from the mounted filesystem root instead
of requiring those optional textual spellings.
After creating the declared parent, the campaign synchronizes, unmounts,
remounts, checks persistence, synchronizes again, and unmounts.

## Interrupted runs

The challenge is single-use at the destructive boundary. After consumption,
any nonterminal failure preserves the consumed marker and campaign lock even if
formatting returned an error and nothing is mounted. A normal failure
tries to unmount only when the private mountpoint still has the exact admitted
device and policy. It does not force, retry, discover a replacement, or touch
another mount. Ambiguous cleanup leaves the consumed marker and campaign lock
in `/run/cast-vm-boot-storage` so a later invocation fails closed.

`SIGKILL` cannot run shell cleanup, so the same-boot runtime sentinels and any
remaining attachment must be reviewed inside the disposable VM. Power loss or
reboot clears `/run`; after either, the disk is unclassified and the operator
must recover from the VM snapshot rather than infer success from missing
sentinels. There is intentionally no automatic cleanup, reboot, host fallback,
or hypervisor recovery target.

The safe local static check is:

```sh
make disposable-vm-uefi-boot-storage-harness-test
```

It parses and inspects the harness without reading a block device or running a
storage command. It is not a dependency of `make test`, `make check`, or any
default target.
