# Durability close-out — what actually remains

Successor to `close_out.md`, `cleanup_legacy.md` and
`previous-restore-recovery-identity.md` (retired 2026-07-30; full text in git at
`fe12e530`). Only open work is kept here.

**State of `develop` at time of writing:** production build at zero warnings, all
three stateful operations on the coordinated journal route.

**On "suite green 2752/0" — read that number with care.** This machine is never
idle: five-plus QEMU guests (one up eight days) plus several projects' builds run
concurrently as its normal state, with load average ~32 on 24 cores. Under that
load a handful of timing tests fail nondeterministically — `Timeout`,
`AcquireLock { WouldBlock }`, "cooperating writer did not acquire" — and a
*different* one each run. The unmodified baseline fails the same way. So `2752/0`
was a lucky draw, not a property of the tree, and a single red run here is not
evidence of a regression.

The usable test for a regression on this machine is **repetition, not
isolation**: run the suite more than once and treat only a test that fails in
multiple runs as real. Anything that fails once and passes in isolation is load.
Do not "fix" those by loosening durability — the deadlines are test-side
(`timeout_policy::tests::the_test_build_actually_scales_budgets` guards the
production budgets).

---

## A. Crash matrix — widen to every operation · E:XL R:high

The harness is proven. What remains is coverage, not invention.

### A1. Re-run the ActivateArchived phase-targeted cut · **do first, cheap**

The last recorded attempt (2026-07-27) found that `OPS=(activate)` produced **no
ActivateArchived transition at all**: targets at
`ActivateArchived.TransactionTriggersStarted` and `.CandidatePrepared` both
reported `CELL-OP-DONE` without the `CAST-AT-PHASE` marker printing.

**That diagnosis is stale in a useful way.** It predates activation being wired
to the coordinated route (merged 2026-07-30). The route now exists, so the marker
should fire. This is the cheapest possible confirmation that the whole activation
epic did what it claimed at guest level, and it must be run before anything else
in this section.

Two earlier false-greens are the reason this is gated rather than assumed: the
`state=absent` column, and the `activate` cells that looked green because
`cast state activate 1` was failing with `state 1 already active` and nothing was
happening.

### A1 RESULT 2026-07-30 — the false green is gone, and it exposed a stall

Two harness bugs had to be fixed before the cell meant anything:

1. **The guest kernel is `0600` root-only**, so qemu never booted:
   `could not open kernel file ... Permission denied`. The "never reached"
   message was printed by a run that never ran. Stage a readable copy
   (`sudo cp /boot/vmlinuz-* /tmp/vmlinuz && sudo chmod 644`) and point
   `KERNEL` at it.
2. **`stage_and_activate` never produced an ActivateArchived transition.** Its
   comment claimed "the install created state 2", but on a fresh root the
   install creates state **1**, so `cast state activate 1` was a no-op error and
   the marker could never fire. It now installs, *removes* (creating state 2 and
   archiving state 1), then activates 1. That sequence only became possible on
   2026-07-30 — a first install used to leave its displaced `/usr` placeholder in
   fixed staging and wedge the next operation.

With both fixed, the cell produces a real verdict for the first time:

    activate  phase:ActivateArchived.CandidatePrepared
      recovery=PENDING  driver=stalled-at-CandidatePreserveIntent  state=absent

    PHASE-1: CandidatePreserveIntent
    STALL: state transition 33c7e4db... at CandidatePreserveIntent requires
           ResumeRollback { phase: CandidatePreserveIntent }

**CONFIRMED A DEFECT 2026-07-30.** The standing rule was applied rather than
skipped: recovery is incremental, so "still pending after N tries" and "cannot
recover" are indistinguishable at a fixed N, and that confusion already produced
one false defect report in Phase 1. So the stall threshold was raised to 25 and
every attempt's phase printed:

    TRY-1:  CandidatePreserveIntent
    TRY-2:  CandidatePreserveIntent
    ...
    TRY-26: CandidatePreserveIntent
    STALL: state transition fe527020... at CandidatePreserveIntent
           requires ResumeRollback { phase: CandidatePreserveIntent }

**26 consecutive attempts, the phase never moves.** The record advances exactly
once — `RollbackDecided -> CandidatePreserveIntent` — and then stops forever.
`state=absent`: the guest is left with no installed state at all.

