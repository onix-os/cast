# Private minimal `/dev`

Frozen builds and transaction triggers need ordinary Unix device semantics
without inheriting the host device namespace. `DevPolicy::Minimal` therefore
means exactly three private character-device inodes:

| Name | Linux identity | Required behavior |
|---|---:|---|
| `null` | `c 1:3` | writes succeed; reads return EOF |
| `zero` | `c 1:5` | reads return zero bytes |
| `full` | `c 1:7` | writes fail with `ENOSPC` |

There is deliberately no entropy source, terminal, device discovery, host
`/dev` mount, or optional device list. The three names are executor policy,
not recipe-controlled inputs.

## Why read-only host binds are insufficient

A read-only bind protects the device inode's metadata and still permits normal
`read(2)` and `write(2)` data operations. It does not implement the complete
Unix open contract: an existing device opened with `O_CREAT` is rejected. This
breaks ordinary APIs such as Python's `Path(os.devnull).open("wb")` and common
installers which use that API.

Making the host bind writable fixes `O_CREAT`, but it also exposes the ambient
inode. A root-mapped payload could change its mode, owner, timestamps, or
extended attributes. Even an unprivileged caller can update some metadata on
world-writable devices. That violates the frozen execution rule that a build
cannot mutate undeclared host state or vary with ambient device metadata.

The implementation must not fall back to writable host binds.

## Provisioning boundary

Linux does not permit a rootless user namespace to create usable character
devices. A narrow initial-user-namespace privilege boundary is therefore part
of minimal-device provisioning, just as the existing fixed `newgidmap`
boundary is part of isolated supplementary-group removal.

For each execution, the privileged provider:

1. creates one detached, bounded tmpfs;
2. creates exactly the three fixed root-owned mode-`0666` character devices;
3. clones each device into its own writable detached file mount;
4. seals the never-attached scratch root `ro,nosuid,nodev,noexec` and closes its
   only directory descriptor; and
5. returns exactly three close-on-exec file-mount descriptors.

Each inode deliberately retains exactly one link inside that unreachable
scratch root. Linux returns `ENOENT` when `move_mount(2)` is asked to move an
unlinked detached file mount. The root was never attached to a pathname in a
reachable/current mount namespace and is never sent through the broker, so there is no
namespace-reachable cleanup pathname, shared device pool, or reusable inode.
The exact 64-KiB/four-inode ceiling and zero-free-inode readback bound the
otherwise hidden filesystem to its root plus these three names.

Every container activation obtains the descriptors from the fixed Cast device
broker, including callers whose effective UID is zero. UID zero alone does not
prove initial-user-namespace authority, so activation has no direct shortcut.
The system service runs the fixed provider with the required authority and
accepts no device identity, path, mode, mount option, or count from the client.
The provider remains directly exercised only by its privileged VM test.

## Descriptor protocol

The rootless exchange uses one bounded Unix `SOCK_SEQPACKET` transaction. The
client requires:

- a root peer authenticated by `SO_PEERCRED`;
- the exact supported protocol version and packet length;
- exactly three descriptors received with `MSG_CMSG_CLOEXEC`;
- no data or control truncation and no extra ancillary messages;
- distinct, single-linked, mode-`0666` character inodes with identities `1:3`,
  `1:5`, and `1:7` in canonical order;
- tmpfs backing and writable detached file mounts; and
- provider-created bounded tmpfs provenance with no free inodes, authenticated
  through the fixed root broker construction rather than an ambient pathname.

Every timeout, malformed response, missing broker, peer mismatch, validation
failure, or unsupported kernel fails before the container is cloned. There is
no ambient compatibility path.

## Child assembly and lifetime

The supervisor acquires the linear descriptor capability before clone. In the
single trusted setup child, the final `/dev` directory is authenticated before
the fresh bounded parent tmpfs is attached. Only after that attachment does the
child create the three controlled placeholders and move one private writable
file mount onto each fixed descriptor-relative name. Each visible name is
reopened and required to expose the exact validated private inode, and the
parent must have no remaining inode headroom. The child then marks only the
parent tmpfs mount read-only, non-recursively. Consequently:

- the payload cannot create, remove, rename, or replace device names;
- `O_WRONLY | O_CREAT | O_TRUNC` retains normal device behavior;
- metadata changes affect only disposable private inodes;
- the host device inodes are never mounted into the container; and
- all setup descriptors are dropped before the payload descriptor table is
  sanitized and arbitrary code begins.

Exact initial-namespace `0:0` ownership is authenticated by the supervisor
before clone. Child-side revalidation deliberately checks only
namespace-invariant facts: a root-owned inode appears under the kernel's
overflow identity after a rootless `0 -> caller` ID map, while its type,
device number, mode, links, backing tmpfs, mount flags, and descriptor flags
remain stable.

The capability is deliberately non-`Clone`. A setup error after parent
attachment may leave a partial tree only inside the disposable child's private
mount namespace; payload code is never entered and child exit reclaims it. A
single retained device mount pins the tiny backing tmpfs and all three linked
inodes, but grants no path to its siblings. The whole bounded per-execution
fixture is reclaimed after the last descriptor or attached mount closes.

## Required evidence

Host-safe tests cover protocol framing and pure validation without mutating a
device until private provenance has been established. Disposable-VM tests must
also prove:

1. direct privileged provider execution and ordinary-user broker acquisition;
2. attachment of initial-user-namespace-created detached mounts inside the
   mapped child namespace on the Linux 5.14 floor;
3. pathname and descriptor-anchored container activation;
4. the exact three-name inventory and `EROFS` for a fourth name;
5. Python-shaped null opens plus null, zero, and full data behavior;
6. different private inode identities across executions;
7. unchanged canonical host-device identity and metadata;
8. the sealed scratch root is absent from the three-descriptor response, its
   single links cannot be traversed from a device mount, and no inode headroom
   remains;
9. bounded failure under truncated packets, extra descriptors, peer failure,
   broker death, client death, and provider interruption; and
10. no leaked descriptor, mount, process, socket, or reusable staging object.

The required delegated preflight performs the device contract before any
bootstrap closure is downloaded. A successful preflight is necessary but does
not replace the complete repeated fixture matrix.

## Nix inspiration and deliberate difference

Nix's Linux sandbox constructs a small `/dev` and bind-mounts a standard host
device set into it. That confirms the importance of normal device behavior for
real package builds. Cast retains the small explicit inventory, but uses fresh
private inodes because its frozen plan and transaction-trigger contracts also
forbid undeclared host mutation and ambient metadata dependence.

Reference inspected during this design:
[`linux-derivation-builder.cc`](https://github.com/NixOS/nix/blob/bebd2f851a304e9fb2e143ce0cbeff577c6a37ac/src/libstore/linux/build/linux-derivation-builder.cc).
