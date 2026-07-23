# Future Plan

This file records useful work deliberately excluded from the current
[`PLAN.md`](PLAN.md). Items here must not delay that plan's validation or
repository closure. Moving an item into active work requires a separate
decision after the current plan is finished.

Proposals rejected from the current implementation sequence are deferred here
rather than discarded. Each entry should retain the useful idea, state why it
does not close the current blocker, and define what would make it worth
reconsidering.

This is a backlog, not an implicit extension of the active plan. Safety
violations and dishonest evidence shortcuts are not future features: host-disk
mutation, ambient host or `/nix/store` fixture mounts, mutable untracked recipe
mounts, fake tool shims, and claiming same-boot tests as reboot or power-loss
proof remain forbidden. Everything else listed below is preserved for a later,
explicit scope decision.

## Deferred state-activation closure

On 2026-07-23 the remaining broad Phase 11 system-manager campaign was moved
out of the active plan so the completed Gluon package architecture and the
reviewed ActiveReblit foundation could merge. This is an honest deferral, not
evidence that the following work is complete:

- Migrate fresh-state creation from the legacy
  `client/core/stateful_transition.rs` path into the durable journal
  coordinator. The live call remains
  `client/core/state_planning.rs::apply_stateful_candidate`; existing
  coordinator typestates and recovery foundations do not by themselves prove
  production dispatch.
- Migrate archived-state verification repair from
  `client/verify.rs::repair_archived_state` into its operation-specific durable
  coordinator route. Its established repair tests cover the legacy path, not
  a journal-coordinated live activation.
- Add a direct production-boundary test for the boot-applicable `Ready` branch
  of `client/active_reblit_transition.rs`. Commit `ce060c13` production-wires
  both branches and proves real `Client::verify(true, false)` through the
  no-boot `NotApplicable` branch; the receipt-backed boot branch is presently
  supported by component compositions rather than one complete live-client
  regression.
- Complete real startup boot repair. Current production recovery can preserve
  `BootRepairRequired` evidence and fail closed at terminal
  `BootRepairUnverified`, but it intentionally invokes no repair effect and
  emits no `BootRepairComplete` success.
- Prove forward ActiveReblit cleanup and finalization across genuine process
  interruption, reboot, and power-loss-equivalent boundaries. Existing
  same-boot component and terminal-delete campaigns must not be described as
  reboot or power-loss proof.
- Run the remaining destructive evidence only inside the user-approved
  disposable UEFI VM, never on the host: selected-payload bootability,
  interruption at every persisted boot-publication boundary, reboot recovery,
  and power-loss-equivalent durability. A VM reboot may return to installation
  media and lose SSH access; campaign orchestration must account for that
  before rebooting.
- After those routes and VM campaigns exist, rerun the complete Phase 11 exit
  gate and update the recovery subplans with exact accepted commits, machine
  identity, target-disk identity, logs, hashes, and explicit non-claims.

## Package model

- Evaluate a typed toolchain-free package mode for prebuilt artifacts. The
  current package can declare no compiler or compile step, but repository
  policy still freezes its selected compiler toolchain.
- Design a content-addressed local directory/file source ABI. It must bind file
  type, mode, content, symlink target, and destination before frozen execution;
  mutable recipe-directory mounts remain forbidden.
- Design a fixed-output network ABI only after locked-source execution is
  complete. Reconsider the retained typed network request when an exact output
  digest, bounded transfer policy, isolation contract, and reproducible cache
  semantics can make network access no less explicit than an ordinary source;
  `options.networking = true` remains rejected until then.
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
- Keep a full Nix-language/store or lazy recursive package-set architecture as
  comparative research rather than a presumed destination. Reconsider it only
  through a separate architecture decision if Stone-native packages expose a
  concrete limitation that smaller interoperability layers cannot solve.

## Declarative system and user workflows

- Extend declarative scope beyond package sets and repositories to typed
  services, users, kernel selection, and per-machine composition after the
  package and crash-recoverable activation contracts are complete. A future
  module system must preserve the `/usr` versus `/etc` boundary and atomic
  rollback instead of bypassing them with imperative activation scripts.