**This is an ActivateArchived rollback that bricks the system.** Nothing on the
startup path can resume from `CandidatePreserveIntent` for this operation, so
every subsequent command fails the startup baseline and the machine never
recovers.

It also answers A3 for ActivateArchived, in the worst way: this operation does
**not** share NewState's working pre-exchange recovery. The §1.4 fix was scoped
to NewState deliberately, and this is the consequence — measured, not assumed.

### A1 FIXED 2026-07-30 — five bricking windows, not one

Reproduced in-process, so this no longer needs a VM run to study. Walking every
operation against every pre-exchange crash source gave:

| operation @ crash source | route admits | tail consumes | before |
|---|---|---|---|
| NewState @ CandidatePrepared | ✗ | — | **stalls at RollbackDecided** |
| NewState @ TransactionTriggersStarted | ✓ | ✓ | recovers |
| NewState @ TransactionTriggersComplete | ✓ | ✓ | recovers |
| ActivateArchived @ CandidatePrepared | ✓ | ✗ | **stalls at CandidatePreserveIntent** |
| ActiveReblit @ CandidatePrepared | ✗ | — | **stalls at RollbackDecided** |
| ActiveReblit @ TransactionTriggersStarted | ✓ | ✗ | **stalls at CandidatePreserveIntent** |
| ActiveReblit @ TransactionTriggersComplete | ✓ | ✗ | **stalls at CandidatePreserveIntent** |

Exactly one case recovered: NewState during transaction triggers — the single
window §1.4 ever measured. Everything else bricked.

Two causes, both the standing hazard below (a hard-coded value encoding
structure), and both now derived instead:

1. `rollback_source_is_supported` / `rollback_usr_exchange_is_settled` gated
   pre-exchange sources to NewState. But the gates that *decide* to roll back
   (`rollback_decision_source_is_supported`, `is_usr_exchange_rollback_source`)
   never had that restriction, so the system persisted `RollbackDecided` and
   then refused to carry it out. **Being cautious in one half of a two-sided
   contract is not caution — the asymmetry is what bricks the machine.**
   `usr_exchange_is_settled` now reads the journal's own `usr_possible` rule
   backwards (`source.ordinal() < UsrExchangeIntent.ordinal()`).
2. `external_effects_may_remain == (operation != ActivateArchived)` was a
   stand-in for "did we get past transaction triggers", false pre-exchange for
   every operation. It was a fourth copy of a derivation that already existed
   three times. Now one method, `expected_external_effects_may_remain`.

Guarded by `admitted_rollback_resume_routes_always_have_a_consuming_successor`,
which fails if the head and tail drift apart again.

**The test that was supposed to catch this passed while asserting nothing.**
Its helper hard-coded `layout = Post` for `RollbackDecided`, so no caller could
express a pre-exchange rollback: all seven cases hit a `continue` and the
assertion ran on an empty set. The layout is now a caller-supplied fact, and the
test names its expected coverage so a silent skip fails instead of passing.
Same shape as the two false greens above — worth assuming the next one exists.

### A1b RECONCILED 2026-07-30 — the phase hook was on the abandoned route

The `CAST-AT-PHASE` marker printed "never reached" while a real transition was
running because `park_for_phase_targeted_crash` lived only in the unbound
`store::advance`. Every coordinated transition publishes through
`advance_record_binding`, which never called it.

So `CAST_CRASH_AT_PHASE` was **silently inert for exactly the operations this
matrix exists to test** — the epic moved all three onto the coordinated route
and left the crash hook behind. Any phase-targeted cell run since then measured
nothing. The hook is now on both publish paths.

That is the fourth false green in this section, all the same shape: a cell looks
green because the thing it names never ran (root-only kernel → qemu never
booted; `activate 1` no-op → no transition; inert phase hook → no cut; vacuous
test → no assertion). **Assume a fifth exists.** Before believing any green
cell, confirm the operation under test actually ran: `CAST-AT-PHASE` present for
phase cuts, and a non-empty `state=` column.

### A1c. Unmeasured neighbour — sources *below* `CandidatePrepared`

