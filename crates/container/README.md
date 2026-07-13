<!--
# SPDX-FileCopyrightText: 2023 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0
-->

# container

container is a crate to start a basic rootless container. Its goal is to create a safe and isolated environment where to build and/or test packages. Environment isolation is desired in that:

  * We don't want to pollute the *host* system with temporary artifacts
  * We want to work in a reproducible environment, for everyone (~~"it worked on my machine!"~~)
  * Certain applications may run arbitrary code while building. We want to be able do to that securely.

Emphasis is put on **rootless** container. Root permission must not be necessary, both for security and to reach a broader audience (including parent rootless containers, like Docker or Podman!).

## Containerization logic

Below is a walkthrough to build and enter a container. The logic is simple but dives into some technical details of Linux. Take your time to understand it. If you're feeling brave, you may try to reproduce the logic in a terminal using the [unshare](https://man7.org/linux/man-pages/man1/unshare.1.html) command.

  1. `clone` a child process. This is like a `fork` on steroids: We can pass it flags to avoid sharing certain resources with the parent, e.g. PIDs, network access, or even user/group IDs, which is the foundation of a rootless container. Let's call the child process "Bill".
  1. Bill has no idea who are the users and groups! It lives within a new user namespace. We must make it wait to build/test a package until we assigned some. To pause the execution, we use a pipe: the parent on one end, Bill on the other. A `read` blocks until there's data coming out of the pipe.
  1. Parent-side, namespace user and group 0 are mapped to the caller. Linux does not let an unprivileged mapper both deny `setgroups` and then clear the supplementary groups inherited by Bill. For an unprivileged caller we therefore use the fixed `/usr/bin/newgidmap` helper plus one delegated `/etc/subgid` entry. That host ID is mapped only to a fixed, setup-only namespace slot.
  1. Once the maps exist, Bill clears its complete supplementary-group list and verifies that its real/effective UID and GID are all zero before it mounts or runs anything. Rootful and rootless payloads consequently receive the same build-visible starting credentials; the caller's host groups are never inherited by build commands.
  1. With Bill's identity crisis over, the parent writes a sentinel value into the pipe that makes the `read` function unblock.
  1. Bills has an isolated user and group, but not an isolated filesystem. Since we are now privileged inside it, we're able to choose a directory as the root directory, and bind-mount important filesystems like tmpfs, procfs and sysfs. We also bind-mount devices like `/dev/zero` and `/dev/urandom`. Finally, we perform a `pivot_root` to hide the host filesystem for good.
     - `pivot_root` is more secure than the well-known `chroot`, in that it's not possible to escape it.
  1. Bill finally builds/tests the package as the parent waits for it to finish.

## Pseudo-filesystem policy

`Container::pseudo_filesystems` controls the container's `proc`, `tmp`, `sys`,
and `dev` mounts. The default keeps the historical behavior: writable proc,
an empty tmpfs at `/tmp`, and writable recursive host views of `/sys` and
`/dev`.

Callers can disable individual mounts, make proc or the host trees read-only,
or choose `DevPolicy::Minimal`. Minimal device policy creates a fresh tmpfs at
`/dev` and exposes exactly the read-only `null`, `zero`, and `full` nodes. It
does not conditionally add devices from host state and never exposes the full
host `/dev` tree.

`Container::loopback` separately controls loopback setup. Its compatibility
default invokes `/usr/sbin/ip` when that host utility exists. Deterministic
callers select `LoopbackPolicy::KernelDefault`, which leaves the interface in
the state supplied by the selected network namespace and performs no host
filesystem probe or setup command. The parent/child synchronization pipe is
created close-on-exec, so it is never inherited by commands run in a payload.

#### References

[`clone` syscall](https://linux.die.net/man/2/clone). [Linux namespaces](https://man7.org/linux/man-pages/man7/namespaces.7.html). [Linux user namespaces](https://man7.org/linux/man-pages/man7/user_namespaces.7.html) (a particular case of namespace). [`pipe`](https://linux.die.net/man/2/pipe). [`newgidmap`](https://man7.org/linux/man-pages/man1/newgidmap.1.html). [`pivot_root`](https://linux.die.net/man/8/pivot_root).

## FAQ

#### Why don't you just use Docker, or Podman, or... ?

Those are full-fledged containerization solutions. They offer many features we don't need, and they have to be set up or installed first. Mason owns the namespace and isolated-root setup directly; rootless execution uses only the standard, fixed-path `newgidmap` privilege boundary needed to clear inherited groups safely.
