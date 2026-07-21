# Disposable-VM UEFI boot-storage campaign

This is the explicit boundary between host-safe tests and real storage effects.
The repository does not contact a hypervisor, open an SSH connection, select a
machine, select a disk, or reboot anything. An operator must enter an already
snapshotted disposable VM and invoke the dedicated Make targets there.

The harness exposes two separately named, marker-bound profiles. The base
profile extends the filesystem-substrate proof through one real retained leaf
publication. It formats one admitted whole disposable disk as FAT32, mounts it
only below `/run/cast-vm-boot-storage`, creates one declared publication-parent
directory, invokes Forge's production single-leaf publisher, syncs and unmounts
it, then remounts and invokes the same publisher again to prove the exact leaf
persisted. It explicitly disables `mkfs.fat`'s fake whole-device MBR, so this
profile remains a partition-free whole-device filesystem.

The dedicated `gpt-boot-topologies` profile instead repartitions the admitted
disk twice. It first exercises one GPT ESP as both ESP and BOOT, then exercises
a distinct GPT ESP plus XBOOTLDR. Each layout runs production mounted-topology
capture and the production retained-descriptor leaf publisher before and after
sync, unmount, read-only filesystem checking, and remount. The fixed leaves and
payloads are test evidence, not a selected kernel, initrd, entry, loader, or
bootloader. Neither profile mutates the guest's live ESP, changes firmware boot
entries, reboots, simulates power loss, or proves that the resulting payloads
are bootable.

Both profiles are deliberately cooperative disposable-guest harnesses, not
production descriptor authority: guest root must remain exclusive, the
admitted device must not be hot-unplugged or rebound, and no competing storage
actor may race the pathname-based partitioner, formatter, or mount calls.
Production publication must retain its own descriptor-backed authority and
cannot cite either harness as a substitute.

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
that are not root-owned or are group/other-writable. The publisher lane requires
root-owned `env`, `nix`, `make`, and `rm` from that fixed path. Nix and Make run
below `env -i` with only fixed identity, locale, home, temporary-directory,
publisher-test, Cargo-home, and Cargo-target values. The fresh Cargo home and
target are root-owned mode-0700 directories below a fresh `/var/tmp` root whose
fixed-safe name binds the validated boot ID and 64-hex authorization challenge.
That root must be absent before the campaign creates it and remains outside both
the checkout and admitted VFAT mount. Inherited Cargo, Rust wrapper, Make, or
user configuration variables cannot reach the build.

Before Cargo may read, reuse, or execute anything from that target, the
dedicated Make target performs its own guest-side preflight with fixed absolute,
root-owned system tools. It requires guest UID 0, UEFI, exact current hostname,
machine ID and boot ID, a non-`none` VM classification matching admission, the
root-owned mode-0600 consumed marker, and exactly one mountinfo record binding
the fixed campaign mountpoint to the freshly admitted device number and `vfat`.
It derives the only accepted build root from the current boot ID and the
marker's single 64-hex challenge, then requires Cargo's target and home to be
the exact root-owned mode-0700 children prepared by the campaign. The recipe
changes to the root directory before invoking Cargo with an absolute manifest
path, so Cargo can search only root-controlled `/` and the fresh Cargo home for
configuration; checkout, user-home, and world-writable `/var/tmp` ancestors are
excluded.

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
   internal-only mode so filesystem helpers are not launched. Nix, Make, Cargo,
   Git, and the surrounding SSH session are not externally timeout-wrapped. The
   production publisher retains its own absolute operation deadline.

### GPT ESP/XBOOTLDR profile

The GPT profile uses the same mandatory operator facts, snapshot assertion,
SSH-connection binding, and single-use challenge, but only through its dedicated
targets:

```sh
make disposable-vm-uefi-boot-gpt-topology-challenge
export VM_BOOT_STORAGE_CHALLENGE=the-printed-64-character-value
make disposable-vm-uefi-boot-gpt-topology-admission
export VM_GPT_TOPOLOGY_DESTRUCTIVE_CONFIRMATION="repartition-gpt:$VM_TARGET_STABLE_PATH:$VM_TARGET_DISK_BYTES:$VM_TARGET_DISKSEQ:$VM_EXPECTED_BOOT_ID:gpt-boot-topologies"
make disposable-vm-uefi-boot-gpt-topology-campaign
```

The wrapper owns the exact `gpt-boot-topologies` profile and records it in a
version-2 authorization marker. The base whole-device confirmation is rejected
by this profile, and the GPT confirmation is rejected by the base profile. Only
the final target repartitions the exact admitted disk; challenge and admission
do not mutate that target disk.

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