`rollback_allowed` (the journal) permits a rollback from any source with
ordinal `< CommitDecided`, which includes `Preparing`,
`CandidatePrepareStarted`, and — for ActivateArchived —
`ArchivedCandidateStagingIntent` / `ArchivedCandidateStaged`. But
`rollback_decision_source_is_supported` admits none of them, so a crash there
never reaches `RollbackDecided` at all.

**MEASURED IN-PROCESS 2026-07-30 — fourteen stranded pairs, and it is a lower
bound.** `every_begin_rollback_phase_has_an_admitting_decision_authority` walks
every forward phase of every operation, keeps the ones whose
`recovery_disposition` is `BeginRollback`, and asks whether any authority admits
the decision. Fourteen say no:

| operation | phases with no admitting authority |
|---|---|
| NewState | `Preparing`, `FreshStateAllocating`, `FreshStateAllocated`, `CandidatePrepareStarted`, `BootSyncStarted` |
| ActivateArchived | `Preparing`, `CandidatePrepareStarted`, `SystemTriggersStarted`, `SystemTriggersComplete`, `PreviousArchiveIntent`, `PreviousArchived`, `BootSyncStarted` |
| ActiveReblit | `Preparing`, `CandidatePrepareStarted` |

**ActivateArchived has no post-exchange rollback admission beyond
`RootLinksComplete` at all** — the decision gate's `(operation, phase,
generation)` table contains only NewState and ActiveReblit rows. A power cut
while activation runs its system triggers, archives the previous state, or syncs
boot leaves the machine unable to recover.

The list is a lower bound: `ArchivedCandidateStagingIntent` and
`ArchivedCandidateStaged` are missing from the test's `FORWARD_PHASES` constant,
so they were never examined.

The test **pins this list rather than asserting it empty**. Closing fourteen
admission gates at once with no crash-matrix cell behind any of them is exactly
the over-widening that produced the defect fixed above. Shorten the list as each
is measured and fixed; the test fails on any addition, and on any removal that
is not recorded.

Original reasoning, kept because it is how the gap was found:
`Phase::recovery_disposition` (`transition_journal/recovery.rs`) maps *all* of
`Preparing`, `FreshStateAllocating`, `FreshStateAllocated`,
`CandidatePrepareStarted`, `ArchivedCandidateStagingIntent` and
`ArchivedCandidateStaged` to `BeginRollback { source }`. But
`rollback_decision_source_is_supported` admits none of them. So startup decides
to roll back, no authority accepts the decision, and the boot stalls — the exact
§1.4 shape, for six more phases.

These are reachable: a power cut during `cast install` setup or during archived
candidate staging lands squarely in them.

The real invariant is stronger than the one now tested: **every phase whose
disposition is `BeginRollback` must have an admitting decision authority, and
the resulting chain must be consumable end to end.** The current test starts
from the decision gate's own list, so it cannot see this. Rewrite it to start
from `recovery_disposition` instead — that is the actual contract, and it would
have caught both this and the bug already fixed.

Do this before B: same failure shape, and B's boot-repair path sits directly
downstream.

### A2. Extend `OPS` past install

Order: **archived repair first** — it is the newest durability claim and has zero
crash coverage — then ActiveReblit, then ActivateArchived.

Wall-clock cuts cannot isolate one operation's window when the setup preceding it
takes seconds. Use `CAST_CRASH_AT_PHASE` (accepts `Phase` or `Operation:Phase`),
which parks once the record is durable so the harness can cut power on the
marker. **Parking rather than aborting is the point** — aborting leaves the page
cache intact, unsynced writes survive, and the cell proves nothing.

Every operation added before phase targeting works produces cells that look green
without testing what they name.

### A3. The unmeasured question

**Do ActiveReblit and ActivateArchived share NewState's pre-exchange recovery
gap?** The §1.4 fix was deliberately scoped to NewState because that is the only
operation whose window was ever measured. Extend deliberately, with a
crash-matrix cell per operation — *not* by widening a shared predicate. Two
over-widenings during Phase 1 came from exactly that shortcut.

### A4. Cross-reboot proof for previous-restore

`previous-restore-recovery-identity.md`'s implementation is complete in-process
(record → producer → authoritative name → recovery identity → attempt adoption),
but every proof is an in-process fixture. Its own exit criteria demanded a VM
reboot matrix, because in-process fixtures cannot prove cross-reboot behaviour.

Concretely: archive, cut power, reboot, and confirm the dispatcher adopts the
attempt from the recorded parking name and reverses the archive.

Destructive runs happen in the approved VM only. Never on the host.

---

## B. Real startup boot repair · E:XL R:**critical** · §2.3

The most dangerous item in the project. **Do not start before A can exercise it.**
A boot-repair path that cannot be crash-tested is exactly what this epic exists to
avoid shipping.

§2.2 (live `Ready`-branch boot regression) is the natural warm-up.

---

## C. Remove the legacy stateful transition route · E:M R:med

All three operations are coordinated, so the legacy route is dead production code
reachable only from tests. Measured 2026-07-30.

### The blocker: two test helpers

| helper | trivial `\|_\| Ok(())` | fault-injecting |
|---|---|---|
| `active_reblit_tests::run` | 19 | 7 |
| `stateful_candidate_metadata::apply_fresh_candidate` | 6 | 2 |

**Re-pointing is not a signature swap**, even for the trivial 25: the legacy
helpers take a `vfs` tree and blit it, while the coordinated entries take an
already-materialized `fixed_staging::StatefulCandidate`. Every site needs a
materialization step.

The 9 fault-injecting sites need re-expressing as journal-phase fault-hook tests,
because the coordinated route has no checkpoint mechanism — the same port the
archived-activation tests already went through.

Some "trivial" sites also assert **quarantine**, which is legacy-only:
`quarantine_candidate` has one non-test caller (`stateful_recovery.rs:370`) and
begins with `require_no_journal()`, so a coordinated transition can never reach
it.

### Named counterparts, so deletion is not a guess

| legacy test area | coordinated counterpart |
|---|---|
| quarantine collision / durability faults | `usr_rollback_candidate_preserve_authority` |
| root-ABI preflight + exchange boundary | `journal_coordinator::root_abi_publication_*` (15 tests) |
| previous archive/restore suffix routing | `previous_tree_move` suffix tests + recovery identity/adoption (2026-07-30) |
| marker/token substitution refusal | `transition_identity` (158 tests) |

### Then, mechanically

Delete the 34 tests, `apply_stateful_blit*`, `commit_stateful_staging`,
`stateful_recovery.rs`, `candidate_quarantine.rs`. `ArchiveJournalGuard::LegacyNoJournal`,
`JournalAcquisition::LegacyBlocking` and `legacy_boot_repair.rs` then collapse on
their own, along with `require_no_journal`.

---

## Standing hazards — all cost a misdiagnosis, all the same shape

A hard-coded value silently encoding structure:

- **Phase ordinals** were duplicated across three tables, two invisible to a
  `.ordinal()` grep. Now one source of truth. Keep it that way.
- **Generation expectations** were hard-coded in production. Now derived — and
  this one *recurred*: a stale table in `exact_system_trigger_successor` still
  described the pre-archived-staging chain, duplicating
  `expected_forward_generation`. Nothing noticed because the route it governed
  had no callers.
- **Test-side deadlines that do not scale with load** look like durability
  failures. `timeout_policy::tests::the_test_build_actually_scales_budgets`
  guards the production budgets.
- **When extracting a predicate, grep for every raw copy of the condition.** One
  left behind survived two rounds of narrowing and silently accepted an invalid
  plan.
- **A production dead-code warning proves nothing.** Confirm with
  `cargo build -p forge --tests` before deleting. This bit twice.
- **Unit tests of a route with no callers prove it compiles, not that it runs.**
  Wiring `cast state activate` exposed seven defects that every prior green run
  had missed.
- **A test whose loop can `continue` past every case passes while asserting
  nothing.** The pre-exchange rollback test skipped all seven of its cases for
  days. Any test that filters cases must name the coverage it expects, so a
  silent skip fails instead of going green.
- **A predicate that infers a fact it should be given cannot express the case
  you need.** A test helper deriving `layout` from the phase made the
  pre-exchange rollback unrepresentable — the bug and the blindness to it had
  the same root.

And the method lesson that broke two deadlocks after repeated guessing failed:
**measure the value, do not derive it from assumed arithmetic.** Instrument and
print, then fix.

---

## Loose end

`develop` is ~139 commits ahead of `origin/develop` and unpushed.