- Evaluate multi-version package coexistence, profile generations, and
  reproducible development shells as a separate store/profile design. Revisit
  these when concrete workflows require concurrent closures; do not weaken the
  one-live-tree transaction model accidentally while adding them.

## Language and evaluation model

- Revisit the policy-layer adapter's currently enabled pure array capability
  after neutral identity v2 is authoritative. The present Gluon adapter admits
  it even though the reachable policy-layer ABI does not import it; tightening
  that capability during the equivalence migration would mix a security-policy
  behavior change into identity extraction. Preserve it for now, then remove
  it through a separately versioned evaluator-policy change with explicit
  before/after evidence.
- Reconsider recursive policy-overlay fixed points only if the completed
  one-way `add`, `replace`, and `modify` model cannot express a concrete package
  family without duplication. Any later design must remain finite, explainable,
  and Stone-native rather than importing Nix semantics by default.
- Evaluate automatic package-argument reflection, similar in convenience to
  `callPackage`, only after the explicit `PackageInputs -> PackageSpec` ABI is
  stable. Reconsider it when reflection can preserve typed missing/extra-input
  errors and deterministic provenance instead of hiding dependency selection.
- Research evaluation-time fetching or import-from-derivation only as a
  separately versioned capability with a complete hermeticity, locking,
  recursion, and failure model. It remains outside the pure initial evaluator
  because the current derivation must be frozen before execution.
- Consider explicitly declared global or user policy discovery only if every
  selected layer can be sealed into evaluation identity and reproduced without
  ambient home-directory state. Unrestricted discovery remains inadmissible.
- Add direct Gluon provenance to future structured build steps when those steps
  have a stable evaluated source identity. Until then, diagnostics should keep
  reporting stable positions inside the evaluated script and must not guess a
  source location by scanning the root recipe text.

## Package and archive architecture

- Reconsider replacing Forge provider resolution with another dependency
  solver only as a coherent future architecture migration. A second concurrent
  solver would create two sources of package identity and therefore does not
  belong in the current plan.
- Explore fully structured, shell-free builders if real recipes demonstrate
  that they improve auditability without reducing expressiveness. Explicit
  shell steps remain valid typed data; removing all shell execution is not a
  prerequisite for declarative packages.
- Treat `.stone` archive-format evolution and workspace release/version changes
  as independent, versioned migrations after the current semantic and recovery
  contracts close. They must include readers, upgrade policy, reproducibility
  evidence, and rollback compatibility rather than being folded into Gluon
  adoption.

## Installation and metadata extensions

- Design an authenticated adoption flow for a nonempty unmanaged prior `/usr`.
  The `Unmanaged` transition origin remains representable, but current
  preparation correctly refuses to bless arbitrary unowned content. Reconsider
  it only with a bounded inventory, explicit operator intent, durable ownership
  provenance, and a recovery path that never converts ambiguity into authority.
- Add typed SELinux, IMA, EVM, and security-xattr policy before supporting
  labeled candidate filesystems or labeled local boot-policy inputs. The future
  model must declare labels as package/system data and authenticate them through
  descriptor-rooted inventories; pathname fallbacks and silent label stripping
  remain forbidden.
- Add a read-only repository-manager backend when SQLite can be opened strictly
  in `mode=ro` and installation/cache ownership has an explicit trusted-owner
  model. It must not weaken the writable manager's current lock, mode, or owner
  checks merely to admit unprivileged readers.
- Investigate privilege separation or a stronger kernel isolation boundary for
  mutations currently protected against cooperative same-credential writers by
  authenticated descriptors and locks. Reconsider it with an explicit hostile
  same-UID threat model; do not claim the existing cooperative contract already
  provides a kernel freeze.

## Migration and operator UX

- Consider an explicit offline `cast.boot_topology.v1` to v2 migration helper
  after the v2 contract is stable. It must produce reviewed canonical Gluon and
  never become an automatic runtime fallback or a second accepted ABI.
