# Implementation Plan: FUTURE_PLAN.md closure

Concrete, sequenced plan to implement every item in
[`../FUTURE_PLAN.md`](../FUTURE_PLAN.md). Grounded in four codebase-research
passes (boot-recovery, package/build/archive, evaluator/config/installation,
maintenance/test-ops). Sequencing is **durability-gap-first** by explicit
decision; the two most dangerous items (real boot repair, reboot/power-loss VM
campaigns) are **in scope with full care** — gated behind prerequisites, proven
only in the disposable VM, and each dangerous step confirmed before execution.

The forbidden list from FUTURE_PLAN.md stays forbidden throughout: no host-disk
mutation, no ambient host/`/nix/store` fixture mounts, no mutable untracked
recipe mounts, no fake tool shims, and no same-boot test described as reboot or
power-loss proof.

Legend — **E** effort (S/M/L/XL), **R** risk (low/med/high/critical),
**D** = open decision I need from you before that item starts.

### Resolved decisions
- **D2.3 → prerequisites first.** Real boot repair lands the receipt-bound
  partial-residue authentication + exact-pending-receipt-head DB op *before* any
  boot-mutation effect; boot mutation is confirmed with the user before wiring.
- **D3.1 → strict.** Toolchain-free means *zero* compiler tools in the locked
  closure/identity (analyzer objcopy/strip stay, selected separately); a `Shell`
  step invoking `cc` in a toolchain-free package is a validation error.
- **D4.3 → accept global bumps.** Keep the single global
  `EVALUATOR_POLICY_VERSION`; a policy tightening re-fingerprints all Gluon
  evaluations. No per-adapter policy id.
- **D6.x → design sketch + trigger.** Phase 6 items stay concrete design specs
  with explicit reconsideration triggers; no speculative implementation.

---

## Phase 0 — Safe prerequisites (unblock trust; do first)

These are low-risk and clear the ground so later phases are trustworthy. None
touch boot behavior.

### 0.1 Fix the stale boot-repair phase-matrix test fixture  · E:S R:low
The failing `startup_reconciliation::tests::startup_reconciliation_database_phase_matrix_is_exact`
is a **stale test fixture**, not a broken feature. The boot-publication-receipt
feature is live/wired (commit `72ddcb8c`); the test helpers
(`startup_reconciliation/tests.rs` `record_at`/`creation_record`/
`boot_repair_complete_database_record`) fabricate `BootSyncStarted`+ records by
direct phase assignment, omitting the now-mandatory `boot_publication_receipts`.
**Fix:** route those helpers through `boot_sync_started_successor(receipt_pair)`
using the existing `startup_recovery/test_support.rs:624` receipt helper. Test
only; no production change. Do this first so the phase-matrix invariant is
trustworthy before Phase 1/2.

### 0.2 Fix the stale `forge-focused-tests.mk` make gate  · E:S R:low
`misc/make/forge-focused-tests.mk:320` requires the removed test
`system_container_mounts_usr_and_etc_read_write`. Confirm the current
same-scope test name under `client/postblit/retained_ephemeral` and replace the
line (or delete it if the behavior is covered by the surviving
`transaction_container_mounts_usr_read_write_and_etc_read_only`).
**D0.2:** was that test renamed or genuinely removed (coverage gap)?

### 0.3 flake.nix formatting/toolchain pin  · E:S R:low
Per your decision: keep it in `flake.nix`, no `rust-toolchain.toml`/`rustfmt.toml`.
Pin the rust version in the flake devShell to an exact release (instead of the
moving `rust-bin.stable.latest`) and add the rustfmt config there (a
`rustfmt.toml`-equivalent written by the flake, or a flake-set `RUSTFMT`
config) that matches the existing wide-line house style so the current code is
"formatted" with minimal churn. This stops the workspace-wide (1,321-file) drift
permanently without a reformat sweep. No `cargo fmt --all` rewrite.

