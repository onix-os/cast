# Delegated fixture execution

This subplan records the detailed evidence behind Phase 10 of
[`PLAN.md`](../../PLAN.md). The canonical Phase 10 checklist and exit gate
remain in that file.

## Matrix inventory

By 2026-07-20, the matrix contains twenty-eight fixtures spanning
standard/custom builders, mixed archive/Git/raw sources, generated payloads,
an empty userspace profile, plugin/split outputs, localization,
system/desktop integration, fonts, a vendored Go module, an offline PEP 517
Python wheel, and a CMake/CTest executable checked against an independently
locked raw vector corpus. The two newest contracts add a genuine two-stage PGO
workload and a source-less typed-relation policy fixture.

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
pre-check Bash/`cp` capability, routes analyzer-generated build-ID symbols to
an explicit non-manifest `dbginfo` output, and forbids the corpus from both
emitted Stones. Its
disposable supplemental host proof does not replace live delegated execution.

All fixtures union to an exact 179-package, 388,713,448-byte bootstrap pool.
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
undeclared ambient state. That diagnostic instead required the
[private minimal-device boundary](../architecture/private-minimal-dev.md):
three fresh namespace-unreachable device mounts per execution, provided through
a narrow initial-user-namespace privilege boundary and installed beneath an
immutable three-name `/dev`. The incomplete Python run emitted no Stone and
closed none of the canonical Phase 10 live items.

At exact commit `10d51fb9`, a canonical required campaign on the disposable
NixOS VM completed both required executions for nineteen of the twenty-six
fixtures. In particular, `hooks-patch` passed both executions, including its
pre-setup hook. Fixture twenty, `multiple-sources`, passed its first execution
and emitted nine Stones. Its second execution was terminated when the inner
delegated unit reached the runner's former fixed two-hour runtime limit.

No fixture assertion failed. The enclosing campaign nevertheless failed
closed: it published no v2 receipt because all twenty-six fixtures had not
completed. The VM was safely cleaned of campaign units, processes, mounts, and
broker state without a reboot. This is useful partial execution evidence, but
it does not close any checklist item that requires the complete matrix receipt.

A later clean rerun from exact commit `7b3770b1` proved the new nested runtime
budgets but exposed a separate guest-capacity failure before delegated preflight
or fixture selection. The outer six-hour unit started correctly; the
pre-fixture `make fixtures-ci` contract-test build then filled the NixOS live
root tmpfs. `ld.lld` terminated with `SIGBUS`, the bounded log writer reported
`ENOSPC`, and the campaign closed with zero of twenty-six fixtures and zero of
fifty-two executions. It published no receipt. Its owned units, processes,
mounts, and run staging were removed before the failed recovery experiment.

Moving an already-populated Cargo target between two memory filesystems was
not a valid recovery: the duplicate allocation caused guest OOM pressure and
loss of SSH service. No block device or installed-system disk was modified, and
the VM was not rebooted or reset. The guest's post-relocation boot identity,
filesystem headroom, partial relocation state, and cleanup are not currently
authenticated, so another run on that boot is unsafe. This incident is environment
evidence, not package evidence. A replacement campaign must prove sufficient
persistent build-space and inode headroom before compilation, keep bounded
evidence separate from disposable build artifacts, and fail before launching
the outer unit rather than attempting a cross-tmpfs relocation after pressure.

At exact commit `99c66ada1ae33fa9918066dec2f445032cfe2a97`, the approved
persistent UEFI Ubuntu VM completed the required `all` selection without a
capability skip. The repository's canonical validator accepted the regular,
caller-owned, mode-`0644`, single-link v2 receipt from a clean checkout. It
records 28 fixtures, 56 executions, 84 bundle validations, 134 Stones, 56
manifests, and 190 artifacts totaling 1,396,417 bytes. Every fixture retained
identical first/repeat plans and derivation IDs, a written-then-unchanged exact
build lock, published-then-reused outcomes, and three identical bundle-ledger
observations.

The userspace-profile fixture emitted one 470-byte empty-root Stone. Its two
plans have the same derivation ID
`7ad3cc4c32d148d946ebc739fe0e5ee86889ea3fe92bb5cc4628c0c549654d6b`;
its decoded expectations prove exactly `bash`, `uutils-coreutils`, `findutils`,
`ca-certificates`, and `xz` as runtime relations. The receipt is 103,612 bytes
with SHA-256
`e50e1f6f156a2bcd9747d5686b8994d7ad1e636747099e28afdd2148e5f1b532`.
The mode-`0600` execution log is 485,372 bytes with SHA-256
`f6ba0046a1cc17762b6aae5eb85047b025a6c702c61b525c88cdc57d3712629f`.

The enclosing system service exited successfully. Its cleanup restored linger
to `no` and `kernel.apparmor_restrict_unprivileged_userns` to `1`; no
`/dev/vdb` mount, fixture unit, or fixture process remained, and the checkout
was clean at the exact tested commit. Copies of the receipt and log are kept in
the host checkout's ignored `target/fixture-evidence/vm-99c66ada/` directory.
This proves delegated package execution and deterministic reproduction. It is
not ESP/BOOT publication, activation, rollback, reboot, power-loss, or signed
remote-attestation evidence.

## Runtime containment contract

Accepted runtime commit `249b5c8b` replaces the equal nested deadlines exposed
by that campaign with one shared, tested budget hierarchy:

- delegated preflight retains a 30-second service runtime;
- one named fixture retains a 7,200-second service runtime;
- `all` defaults to 14,400 seconds and accepts an explicit bounded maximum of
  18,000 seconds; and
- the enclosing evidence service defaults to, and is capped at, 21,600 seconds.

At the largest accepted inner runtime, the ordinary outer limit therefore
retains exactly 3,600 seconds for preparation, client stop/reap, validation,
evidence publication, and cleanup. The outer runner explicitly passes the
validated inner runtime to the delegated runner instead of allowing the inner
unit to rediscover an unrelated default.

Client and status waits are derived rather than hard-coded. For a service
runtime `R`, kill-after `K`, completion margin `C`, and five-second status
delivery margin, the status deadline is exactly `R + 2K + C + 5`. The
completion margin is five seconds for preflight, sixty seconds for delegated
fixtures, and ten seconds for the outer campaign. With the ordinary kill-after
defaults, the resulting preflight, named-fixture, default-`all`, and outer
status deadlines are respectively 50, 7,325, 14,525, and 21,675 seconds. With
the accepted maximum runtime and 300-second kill envelope, the inner and outer
status caps are respectively 18,665 and 22,215 seconds.

An explicitly shorter outer deadline remains valid for bounded fault-injection
and cleanup tests. It deliberately may expire before the requested inner
runtime and therefore cannot produce accepted complete-campaign evidence. The
successful twenty-eight-fixture campaign above stayed within the accepted
hierarchy on a capacity-proven persistent guest.

## Acceptance rule

Only one complete, non-skipped `make fixtures-ci` run from its exact accepted
commit may publish the bounded v2 receipt described in the canonical
checklist. The accepted receipt above covers both executions of every selected
fixture, every decoded bundle and manifest, and the exact cross-run artifact
ledgers. Focused tests, host-side builds, a production preflight, or an
incomplete matrix remain diagnostics rather than substitutes for that
evidence.