- Support separately mounted `/etc` or `/etc/cast` only through an explicitly
  supplied, descriptor-authenticated configuration root with a defined
  cross-mount trust model. Current rooted loading correctly rejects crossing an
  undeclared mount boundary.
- Complete canonical Gluon repository-list output when the CLI can preserve
  generated-file ownership and round-trip every typed repository field without
  creating a legacy configuration path.

## Deferred design alternatives

The authenticated receipt/provenance chain, descriptor-safe boot publisher,
restart reconciliation, and VM durability evidence remain required by the
current Phase 11 plan. Only alternative foundations that cannot establish that
authority are deferred here.

- Add automatic pending-receipt boot rollback only after incomplete private
  publication residue has receipt-bound ownership and exact reconciliation.
  Process death while streaming can currently leave a partial immutable
  `.stage` or replacement `.replace` leaf which the exact terminal-state
  reconcilers correctly refuse to adopt or remove. The future protocol must
  bind every such private name to the exact pending receipt and safely
  authenticate, resume, or remove partial residue without widening deletion
  authority. It must also add one conditional database operation which clears
  only the exact pending receipt head while retaining its committed
  predecessor. These are prerequisites for automatic inverse repair across
  every `BootSyncStarted` crash prefix; they are deliberately not required for
  the current safe journal-only `BootRepairRequired -> BootRepairStarted ->
  BootRepairUnverified` closure and manual-recovery retention.

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
- After the operation-specific recovery routes are complete and repeated
  structure is measured, consider an optional generic typed roll-forward and
  exchange-persistence framework. It may factor exact binding capture,
  one-successor persistence, canonical reopen, and durable error
  classification, but must not erase phase-specific sealed authorities or
  weaken operation-specific database and namespace proofs. This is a later
  refactor, not a prerequisite for finishing the explicit current routes.
- Define a separate long-term retention and garbage-collection policy for old
  immutable boot-receipt bodies and preserved corrupt or quarantined wrappers.
  It must specify authenticated reachability, minimum forensic and rollback
  retention, bounded storage policy, operator audit/export, deletion authority,
  and crash-safe reconciliation before removing anything. Until that policy is
  approved, ambiguous evidence stays preserved; this must not substitute for
  cleanup or finalization already required by [`PLAN.md`](PLAN.md).

## Archived research

- Retain the [`cast-core` and platform-backend study](plans/bsd.md) for a
  possible post-plan portability effort. Reconsider it only after the Linux
  activation and recovery contracts are complete and another platform can
  provide equivalent atomicity, confinement, sandboxing, boot, and durability
  guarantees rather than weaker syscall-shaped substitutes.
- Retain the [Lua adapter plan](plans/lua.md) as the next declaration-language
  phase, blocked on complete acceptance of
  [`agnostic_config.md`](plans/agnostic_config.md). Cast remains Gluon-only
  while that foundation is extracted; Lua must connect through the accepted
  adapter rather than create a second loader, manager, identity, or persistence
  path.

## Maintenance

- Repair the stale `forge-ephemeral-candidate-metadata-test` inventory. The
  Make gate still requires the removed test
  `client::postblit::retained_ephemeral::tests::system_container_mounts_usr_and_etc_read_write`,
  while the adjacent transaction and public-root coverage remains present.
  This pre-existing expected-name mismatch is unrelated to declaration-adapter
  extraction and must not delay `plans/agnostic_config.md`.
- Reconcile the stale workspace-package entries in `Cargo.lock` with the
  workspace's current inherited version in a separate release-metadata change.
  Current Make/Cargo runs rewrite existing local package entries from `0.26.6`
  to `0.27.0` and also alter an unrelated `windows-sys` resolution. Foundation
  commits must continue restoring that unrelated churn instead of silently
  folding a repository-wide lockfile rewrite into declaration extraction.