### 0.4 Resolve the 18 forge compiler warnings  · E:S R:low
`cargo fix --lib -p forge` handles ~10 (unnecessary qualifications in
`state_queries.rs`/`error.rs`, unused import in `mount_namespace/attachment.rs`,
unused vars). The 8 dead-code items are dormant boot-publication/coordinator
scaffolding (`boot_publication/receipt_body.rs`,
`db/state/boot_publication_receipt_head.rs`, `journal_usr_exchange_authority.rs`,
`transition_identity/tree_lifecycle.rs`). **D0.4:** these are forward
scaffolding for Phase 1/2 (NewState wiring) — keep with `#[allow(dead_code)]` +
rationale (recommended, they'll be consumed in Phase 1) rather than delete.

### 0.5 Repository-private temp root for host-validation scripts  · E:S R:low
The LOC gate (`check-source-loc.sh:36`) and ~11 host scripts fall back to
`/tmp`, risking startup failure under `/tmp` saturation. Add a shared shell
helper resolving a repo-private root (mirror `execution-fixtures.mk`'s
`$(TOP_DIR)/target/...` pattern), change the `${TMPDIR:-/tmp}` fallbacks to it,
plus a non-destructive stale-artifact report scoped to `cast-*` prefixes.

### 0.6 (drop) Cargo.lock churn — already resolved on `develop`; no action.

**Exit Phase 0:** the workspace test suite has no stale-fixture/stale-gate
failures, warnings are triaged, formatting is pinned, and host gates start
reliably.

---

## Phase 1 — Durability closure (the biggest real gap; leads by your decision)

Today **only ActiveReblit is journal-durable.** `NewState` and `ActivateArchived`
run with **no journal at all** — a crash mid-transition survives no reboot,
recoverable only by same-boot in-process logic. The coordinator's own module doc
names durable startup reconciliation as the precondition for live wiring. This
phase closes that.

### 1.1 NewState → durable journal coordinator  · E:XL R:critical
**State:** `new_state` (`state_planning.rs:140`) → `apply_stateful_candidate`
(`stateful_transition.rs`) does the full `/usr` exchange + triggers +
previous-archive + `boot::synchronize` with **no `TransitionRecord`**. The
coordinator has the durable NewState typestate *prefix*
(`Preparing → FreshStateAllocating → … → CandidatePrepared`,
`journal_coordinator/mod.rs:379`, `candidate_preparation.rs`) + the shared
trigger/exchange/system-trigger typestates, but the only production driver is
`execute_active_reblit_forward` (ActiveReblit-specific).
**Approach:**
1. Refactor the ActiveReblit boot-publication chain (`complete_active_reblit_boot`,
   `active_reblit_transition.rs:173`) into an **operation-neutral** helper.
2. Build `execute_new_state_forward` mirroring `active_reblit_forward.rs` but
   driving `begin_transition(Operation::NewState)` + `begin/finish_fresh_allocation`
   (DB row correlation) + `archive_previous` + the neutral boot-publication suffix.
3. Add **forward roll-forward startup dispatch** for committed NewState phases in
   `startup_gate.rs::enter` (today only ActiveReblit forward-dispatches;
   `usr_rollback_new_state` covers only the rollback direction).
4. Switch `new_state` to the new route behind the same `StatefulCandidate`
   capability. Keep the legacy path until the coordinator route passes the full
   crash matrix.
**D1.1:** does NewState reuse the ActiveReblit boot-publication receipt machinery
verbatim, or does archiving a real predecessor need distinct receipt/rollback
semantics? Does the fresh-allocation DB edge need a new forward startup authority
(paired with the existing `usr_rollback_fresh_db_invalidation` reverse route)?
**Risk note:** getting `archive_previous` + receipt binding wrong on the fresh
path can leave a system unable to roll back to its predecessor.

### 1.1a NewState pre-allocation boot applicability (D1.4)  · E:M R:high

**Decision D1.4 (user, 2026-07-25): assess applicability pre-allocation from the
selections.** Do not let the journal assert `run_boot_sync` and then finish
without boot, and do not move row allocation back out of the journal.

**Why this is needed to wire Slice 5.** `apply_new_state_candidate` currently
hardcodes `run_boot_sync = true` and turns a non-bootable candidate into a hard
error (`NewStateBootNotApplicable`, `new_state_boot_transition.rs`). Making it
the default would regress every package install whose candidate publishes no
kernel — the legacy route simply skips boot for those. ActiveReblit has no such
problem: it assesses applicability pre-journal and passes the resulting
`run_boot_sync` into the coordinator (`active_reblit_transition.rs:88-114`).
NewState cannot copy that directly because its state row is allocated *inside*
the journal (Slice 1's `add_with_transition`, which exists so a crash cannot
orphan the row from its transition).

**What the assessment actually needs (traced 2026-07-25).** Both
`NotApplicable` outcomes are decided from the head state's *layouts*, not from
the state chain:

- `NoSystemdBootAsset` — `head_systemd_candidate_count(projection, ..)`
  (`active_reblit_asset_plan.rs:455`, rejects at `:461`).
- `NoKernel` — `kernel_count == 0` over the projected layouts (`:550-556`).

For a NewState candidate those layouts are exactly the incoming `&[Selection]`
package layouts, all resolvable from `layout_db` before any row exists.

**Implementation shape — CORRECTED after tracing the plan builder.** An earlier
draft of this entry assumed the two rules could simply be lifted out over a
`(package ids, layouts)` pair. They cannot, and that route would be wrong:

- `NoSystemdBootAsset` *is* head-local (`head_systemd_candidate_count` filters
  `projection.layouts()` by `selected_packages(projection.head())`), so it would
  lift cleanly on its own.
- `NoKernel` is **not**. `kernel_count` is accumulated across **every state in
  the projection's chain** (`active_reblit_asset_plan.rs:473-485`), inside the
  same loop that builds `state_plans`, `schema_requirements` and per-asset
  roles. Bootability therefore means "the retained chain contributes at least
  one kernel", not "this candidate ships one".

So the pre-allocation probe cannot ignore the chain — it must run the existing
accumulation with the *candidate's selections standing in as the prospective
head*, the rest of the chain coming from `state_db` as it already does. The real
obstacle is that `PreparedActiveReblitBootProjection.states` is a
`db::state::FrozenBootInput` — a frozen capture of persisted `State` rows — with
no way to express a head that has no row yet.

**Therefore the work is:** let the projection carry a *prospective* head
(package set + layouts, no `state::Id`) alongside the persisted tail, and have
`prepare_asset_plan_until` treat it as `state_index == 0`. One implementation
still serves both callers. This is a change to a crash-matrix-verified capture
type, so it wants a fresh session and its own test pass — not a tail-end edit.

Do **not** re-implement either rule separately: the pre-journal answer would
drift from the real plan, and the journal would claim a boot that never becomes
possible.

**Then** Slice 5 passes the derived `run_boot_sync` into
`execute_new_state_forward` (replacing the hardcoded `true`) and treats
`NotApplicable` as "finish without boot" rather than an error.

**Still separate:** `apply_new_state_candidate` handles only the Active-previous
(archive) case. First install with no active predecessor has no coordinated
route at all and must be built before the legacy path can be deleted
(`plans/cleanup_legacy.md` §2).

### 1.1b NewState commit-cleanup route  · E:M R:high · **blocks Slice 5**

**Found 2026-07-25 by tracing the tail; not previously known.** Both coordinated
NewState paths reach `Complete` through
`ActiveReblitCommitCleanupAuthority`, which **rejects NewState records at
runtime**:

- `capture_with_record_binding` (`active_reblit_commit_cleanup_authority.rs:203`)
  returns `NotApplicable` unless `record.operation == Operation::ActiveReblit`.
- `capture` (`:165-173`) additionally rejects `options.archive_previous` and
  `!same_nonempty_candidate_and_previous(record)`.

A NewState record trips all three (operation `NewState`, `archive_previous`
true, candidate ≠ previous). Both routes hit it:

- boot path — `complete_new_state_boot` → `persist_commit_cleanup_complete` →
  `capture_retained_binding`
- no-boot path — the terminal tail `finish_active_reblit_no_boot` → `capture`

**This corrects an earlier conclusion.** Slice 3c was recorded as "COMPILES for
the NewState-derived staged boot — proving it operation-neutral". The types line
up; the runtime admission does not. `apply_new_state_candidate` would fail at
commit cleanup on a real system. The integration test
`apply_new_state_candidate_forwards_and_archives_before_boot_applicability`
never caught it because it stops earlier at `NewStateBootNotApplicable` — the
very error §1.1a removes, so fixing §1.1a alone would have walked straight into
this.

**Shape — SMALLER than a mirror (traced further, 2026-07-25).** An earlier draft
of this entry called for a full ~500-800 line NewState authority mirroring
`active_reblit_commit_cleanup_authority.rs`. That is wrong, because the thing
ActiveReblit's cleanup *does* has no NewState counterpart:

- The cleanup effect is a staging-wrapper **exchange**
  (`.../effect.rs:51` → `prepare_exchange` / `attempt_exchange_once`).
- That wrapper rotation is set up by `prepare_active_reblit_staging_rotation`,
  which the legacy path gates on
  `candidate_origin == StatefulCandidateOrigin::ActiveReblit`
  (`core/stateful_transition.rs:142`). ActiveReblit needs it because candidate
  and previous are the *same* state, so it activates through a replacement
  wrapper. **NewState never rotates a wrapper** — its candidate came from
  staging into `/usr` and its predecessor went to its own state slot.

So NewState has **nothing to reconcile** at `CommitDecided`. But the phase chain
is mandatory — `CommitDecided → CommitCleanupComplete → Complete`
(`validation.rs:511-512`) — so it still must *traverse* the phase.

**Therefore build a no-effect cleanup route**, not a parallel authority: admit
NewState at `CommitDecided` and advance the record to `CommitCleanupComplete`
without an exchange attempt, in the spirit of the existing
`ActiveReblitCommitCleanupRoutePlan::NoBoot` variant. Keep the record-binding
and database revalidation sandwiches; drop only the namespace exchange. Revised
estimate: **E:M-L**, not the original E:L "full mirror".

**The one real obstacle (traced to the bottom).** The persistence step is
already operation-neutral — `persist_active_reblit_commit_cleanup_complete_retaining_binding`
(`startup_recovery/active_reblit_commit_cleanup_complete.rs:69`) just calls
`forward_successor(None)`, which derives the successor from the record's own
options. So nothing below the authority needs changing.

The obstacle is the typestate above it:
`ActiveReblitCommitCleanupDurableAuthority` holds a
`DurableActiveReblitCommitCleanupNamespace` (`.../effect.rs:44-47`), and the
only way to obtain one is `complete()` on a namespace that has *already
exchanged* (`.../effect.rs:120-133`). A no-exchange route therefore cannot
produce the existing durable type. **Resolved: NewState must not reuse this namespace machinery at all.** Both
options first considered (enum the durable namespace; or clone the type) assume
NewState needs *some* namespace evidence. It does not.
`DurableActiveReblitCommitCleanupNamespace` holds `parents` (retained wrapper
descriptors from the exchange), `final_finish` and `final_projection` — all
wrapper-shaped concepts with no NewState counterpart, since NewState performs no
exchange and rotates no wrapper.

So build a small, self-contained `new_state_commit_cleanup` authority whose
evidence is **record binding + database only**, with no namespace member:

- admission: `Operation::NewState`, `Phase::CommitDecided`, `rollback.is_none()`,
  `options.archive_previous`, candidate ≠ previous, receipt correlation
  consistent with `run_boot_sync`
- durable authority: holds that evidence; nothing to reconcile
- persistence: sibling of
  `persist_active_reblit_commit_cleanup_complete_retaining_binding`, calling
  `forward_successor(None)` → `CommitCleanupComplete` (already neutral)

This touches **no** ActiveReblit typestate, so the crash-verified path is
untouched and its tests keep their meaning. That is why the estimate is E:M
rather than E:L, and it is the shape to build.

**Status: authority side SHIPPED** (`a9bdc13a`, `891e12dc`) —
`new_state_commit_cleanup_authority.rs`: admission gate, bracketing database
captures, record-binding checks, `advance_record_binding` to
`CommitCleanupComplete`, and a post-advance authority proving the published
successor. No namespace member anywhere; ~200 lines; zero production warnings.

**Remaining: the persistence sibling — make it generic, do NOT duplicate it.**
`persist_active_reblit_commit_cleanup_complete_inner`
(`startup_recovery/active_reblit_commit_cleanup_complete.rs:79`) is ~300 lines of
durability-critical logic: blocking vs non-blocking journal reopen, successor
revalidation against the reopened journal, fresh binding recapture, multi-stage
validation, storage-failure reconciliation, and fault-injection hooks. Copying it
for NewState would fork the most safety-critical code in the crate.

The *method* surface it calls is small and trait-shaped — `revalidate`,
`record`, `installation`, `advance_record_binding`, plus
`revalidate_successor_same_store` / `revalidate_successor_reopened` on the
post-advance value. **The NewState authority now implements all of it**
(`a9bdc13a`, `891e12dc`, `dcfbbb51`), so the authority side of §1.1b is done.

**But genericizing the persistence function is more invasive than that surface
suggests** (measured 2026-07-25): nine ActiveReblit-specific *types* are threaded
through its body and signature —
`ActiveReblitCommitCleanupDurableAuthority`, `…PostAdvanceAuthority`,
`…Record`, `…ValidationStage`, and the `…EffectError`, `…FreshBindingError`,
`…PersistenceError`, `…RecordAdvanceError`, `…ReopenError` error types. A generic
version must abstract over all nine, which is a substantial refactor of
crash-matrix-verified durability code, not a small trait extraction.

**Decide with the crash matrix runnable** (the VM at `192.168.122.148` is
reachable — confirmed 2026-07-25):

- **(a) Genericize anyway**, using the ActiveReblit crash matrix as the net. One
  implementation of reopen/recapture/storage-failure reconciliation forever.
  Higher up-front risk, lower long-term risk.
- **(b) NewState-specific persistence**, sharing only leaf helpers. Lower
  up-front risk, but forks the crate's most safety-critical logic into two
  copies that must stay identical.

Prefer (a) if the crash matrix can be run against both operations; otherwise the
fork in (b) is the kind of duplication that rots.

The 8 ActiveReblit-specific predicates (`capture`, `capture_with_record_binding`,
`exact_route_plan`, `record_plan_is_exact`) still all need a NewState branch, so
this is not a one-line gate relaxation either.

**Ordering:** build this **before** wiring §1.1a. Wiring the probe first yields
a route that gets further and then fails at cleanup — strictly worse than
today's honest `NewStateBootNotApplicable`.

### 1.1c NewState terminal tail — the full authority chain  · E:L R:high

**Found 2026-07-25 by walking the tail; this resizes Phase 1.1.** §1.1b treated
commit cleanup as *the* ActiveReblit-gated step blocking NewState. It is one of
**four**. Every authority the terminal tail traverses gates on
`Operation::ActiveReblit`, and each carries its own namespace evidence shaped
around ActiveReblit's wrapper rotation:

| authority | lines | `Operation::ActiveReblit` gates | NewState analog |
|---|---|---|---|
| `ActiveReblitBootSyncCompleteAuthority` | 820 | 3 | needed |
| `ActiveReblitCommitCleanupAuthority` | 801 | 4 | **built** (`a9bdc13a`…`d33a8856`) |
| `ActiveReblitCommitCleanupCompleteAuthority` | 812 | 4 | needed |
| `ActiveReblitCompleteFinalizationAuthority` | 762 | 3 | needed |

Both tails route through them — the boot tail via `capture_retained_binding`
(`boot_sync_complete_persistence/…`), the no-boot tail via `capture`
(`startup_gate/live_active_reblit_no_boot.rs:72,119,143`).

**So reaching `Complete` on the coordinated NewState route needs roughly three
more authority stacks (~2400 lines), not one.** Each repeats the §1.1b design
question — *what namespace evidence does NewState actually need here?* — and the
§1.1b answer ("none: NewState rotates no wrapper") will not simply carry over.
`ActiveReblitCommitCleanupCompleteAuthority` holds an
`ActiveReblitCommitCleanupFinishNamespaceProof`, and the NewState equivalent must
prove something real about `/usr` being the candidate with the predecessor
archived — a different shape, not an absence.

**Consequence for sequencing.** Slice 5 cannot wire the coordinated route live
until this chain exists; wiring earlier produces a route that runs further and
then fails deeper, which is worse than today's honest early error. The
`cleanup_legacy.md` §2 blocker therefore stands well beyond §1.1a/§1.1b.

**Question answered (2026-07-25) — the namespace modelling already exists, so
the NewState analogs are far smaller than the ActiveReblit files.**

The blocking worry was that each analog would need new namespace modelling. It
does not. `policy.rs::commit_layouts` (:261-276) is **record-driven and
operation-general**: for `PreviousOrigin::ActiveState` — exactly NewState's
archive case — it already yields `{candidate: Live, previous: Archived}` for
`CommitDecided`, `CommitCleanupComplete` and `Complete` (:255-256). And
`assess_snapshot_layout` (:147) validates a snapshot against those alternatives
using only the record's own tree tokens.

So a NewState terminal authority validates its namespace with the **generic**
snapshot+policy machinery. What it must *not* reuse is
`ProjectedActiveReblitCommitCleanupNamespace`, which rejects non-ActiveReblit
outright (`capture/active_reblit_commit_cleanup.rs:149`) and is built from
`wrapper_index` / `target_name` / `CommitCleanupInvariant` — it tracks a wrapper
identity through an exchange.

**That exchange-tracking is why the ActiveReblit files are 762-820 lines, and
NewState does none of it.** Expect analogs closer to the ~200-line shape of
`new_state_commit_cleanup_authority.rs` than to their ActiveReblit namesakes:
admission on the record, generic layout assessment, bound advance, successor
revalidation. Revised estimate **E:L, not E:XL**.

### 1.1d NewState boot tail — the staged chain is generation-bound  · E:M R:high · **last gate before Slice 5**

**Status: the NewState terminal *authority* chain is complete** (§1.1b/§1.1c —
`BootSyncStarted→BootSyncComplete→CommitDecided→CommitCleanupComplete→Complete→delete`,
all built and tested). The **no-boot** path is wired and proven live by
`apply_new_state_candidate_forwards_archives_and_commits_without_boot`.

**What remains is the bootable path.** `complete_new_state_boot` drives the
ActiveReblit *staged* boot chain (`client/boot/active_reblit_boot_sync_staging/`),
whose validation is ActiveReblit-shaped in two ways:

1. **Shape gates** — 4 `archive_previous` references across 3 files, plus
   `record.operation != Operation::ActiveReblit`. NewState-with-archive trips
   all of them (`commit_decision_handoff.rs:105-115`,
   `commit_cleanup_handoff.rs:212`).
2. **Hard-coded generations** — `ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_GENERATION = 12`
   and `ACTIVE_REBLIT_COMMIT_DECIDED_GENERATION = 13`
   (`commit_decision_handoff.rs:32`, `commit_cleanup_handoff.rs:30`,
   `active_reblit_boot_sync_complete_authority.rs:38-39`).

The generations are the real work. NewState-with-archive traverses
`PreviousArchiveIntent` and `PreviousArchived` that ActiveReblit does not, so its
generations run **four higher** at the same phases. There is **no
generation-derivation helper anywhere in the crate** — every site compares
against a literal.

**So this needs a derived expectation** (operation + options + phase → generation)
replacing the literals, then the shape gates widened. It is contained, but it is
arithmetic inside crash-verified staged validation: an off-by-one silently
accepts a record from the wrong point in the chain. Worth its own session with
the crash matrix runnable.

**Do not wire Slice 5 before this.** Wiring now would split production between
two transition engines keyed on whether a candidate happens to ship a kernel —
non-bootable installs through the durable route, bootable ones still legacy.
That is worse than either engine alone.

### 1.2 ActivateArchived → durable coordinator route  · E:L R:high
Same untethered legacy path (`commit_stateful_staging`) for activating an
archived state into live `/usr`. The coordinator already has
`Operation::ActivateArchived` typestates (`candidate_preparation.rs:220`).
**Approach:** an `execute_activate_archived_forward` analog + forward startup
dispatch. Largely shares 1.1's neutral helper.

### 1.3 Archived-state repair → reduced durable route  · E:M R:med
`repair_archived_state` (`client/archived_repair.rs:82`) rebuilds an **inactive**
tree — no live `/usr` mutation, no boot, transaction-triggers only — via the
separate legacy `ArchivedStateRepairIdentity`. Narrower durability need: survive
a crash between "metadata published to candidate row" and "publication committed."
**Approach:** a reduced coordinator operation
(`Operation::ArchivedRepair` or a degenerate `ActivateArchived` with
`run_boot_sync=false, run_system_triggers=false`, no exchange) covering
`Preparing → CandidatePrepared → TransactionTriggersComplete → publish`, plus a
startup reconciler for a partial publish. Reuse `ArchivedStateRepairIdentity` as
the effect layer. **D1.3:** full journal record vs a lighter durable marker,
given it never crosses the `/usr`/boot boundary?

### 1.4 Forward cleanup crash-safety audit (NewState path)  · E:M R:high
ActiveReblit forward cleanup/finalization is already journal-durable+resumable
(`recovery.rs:45` RollForward + startup dispatch). NewState/ActivateArchived
cleanup (`rotate_active_reblit_staging`, `archive_previous`,
`recover/preserve_*candidate`) is in-process/same-boot. Once 1.1/1.2 land, prove
their cleanup is resumable and audit for any step between the last journal
advance and a physical cleanup (e.g. staging-wrapper rotation) that a crash
could strand.

**Exit Phase 1:** NewState, ActivateArchived, and archived repair each publish a
durable journal that startup reconciliation resumes/rolls-back across a reboot;
crash-matrix tests (before/after each persisted boundary) pass in-process, with
real-reboot proof deferred to Phase 2's harness.

---

## Phase 2 — Live boot proof, real boot repair, and destructive VM campaigns

The heaviest, most dangerous phase. **Both dangerous items are in scope, with
full care.** Nothing here runs on the host; all destructive evidence is
disposable-VM-only; each dangerous step is confirmed with you before running.

### 2.1 Reboot-capable VM campaign harness  · E:XL R:high  (prerequisite for 2.2–2.4)
The current harness (`misc/vm/disposable-uefi-boot-*`) **explicitly never
reboots** and depends on a persistent SSH session. Real durability proof needs a
harness that survives SSH loss.
**Approach:** add a reboot-capable orchestration layer that (i) persists a
self-describing on-disk campaign state + a **first-boot resume service** that
re-attaches and reports (serial-console evidence as the SSH-independent channel),
(ii) injects interruption at each `BootSyncStarted`/receipt-promotion/
`BootSyncComplete` boundary, (iii) verifies the post-reboot startup gate
reconciles the journal to a bootable state. Keep the existing static guards
(no `ssh|virsh|reboot` inside the assert scripts; disposable-disk-only).
**D2.1 (need your input on VM model):** (a) power-loss model admissible as
"equivalent" — qemu hard `quit`, `fsync`-fault (`dm-flakey`), or nested-VM
snapshot revert? (b) is nested KVM available in the approved VM, or must
interruption be driven from the host hypervisor (current rules forbid `virsh`
inside scripts)? (c) how to authenticate campaign identity/results across a
reboot that severs SSH — first-boot resume service + serial evidence?

### 2.2 Live `Ready`-branch boot regression  · E:L R:med
No single regression drives `Client::verify → complete_active_reblit_boot →
finalize` end-to-end (CI resolves boot inputs to `NotApplicable`; the VM test
drives the low-level publisher directly and is forbidden from finalization).
**Approach:** an admissible boot-topology fixture (real UEFI/GPT in the VM, or a
hermetic loopback-ESP) so `prepare_until` returns `Ready` under real
`Client::verify`, asserting the journal walks `BootSyncStarted → … → Complete →
finalized`. **D2.2:** VM (real UEFI) vs host-side loopback-ESP hermetic fixture —
which counts as "live-client" without being misread as reboot proof?

### 2.3 Real startup boot repair  · E:XL R:critical  ⚠ most dangerous item
Production currently fails closed: `BootRepairRequired → BootRepairStarted →
BootRepairUnverified → ManualBootRepair`, emitting **no** `BootRepairComplete`
(the success edge `boot_repair_complete_successor(Applied)` exists but has no
producer). Implementing real repair means minting a **new authenticated
boot-mutation authority** on an already-failed rollback path — re-publishing the
restored state's boot entries to ESP/XBOOTLDR, verifying, then advancing to
`BootRepairComplete`.
**Approach (staged, gated):**
1. First land the **receipt-bound partial-residue prerequisites**
   (FUTURE_PLAN.md:203–214): bind every private `.stage`/`.replace` residue name
   to its exact pending receipt; add one conditional DB op clearing only the
   exact pending receipt head while retaining its committed predecessor.
2. Then a narrow "re-publish the restored (predecessor) state" repair effect,
   bound to the exact journal record + ESP identity, behind a new authenticated
   authority that is structurally forbidden from touching anything but the exact
   target — never the failed candidate's residue.
3. Only after 2.1 exists, prove it across real reboot + power-loss in the VM.
**D2.3:** (a) is the automatic pending-receipt rollback protocol a **hard
prerequisite**, or can a narrower repair land first? (b) what authenticates that
the restored state's boot entries are the correct repair target vs. failed-candidate
residue? I will **confirm with you before wiring any boot-mutation effect** and
keep the safe fail-closed-to-manual default until it is proven.

### 2.4 Destructive VM evidence campaigns  · E:XL R:high
Using 2.1's harness: selected-payload bootability, interruption at every persisted
boot-publication boundary, reboot recovery (startup gate resuming a durable
journal), power-loss-equivalent durability. Then rerun the Phase-11 exit gate and
record exact accepted commits, machine identity, target-disk identity, logs,
hashes, and explicit non-claims (FUTURE_PLAN.md:57–59). No same-boot result
described as reboot/power-loss proof.

---

## Phase 3 — Package/build model

### 3.1 Toolchain-free package mode  · E:M R:med  D
Prebuilt-artifact packages still freeze the full LLVM/GNU toolchain into their
closure + identity (`build/root.rs:297`, `planner.rs:261`, `lock_resolution.rs`).
**Approach:** typed `ToolchainSpec::None` (or `toolchain_free` option) →
`inputs_for` skips toolchain/compiler blocks, `freeze_toolchain_commands` returns
typed-absent, identity binds a distinct `"none"` toolchain value; a
`plan_checks` rule that a toolchain-free plan carries no compiler roles. Bump
`DERIVATION_PLAN_SCHEMA_VERSION`/`BUILD_LOCK_SCHEMA_VERSION` + update 64-example
goldens. **D3.1:** (a) strict "no compiler in closure" vs "no structured compile
phase" (may a toolchain-free package still `Shell` `cc`?); (b) keep analyzer
tools (objcopy/strip for debug-split) — yes, selected separately; (c) new
`ToolchainSpec` variant vs option flag (v3 ABI surface).

### 3.2 Contentful prebuilt-ELF execution fixture  · E:M R:low
Pairs with 3.1 as its execution proof. Commit a deterministic ELF archive
fixture under `tests/fixtures/gluon/execution/archives`; add
`execution_prebuilt_elf.rs` (model: `execution_go_module.rs`) exercising the
existing ELF analysis (`package/analysis/handler/elf.rs`: `DT_NEEDED`, interp,
`split_debug`) + byte-identical rebuilt Stones. **D3.2:** produce the ELF via a
bootstrap build step vs commit a pinned prebuilt binary (no compiler in hermetic
env); which interpreter/libc soname to assert.

### 3.3 Content-addressed local directory/file source ABI  · E:L R:med  D
Add `UpstreamSpec::LocalTree { path, digest, destination }` (+ `LocalFile`) with
matching `SourceResolution::Local`/`LockedSource::Local`, reusing the existing
Git tree normalization+hashing (`upstream/git/materialization/normalization_hashing.rs`,
binds type/mode/symlink) as the digest, and the fixture-import copy-from-cache
mechanics promoted to production. **D3.3:** content must be pre-seeded into the
content-addressed store by digest (recipe references a digest, never a mutable
recipe-dir path — the forbidden mount); symlink absolute/out-of-tree admit/reject
rule.

### 3.4 Wrapper helper  · E:S R:low  (demand-gated: 16/64 recipes repeat the shim)
Gluon-only helper in `cast.package.v3` building the exported-env exec-shim +
`install -Dm755` from typed `{ target_exe, env, installed_path }`, lowering to the
existing `StepSpec::Shell`. No Rust ABI change. Do not copy Nix helper names.

### 3.5 Structured Python/Go builders  · E:M-per-builder R:low  (demand-gated)
Add structured `StepSpec` variants + `BuilderEnvironmentSpec` entries for the
highest-repeated shell-based systems (Python PEP517, Go module), mirroring
CMake/Meson. Incremental/opt-in; `Shell` stays valid. **D3.5:** Python vs Go
first; how much layout variability before it loses to `Shell`.

---

## Phase 4 — Evaluator / config (implementable-now wins + the versioning decision)

### 4.1 Boot-topology v1→v2 migration helper  · E:M R:low  D
v2 is the stable authoritative ABI; v1 `aliases_esp` took a bare partuuid string,
v2 requires a full `PartitionSelector{partuuid, mount_point}`. **Approach:** an
offline converter that evaluates the v1 form, synthesizes the missing
`mount_point`(s), and emits **reviewed canonical Gluon** (reuse the repository
canonical-encoder pattern) — human-review output only, never a runtime fallback
or a second accepted ABI. **D4.1:** safe default mount points to synthesize
(refuse-and-prompt on ambiguity vs guess `/efi`/`/boot`); lives as a `cast`
subcommand vs one-shot tool.

### 4.2 Canonical `repo list` output  · E:S R:low  D
The field-complete canonical encoder already exists (`repository/gluon.rs:321`
`encode_specs`); the CLI `list` (`cli/repo.rs:192`, explicit TODO) prints ad-hoc
strings. **Approach:** wire `list()` to `RepositoryCodec::encode` behind a
`--canonical`/`--gluon` flag, preserving `GLUON_GENERATED_MARKER` + generated-slot
ownership so it round-trips; keep the human summary as default. Verify
`enabled`↔`active` mapping. **D4.2:** replace vs supplement the human summary;
emit the generated marker header or a bare fragment.

### 4.3 Policy-layer pure-array capability tightening  · E:S code / M evidence  D
One-line removal of `enable_array_primitives()` (`build_policy/layers/gluon.rs:43`)
— the layers ABI never imports `std.array.prim`. Blocker is identity versioning.
**D4.3 (load-bearing for the whole evaluator section):** `EVALUATOR_POLICY_VERSION`
is a single global constant; bumping it re-fingerprints *every* Gluon evaluation.
Introduce a **per-adapter/per-ABI evaluator-policy id** (so only the layers ABI's
identity changes) — recommended — vs accept a global bump. Ride the neutral-identity-v2
cutover or land as an independent policy-version step? Before/after evidence:
a layer source with `import! std.array.prim` currently admitted → denied, while
canonical `build_policy_layers.glu` and existing manifests keep byte-identical
identities.

### 4.4 Separately-mounted `/etc` / `/etc/cast`  · E:M (after design) R:med  D
Rooted loading rejects mount crossings via `RESOLVE_NO_XDEV`
(`config/declaration/root_slot.rs:18`). **Approach:** accept an explicitly
supplied, descriptor-authenticated config-root descriptor (operator hands in a
pre-opened trusted fd), authenticate *that* instead of blindly crossing; keep the
default `NO_XDEV` rejection. **D4.4:** how the config-root descriptor is
declared/authenticated (fstab-like vs CLI fd); cross-mount trust model
(device allow-list / fs-magic / ownership); reconcile the fixed-root loader's
*missing* `NO_XDEV` vs the rooted loaders that have it (intentional?).

### 4.5 Read-only repository-manager backend  · E:M R:med  D
Generic read-only DB primitives already exist (`db/read_only.rs`,
deserialize-into-memory + `SQLITE_READONLY` proof) but the *repository* manager
lacks a strict `mode=ro` reader + trusted-owner model. **Approach:** a `mode=ro`
(or deserialize-image) repo-manager reader anchored on an explicit trusted-owner
policy (reuse cache/installation ownership checks), taking only shared locks —
never relaxing the writable manager's lock/mode/owner checks. **D4.5:** `mode=ro`
file open vs the existing deserialize model; who is a trusted owner (root-owned
index readable by unprivileged users?).

---

## Phase 5 — Security-sensitive design + implementation

Each needs a design decision before code; I'll write a concrete design spec, then
implement on your approval.

### 5.1 Authenticated adoption flow for nonempty unmanaged `/usr`  · design → E:L R:high  D
`TransitionOrigin::Unmanaged` currently maps to `Quarantine`;
`require_empty_or_marker_only` (`active_state_snapshot.rs:650`) refuses to bless
unowned content. **Spec:** bounded-inventory adoption with explicit operator
intent + durable per-file/inventory ownership provenance + a recovery path that
never converts ambiguity into authority, assuming a hostile same-UID pre-creator
(`transition_identity.rs:222`). Must not weaken `require_empty_or_marker_only`.

### 5.2 Typed SELinux/IMA/EVM/security-xattr policy  · design → E:L R:high  D
**No** xattr/label handling exists today (blit applies only mode; security xattrs
silently stripped). **Spec:** labels as typed package/system data, authenticated
through descriptor-rooted inventories, applied via `lsetxattr` in the blit
metadata phase (order-sensitive vs `fchmod` for IMA/EVM), no pathname fallback.
Touches `.stone` layout payload — coordinate with 6.3 (`.stone` V2). Precedes any
labeled-candidate-filesystem support.

### 5.3 Privilege separation / hostile same-UID isolation  · design → R:high  D
Current contract defends *cooperating* same-credential writers, not a hostile
same-UID adversary (conceded at `transition_identity.rs:222`). **Spec:** an
explicit hostile-same-UID threat model + evaluation of privilege separation
(helper uid), user namespaces, seccomp, or `FS_IMMUTABLE`/verity to close the
same-UID pre-creation window. Do not claim the cooperative contract is a kernel
freeze.

---

## Phase 6 — Deferred design/research (concrete specs + reconsideration triggers)

For each: a concrete design sketch + the exact trigger that would move it into
active work. These stay design-only until their trigger fires (per FUTURE_PLAN.md
"requires a separate decision").

- **6.1 Fixed-output network ABI** — plumbing exists (`NetworkMode` enum inert;
  planner mapping present; validation rejects). Spec: output-digest contract +
  bounded transfer + isolation + reproducible cache reusing the Phase-3.3
  content store. **Trigger:** a real recipe that cannot be expressed as a locked
  source (none demonstrated; go/cargo/node vendored examples disprove the need).
- **6.2 Provider-resolution alternative** — current resolver is deterministic
  first-match single-identity (`registry/transaction.rs`). Spec: any replacement
  must *become* the resolver, preserve first-match/selection semantics +
  build-lock reproducibility + single identity. **Trigger:** a demonstrated
  version-conflict/optional-dep case first-match can't express.
- **6.3 `.stone` V2 format migration** — single-version today
  (`stone/src/header:V1`). Spec: header `V2` dispatch across `stone`+`libstone`+
  `stone.h`, readers, upgrade policy, reproducibility + rollback evidence
  (older Cast already rejects via `UnknownVersion`). Coordinate with 5.2 (labels
  may be the V2 payload). **Trigger:** a concrete new payload need (labels /
  compression / larger fields) + current recovery contracts closed.
- **6.4 Recursive policy-overlay fixed points** — one-way add/replace/modify only.
  Spec: finite, explainable, Stone-native (no Nix fixpoints). **Trigger:** a real
  package family the one-way model can't express without duplication.
- **6.5 `PackageInputs -> PackageSpec` function ABI + callPackage reflection** —
  inputs are identity-only today (hashed, never VM-injected). Spec: first make
  inputs a typed VM-injected argument (still folded into identity), *then*
  reflection preserving typed missing/extra-input errors + deterministic
  provenance. **Trigger:** decision to make inputs a real argument at all.
- **6.6 Eval-time fetching / import-from-derivation** — hermeticity is structural
  (empty VM, forbidden fs/io/net modules, frozen derivation). Spec: separately
  versioned capability with complete hermeticity/locking/recursion/failure model,
  outside the pure initial evaluator. **Trigger:** likely indefinite; a concrete
  need that a locked source cannot serve.
- **6.7 Global/user policy discovery** — single explicit rooted `data_dir/policy`.
  Spec: only if every discovered layer seals into `EvaluationIdentity` reproducibly
  without ambient `$HOME`. **Trigger:** a real multi-layer requirement.
- **6.8 Direct Gluon provenance for structured build steps** — diagnostics already
  keep real script spans (guardrail satisfied); steps have argv identity but no
  per-step evaluated-source identity. Spec: add provenance once a stable per-step
  source identity is defined. **Trigger:** that identity being defined. Do not
  regress into recipe-text scanning.
- **6.9 Nix interoperability / lazy package-set architecture** — comparative
  research only. Spec: none; keep [`bsd.md`](bsd.md)-style as archived study.
  **Trigger:** a concrete Stone-native limitation a smaller interop layer can't
  solve — via a separate architecture decision.
- **6.10 Declarative services/users/kernel/per-machine composition** — beyond
  package sets today. Spec: a future typed module system preserving the `/usr`
  vs `/etc` boundary + atomic rollback (no imperative activation scripts).
  **Trigger:** after package + crash-recoverable activation contracts (Phases 1–2)
  close; a concrete workflow requiring it. Multi-version coexistence / profile
  generations as a separate store/profile design that must not weaken the
  one-live-tree model.
- **6.11 More Stone-native recipe patterns** — demand-gated; the natural new gaps
  are exactly Phase 3's toolchain-free + local-source + prebuilt-ELF patterns.

---

## Phase 7 — Test-ops tooling + remaining maintenance

### 7.1 `timeout` wrapper sweep  · E:M R:low  D
119 files. Remove `timeout <N>s` prefixes on deterministic helpers
(`grep/test/rg/sed/awk/mkdir/rm/...`) and on compilation/`--list` invocations;
**keep** bounds on `$(CARGO) test` that runs fixtures, `systemd*`/`udevadm`, and
fixture-runner scripts (things that can genuinely hang). Per-file classification,
not a blind sed. **D7.1:** a `CARGO_TEST_TIMEOUT` macro to centralize the kept
bounds so the remove-sweep is unambiguous?

### 7.2 VM hygiene audit  · E:M R:low  (build only if VM campaigns become routine — Phase 2 makes them so)
Read-only reporter: leftover `cast-fixtures-ci-*`/`cast-delegated-fixture-*`
`--user` units + procs, VM test-disk mounts (mountinfo), `loginctl` linger, the
Ubuntu AppArmor userns setting. Reuse the UID-scoping from
`stop-owned-fixture-unit.sh`. Report-only.

### 7.3 VM capacity preflight  · E:S R:low
`df --output=avail,iavail` / `stat -f` preflight with thresholds, wired as a make
prerequisite that refuses the outer unit before compilation. **D7.3:** thresholds;
which filesystem (`target/` build root vs `/var/tmp` VM build root).

---

### 7.4 Source-comment backlog (`TODO`/`FIXME`)  · E:S-per-item R:low

Filed from `plans/cleanup_legacy.md` §6 so no source comment survives without a
plan reference. Each is small, independent, and blocks nothing. Grouped by
nature; the file:line is the anchor, not a promise about scope.

**Correctness / behaviour (decide before changing):**
- `vfs/src/tree/mod.rs:147` — duplicate-path detection is downgraded from an
  error to an `eprintln!` (`return Err(e)` commented out). Re-enabling makes
  currently-succeeding installs fail. **D-CL6** in `cleanup_legacy.md`.
- `dag/src/subgraph.rs:123` — cycle breaking is unimplemented.
- `forge/src/registry/plugin/active.rs:39` and
  `forge/src/registry/plugin/active.rs:81` — two unhandled error paths.
- `forge/src/registry/plugin/cobble.rs:99` — unverified flag choice.
- `libstone/src/lib.rs:156` — error handling unimplemented.
- `container/src/lib.rs:548` — replace the catch-all error with finer variants.

**Parsing gaps (known-wrong inputs):**
- `mason/src/draft/metadata/github.rs:106` — string version prefixes unhandled.
- `mason/src/draft/metadata/gitlab.rs:106` — project name embedded in the
  version is unhandled.

**API / ergonomics:**
- `forge/src/cli/repo.rs:60` — the `repo` CLI API wants a full overhaul; the
  current shape is explicitly temporary. (Its canonical-output TODO is already
  closed by `1218c00a`.)
- `forge/src/client/cache.rs:244` — return an `Unpacked` value owning `blit`.
- `forge/src/registry/plugin/repository.rs:31` — replace mutation with a
  type-safe construction.
- `forge/src/cli/search.rs:398` — search binary names by default.
- `forge/src/client/prune.rs:116` — report "no states to be removed".
- `forge/src/client/sync.rs:163` — surface the "why" of system-intent packages.
- `forge/src/client/postblit.rs:235` — cache under `/var/`.
- `stone/src/write.rs:21` — allow plain encoding.
- `container/src/mounts/syscalls.rs:40` — prefer a real API over the current
  approach.

**Blocked on upstream Rust:**
- `forge/src/util.rs:266` — adopt `try {}` once stable.

**Cosmetic:**
- `mason/src/build/job/phase.rs:70` — output formatting.

---

## Cross-cutting sequencing summary

```
Phase 0  (safe prereqs, incl. flake.nix pin + stale-fixture fix)   ── do first
Phase 1  (durability: NewState/ActivateArchived/repair → journal)  ── your priority
Phase 2  (reboot harness → live Ready test → real boot repair → VM campaigns)
Phase 3  (toolchain-free + prebuilt-ELF, local-source ABI, wrappers, builders)
Phase 4  (boot-topology v1→v2, canonical repo-list, policy-version, /etc, ro-repo)
Phase 5  (security design+impl: /usr adoption, SELinux/IMA, privilege sep)
Phase 6  (deferred design specs + triggers)
Phase 7  (timeout sweep, VM hygiene/capacity tooling)
```

Phase 2's real-boot-repair (2.3) and VM campaigns (2.4) are the highest-risk work
and are explicitly in scope **with per-step confirmation**. Phases 3–4 can run in
parallel with Phase 2's long VM campaigns since they touch disjoint code.

## Open decisions I still need from you

Blocking or shaping, by phase: **D0.2** (make-gate test rename/removed),
**D0.4** (dead-code keep vs delete), **D1.1** (NewState receipt/rollback
semantics — RESOLVED: one operation-neutral boot route), **D1.4** (NewState
pre-allocation boot applicability — RESOLVED: assess from selections), **D1.3** (archived-repair record weight), **D2.1** (VM power-loss
model + nested-KVM + cross-reboot identity), **D2.2** (VM vs loopback-ESP for the
Ready test), **D2.3** (pending-receipt protocol as hard prerequisite? — I confirm
before any boot-mutation), **D3.1** (toolchain-free strictness + encoding),
**D3.3** (local-source digest-seeding + symlink policy), **D4.1** (boot-topology
default mount points), **D4.3** (per-adapter vs global evaluator-policy version —
load-bearing), **D4.4/4.5** (config-root + repo trusted-owner models),
**D5.1/5.2/5.3** (security design confirmations), **D7.1/7.3** (timeout macro,
capacity thresholds).
