# Delegated fixture execution

This subplan records the detailed evidence behind Phase 10 of
[`PLAN.md`](../../PLAN.md). The canonical Phase 10 checklist and exit gate
remain in that file.

## Matrix inventory

By 2026-07-19, the matrix contains twenty-six fixtures spanning
standard/custom builders, mixed archive/Git/raw sources, generated payloads,
an empty userspace profile, plugin/split outputs, localization,
system/desktop integration, fonts, a vendored Go module, an offline PEP 517
Python wheel, and a CMake/CTest executable checked against an independently
locked raw vector corpus.

Commit `4c59473d` adds a self-authored Regular/Bold family as a deterministic
30,720-byte USTAR with SHA-256
`8710f0728fbde240fd94ce8bce46c4e4d71336b8470416e8da7c0895dc2d700c`.
Its exact three-leaf `out` contains both TTFs and OFL at mode `0644`; its
closure is 63 packages and 213,892,544 bytes, caches are forbidden, and no
runtime relation is invented.

Commit `b0f16ef1` adds a pinned, vendored, network-disabled Go module whose
one-output static ELF has no runtime relation. Its exact 71-package closure
adds only Go to the userspace baseline.

The Python fixture binds build, installer, setuptools, pytest, interpreter,
and typing-extension roles to an exact 76-package, 214,660,406-byte closure.
Its hostile-host proof rebuilds and executes the wheel in disposable roots,
but remains supplemental rather than delegated Stone execution.

The external-test-vectors fixture independently locks a deterministic primary
USTAR and raw JSON corpus, admits that corpus only through a declared
pre-check Bash/`cp` capability, and forbids it from the one-output Stone. Its
disposable supplemental host proof does not replace live delegated execution.

All fixtures union to an exact 175-package, 385,535,265-byte bootstrap pool.
Offline and hostile-host contracts pin bytes, modes, providers, behavior,
metadata, and syntax without claiming host deployment, a transaction, or
rollback.

## Live-run history

An optional live run classified supplementary-group `setgroups` `EPERM`
before package execution. No Stone was emitted, decoded, or reproduced, so it
closed no supported-host live-evidence item.

The next disposable-VM run passed the exact production
systemd/cgroup/user-namespace preflight, materialized the complete 172-Stone
bootstrap root, and built the Python fixture's real wheel. Its installer then
opened `/dev/null` with `O_WRONLY | O_CREAT | O_TRUNC` and received `EACCES`.
The existing read-only device bind admits direct device reads, writes, and
truncate-shaped opens, but Linux rejects the ordinary `O_CREAT` shape.

Making ambient host-device mounts writable was rejected. It would expose host
inode metadata to a root-mapped payload and make frozen execution depend on
undeclared ambient state. The required correction is the
[private minimal-device boundary](../architecture/private-minimal-dev.md):
three fresh namespace-unreachable device mounts per execution, provided through a narrow
initial-user-namespace privilege boundary and installed beneath an immutable
three-name `/dev`.

Until that boundary and its ordinary-user VM proof land, the Python run is a
useful real-build diagnostic but not accepted execution evidence. It emitted
no Stone and closes none of the canonical Phase 10 live items.

## Acceptance rule

Only one complete, non-skipped `make fixtures-ci` run from the accepted commit
may publish the bounded v2 receipt described in the canonical checklist. The
receipt must cover both executions of every selected fixture, every decoded
bundle and manifest, and the exact cross-run artifact ledgers. Focused tests,
host-side builds, a production preflight, or an incomplete matrix remain
diagnostics rather than substitutes for that evidence.
