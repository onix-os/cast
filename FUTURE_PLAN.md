# Future Plan

This file records useful work deliberately excluded from the current
[`PLAN.md`](PLAN.md). Items here must not delay that plan's validation, VM
evidence, or repository closure. Moving an item into active work requires a
separate decision after the current plan is finished.

Proposals rejected from the current implementation sequence are deferred here
rather than discarded. Each entry should retain the useful idea, state why it
does not close the current blocker, and define what would make it worth
reconsidering.

## Package model

- Evaluate a typed toolchain-free package mode for prebuilt artifacts. The
  current package can declare no compiler or compile step, but repository
  policy still freezes its selected compiler toolchain.
- Design a content-addressed local directory/file source ABI. It must bind file
  type, mode, content, symlink target, and destination before frozen execution;
  mutable recipe-directory mounts remain forbidden.
- Revisit reusable package-specific wrapper helpers only if repeated recipes
  demonstrate a real abstraction. Do not copy Nix helper APIs by name or make
  future Nix interoperability impossible.

## Execution evidence

- Add a contentful prebuilt-ELF fixture for
  `prebuilt-elf-runtime-contract`: execute the locked upstream binary, verify
  its exact interpreter and `DT_NEEDED` relations, split debug data, and prove
  byte-identical rebuilt Stones. This is additional coverage, not a substitute
  for the current 28-fixture completion gate.

## Recipe expansion

- Consider more Stone-native Gluon recipe patterns after the current
  64-example corpus and system-management plan are complete. New examples must
  fill a demonstrated semantic gap and include honest check/freeze versus live
  execution evidence.

## Interoperability

- Evaluate Nix-to-Gluon or evaluated-derivation interoperability separately.
  Compatibility remains undecided: it is neither a current objective nor
  prohibited, and must not reshape the Stone-native package model in advance.

## Deferred design alternatives

- Reconsider a standalone, authority-free
  `active_reblit_publication_ownership` policy module after authenticated boot
  publication provenance exists. The proposed module would distinguish
  `BorrowedFirstAdoption` from claimed `PublishedByCast` records, preserve
  bounded ordering and deadline checks, and keep decoded, self-consistent,
  borrowed, and first-adoption values non-deleting. It is deferred because an
  authority-free value alone cannot close the current requirement for durable
  provenance bound to the exact journal record and ESP/XBOOTLDR identity. It
  may become useful later as a read-only codec or policy surface, but it must
  never mint deletion authority by itself.
- If that module is revisited, keep its error and function-named test splits
  separate (`authority_separation`, `first_adoption`, and
  `bounds_and_deadlines`) and retain the proposed structural guard against
  filesystem, descriptor, write, rename, unlink, and delete APIs. Reuse it only
  when those types remove duplication from the real authenticated publication
  path rather than adding another unconsumed foundation.

## Maintenance

- Resolve the existing Forge compiler warnings reported by the current Make
  gates, including unused Linux boot imports and variables plus dormant
  coordinator and test-support paths. Warning cleanup is not a blocker for the
  current system-management plan.

## Test operations

- Consider an operator-facing VM hygiene audit for fixture campaigns. It could
  report remaining fixture units and processes, VM test-disk mounts, linger,
  and the Ubuntu AppArmor user-namespace setting without changing the existing
  authenticated fixture receipt or treating host policy as package evidence.
- Standardize long host-side validation on a repository-private temporary root
  and provide a non-destructive stale-artifact report. A saturated per-user
  `/tmp` allocation can prevent the sandbox or LOC gate from starting even when
  the home filesystem has ample capacity; cleanup must never remove unrelated
  user data automatically.
