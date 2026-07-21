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