- Audit and remove inappropriate `timeout` wrappers from the remaining Make
  fragments and test helpers. The declaration-core prerequisite fixed only the
  `source-loc` lane; at that checkpoint 117 other files under `misc/make` and
  `misc/scripts` still mentioned `timeout`, including wrappers around Git,
  Cargo, `grep`, `rg`, `awk`, `sed`, `mkdir`, and `rm`. Preserve bounds around
  actual evaluator/application/fixture execution that can hang, but do not
  time-limit compilation or ordinary deterministic helpers.
- Restore workspace rustfmt cleanliness before treating the aggregate
  `make test` gate as green. On merged `develop` at `6c324985`, `make check`
  passed, but `make test` stopped in its `lint` prerequisite because
  `cargo fmt --all -- --check` reported existing drift across Mason planner and
  Stone recipe files; the test body was not entered. Do not hide that result or
  perform an unrequested repository-wide format rewrite.
- Resolve the existing Forge compiler warnings reported by the current Make
  gates, including unused Linux boot imports and variables plus dormant
  coordinator and test-support paths. Warning cleanup is not a blocker for the
  current system-management plan.

## Test operations

- Consider an operator-facing VM hygiene audit for fixture campaigns. It could
  report remaining fixture units and processes, VM test-disk mounts, linger,
  and the Ubuntu AppArmor user-namespace setting without changing the existing
  authenticated fixture receipt or treating host policy as package evidence.
- Add a fail-before-launch VM capacity preflight if delegated campaigns are run
  regularly. It should authenticate persistent build-space and inode headroom,
  keep bounded receipts separate from disposable artifacts, and refuse the
  outer unit before compilation rather than relocate a populated target tree
  between memory filesystems under pressure.
- Standardize long host-side validation on a repository-private temporary root
  and provide a non-destructive stale-artifact report. A saturated per-user
  `/tmp` allocation can prevent the sandbox or LOC gate from starting even when
  the home filesystem has ample capacity; cleanup must never remove unrelated
  user data automatically.

## Declaration evaluator budget rule (Phase 7 part a)

The agnostic-config plan's authoritative evaluator policy asks that "each root or
fragment receives one budget starting immediately before its descriptor-rooted
open/read and ending after typed decode." The concrete part of that rule that is
still open is deferred here; the other two parts are already satisfied:

- **Import cycle rejection (done):** `declarative_config::prepare_module_graph`
  rejects import cycles with a stable import diagnostic before any runtime is
  created (`feat(config): reject configuration import cycles`).
- **Per-fragment budgets (already held):** each discovered fragment is evaluated
  through its own `DeclarationEvaluator::evaluate` call, so shadowed fragments
  already retain separate budgets and are still evaluated.
- **One budget spanning read -> decode (deferred):** `declarative_config`'s
  `evaluate_file` already starts the deadline immediately before its
  descriptor-rooted read, so direct file evaluation spans read through decode
  under a single budget. The generic storage loaders
  (`config::declaration::fixed_root_loader` and the fragment set) instead read
  through `SourceRoot::load` and then call the typed adapter's `evaluate`, which
  starts a fresh deadline. The bounded read therefore sits just outside the
  evaluation budget.

**Why this does not block closure:** `SourceRoot::load` reads a single
descriptor-rooted file bounded by `max_source_bytes`, so the read cannot hang and
the practical budget is effectively one deadline already. No valid evaluation
identity depends on this (the deadline is not hashed), so it is a hardening
refinement, not a correctness gap.

**What it would take:** thread one caller-established `EvaluationDeadline` from
the storage loaders through the typed boundary — e.g. an `evaluate_within(source,
deadline)` method on `DeclarationEvaluator`/`EngineAdapter` with a default that
delegates to `evaluate`, overridden by the Gluon adapters to reuse the passed
deadline via the existing internal `evaluate_with_inputs_until`. That touches the
trait and roughly twelve domain adapters, so it is a deliberate, self-contained
follow-up rather than an extraction-adjacent change.

**Optional companion:** if a literal `EVALUATOR_POLICY_VERSION` increment is ever
wanted to signal the cycle-rejection policy numerically, bump it together with
this change so derivation caches invalidate exactly once. Cycle rejection is
currently carried as part of the initial neutral evaluator policy, so no bump has
been spent on it.