For the base profile, the VFAT attachment is admitted only with `rw`, `nosuid`,
`nodev`, `noexec`, `nosymfollow`, effective root ownership, file mask `0133`, and directory mask
`0022`. Linux may omit default `uid=0,gid=0` strings from mountinfo, so the
harness proves their effective result from the mounted filesystem root instead
of requiring those optional textual spellings.
After creating the declared parent, the campaign runs the exact ignored
`forge-linux-descriptor-boot-file-publication-vfat-test` Make target with phase
`publish`. The test accepts only a parent strictly below the fixed campaign
mount, a fixed disposable confirmation, and phase `publish` or `revalidate`.
It publishes one fixed sealed payload through the real retained-descriptor
publisher, then requires an immediate idempotent `AlreadyExact` result with the
same inode. The campaign synchronizes, unmounts, remounts, checks parent
persistence, and runs the same exact target with phase `revalidate`; that pass
requires `AlreadyExact`. Both passes verify bytes, length, XXH3, SHA-256,
effective mode `0644`, and absence of private `.cast-payload-*` residue.

For that base profile, the harness checks the admitted VFAT identity and complete mount policy
immediately before and after each fixed publisher invocation. It then performs
the additional filesystem sync and final unmount. This is deliberately not a
generic command or callback hook. The ignored test independently authenticates
the current hostname, machine ID, boot ID, UEFI state, actual virtualization,
SSH-connection hash, and root-owned consumed marker. The marker binds the target
path, stable path, disk sequence, and byte size; after fresh disk admission, the
harness separately passes the resulting device number through the sanitized
publisher environment. Immediately before every production publisher call, the
test requires exactly one mountinfo record for the fixed campaign mountpoint
with that device number, `vfat`, and the complete required mount and superblock
policy.

## Interrupted runs

The challenge is single-use at the destructive boundary. After consumption,
any nonterminal failure preserves the consumed marker and campaign lock even if
formatting returned an error and nothing is mounted. A normal failure
tries to unmount only when the private mountpoint still has the exact admitted
device and policy. It does not force, retry, discover a replacement, or touch
another mount. Ambiguous cleanup leaves the consumed marker and campaign lock
in `/run/cast-vm-boot-storage` so a later invocation fails closed.
The private Cargo home and target are likewise preserved at their challenge-bound
`/var/tmp` path after any failed campaign. Only after a successful final unmount
does cleanup remove their fixed root-owned parent with root-validated
`rm --one-file-system`, prove its absence, and then remove the campaign lock.

`SIGKILL` cannot run shell cleanup, so the same-boot runtime sentinels and any
remaining attachment must be reviewed inside the disposable VM. Power loss or
reboot clears `/run`; after either, the disk is unclassified and the operator
must recover from the VM snapshot rather than infer success from missing
sentinels. There is intentionally no automatic cleanup, reboot, host fallback,
or hypervisor recovery target.

The safe local static checks are:

```sh
make disposable-vm-uefi-boot-storage-harness-test
make disposable-vm-uefi-boot-gpt-topology-harness-test
```

They parse and inspect the harnesses without reading a block device or running
a storage command. Neither is a dependency of `make test`, `make check`, or any
default target.

## Recorded disposable-VM evidence

At exact commit `07b917a73189563f02104455c937613ffe6b2e72`, the guarded
campaign ran inside UEFI guest `test`, machine ID
`556a65c27e9b4150a9fb2b68f8693cdb`, during boot
`e875fab7-b970-4881-89d1-e87aa70acffb`. The live root remained
`/dev/vda2`, and the live ESP remained `/dev/vda1` mounted at `/boot/efi`.
The admitted disposable target was
`/dev/disk/by-path/virtio-pci-0000:07:00.0` resolving to `/dev/vdb`, with
disk sequence `10` and exactly `34359738368` bytes.

The first attempt from commit `a6a834df` failed closed immediately after
formatting: `mkfs.fat` had automatically written a fake whole-device MBR, so
the repeated admission observed `/dev/vdb1` and refused to mount. The consumed
sentinel state was preserved, the exact state was inspected, and the target's
signatures were recovered explicitly. That attempt mounted nothing and did not
reboot the guest. Commit `07b917a7` then disabled that behavior with `--mbr=n`
and added validation of effective root ownership on the mounted filesystem.

After confirming fresh snapshot `os-tools-vdb-retry-20260721-07b917a7`, the
corrected challenge, dry admission, and destructive campaign all passed. The
target contained FAT32 filesystem `CASTTEST`; `EFI/Linux` survived the ordered
sync, unmount, remount, validation, sync, and final unmount. The runtime
authorization root was empty afterward with neither marker nor lock, and
read-only `fsck.fat -n` reported 3 files using 3 of 2,096,126 data clusters.
Neither `/dev/vda2` nor the live ESP was modified.

At exact commit `bc8d8b2682e865117ae6a59fb14eb186ad7e4e8b`, after atomic
external snapshot `os-tools-vdb-publisher-manifest-20260721-bc8d8b26`, the
same guest completed the production leaf-publisher extension against only the
same admitted `/dev/vdb`. The harness staged the exact clean commit privately,
resolved one immutable Nix source, and built, SHA-256-manifested, and
revalidated one fixed Forge libtest runner before `destructive_started`; no Nix
or Cargo command ran after disk effects began.

The campaign formatted FAT32 `CASTTEST`, mounted it under the root-private
runtime directory, and invoked the production retained-descriptor primitive.
It published the exact 45-byte mode-`0644`
`EFI/Linux/cast-vm-publisher-test.efi` with SHA-256
`f204c2a6e2404aad56f08410b6f0751516ffee0c7e502d3cafbfac4233c9a004`.
The first result was `Published`; immediate re-entry returned `AlreadyExact`
for the same inode. After sync, unmount, and remount, the fixed runner again
returned `AlreadyExact`, then the campaign synced and unmounted cleanly.

