# Future Plan

This file records useful work deliberately excluded from the current
[`PLAN.md`](PLAN.md). Items here must not delay that plan's validation, VM
evidence, or repository closure. Moving an item into active work requires a
separate decision after the current plan is finished.

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