Independent post-run inspection found VFAT UUID `7DC7-7B1E`, no partition
children, mount, swap, authorization marker, lock, or campaign build root.
Bounded read-only `fsck.fat -n` reported 4 files using 4 of 2,096,126 data
clusters. Root `/dev/vda2` and live ESP `/dev/vda1` at `/boot/efi` remained
unchanged, and no reboot was requested or performed.

This proves one real production BOOT leaf, immediate idempotence, and
sync/unmount/remount persistence on disposable whole-device VFAT. It does not
yet prove aggregate publication ordering and deletion, receipt promotion and
production coordinator wiring, GPT ESP/XBOOTLDR roles, live-ESP mutation,
reboot recovery, interruption recovery, or power-loss durability.

### GPT ESP/XBOOTLDR publication evidence

At exact commit `aae26376f0e6ee9823a0cd5005516186c46c0faf`, after atomic
external snapshot `os-tools-gpt-topology-retry-20260721-aae26376`, the same UEFI
guest and boot completed the guarded GPT challenge, admission, and destructive
campaign against only `/dev/disk/by-path/virtio-pci-0000:07:00.0` -> `/dev/vdb`,
disk sequence `10`, exactly `34359738368` bytes. Root remained `/dev/vda2`, and
the live ESP remained `/dev/vda1` at `/boot/efi`.

Two earlier snapshot-backed attempts failed closed before publishing a leaf and
were explicitly recovered before retry. The `18b9c314` attempt exposed a test
which incorrectly treated Rust `Path::starts_with` as a byte-prefix test for a
challenge-bound directory name; commit `c8b43221` replaced that check with an
exact build-root binding. The `c8b43221` attempt then exposed a genuine Linux
nsfs mountinfo root, `mnt:[4026532758]`, which the generic parser had incorrectly
required to be absolute. Commit `aae26376` admits any nonempty decoded generic
root while the authority-bearing mount selector still requires its selected
root to be exactly `/`. Both failures preserved inspectable authorization state,
and exact recovery left the guest root and live ESP unchanged without rebooting.

The successful campaign first created one 256-MiB GPT ESP and exercised
ESP-as-BOOT. Its production publication created the 47-byte mode-`0644`
`EFI/Linux/cast-vm-gpt-alias.efi` with SHA-256
`0f6965388e00f6deeb67e769a6c81abde7c31666541bc2458bb1cd74cb6224ef`.
The disk was then repartitioned as a 256-MiB ESP plus a distinct 512-MiB
XBOOTLDR. Production publication created the 44-byte mode-`0644`
`EFI/Linux/cast-vm-gpt-distinct-esp.efi` with SHA-256
`a4e064e802a47610f3155407e2fe445f4968b84d2b45e1d3ffeb32a600f557de`
and the 49-byte mode-`0644`
`loader/entries/cast-vm-gpt-distinct-xbootldr.conf` with SHA-256
`492878b78af9460fa68196c6005e1fe916ac8e337c16c6b58343d4aa5cf94c5d`.
For both layouts, initial publication returned `Published`, immediate same-inode
re-entry returned `AlreadyExact`, and the post-sync/unmount/fsck/remount pass
returned `AlreadyExact` before clean final unmounts.

A separately reviewed root-privileged read-only audit, SHA-256
`11ca3712f84996c90a52bf1a82911f4f4ac784187554008aa895c180b82f4b8a`,
then authenticated the exact guest, root, live ESP, target disk, GPT header,
partition count, geometry, role GUIDs, names, sysfs identities, blkid values,
unique PARTUUIDs, and `/dev/disk/by-partuuid` links. In a private mount namespace
it mounted only `/dev/vdb1` and `/dev/vdb2`, read-only, and proved the exact final
trees, payload bytes, lengths, modes, and hashes above. Read-only `fsck.fat -n`
reported `/dev/vdb1: 4 files, 4/516188 clusters` and
`/dev/vdb2: 4 files, 4/130811 clusters`. It finished with
`GPT_SUCCESS_AUDIT_OK exact distinct ESP/XBOOTLDR publications verified`; after
exact temporary cleanup, the corrected root-level check reported
`GPT_SUCCESS_VM_ROOT_AUDIT_OK`. Both target partitions remained unmounted with
no swap, holders, slaves, leaked private mounts, runtime authorization state,
challenge-bound build root, or audit residue. Root, live ESP, and target
identities remained unchanged, and no reboot was requested or performed.

This proves the fixed-path production topology capture and retained leaf
publication for both supported real GPT role layouts, including idempotence,
clean read-only filesystem checks, and unmount/remount persistence. It does not
yet prove aggregate publication ordering, replacement, or deletion; receipt
promotion; journal-coupled forward coordination or startup BOOT repair; selected
kernel/initrd/entry bootability; device-flush survival; interruption or reboot
recovery; or power-loss-equivalent durability.
