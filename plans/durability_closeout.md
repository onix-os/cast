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

### A1c RESULT 2026-07-30 — eight of fourteen closed, six deliberately left

Fixed (`2df2200c`), all pre-exchange: NewState `Preparing`,
`FreshStateAllocating`, `FreshStateAllocated`, `CandidatePrepareStarted`;
ActivateArchived `Preparing`, `CandidatePrepareStarted`; ActiveReblit
`Preparing`, `CandidatePrepareStarted`. Nothing in `/usr` has been touched at
any of them, so admitting them only required the head and tail to agree.

It took **three** predicate fixes, and the tests caught two mistakes that would
otherwise have shipped:

1. Widening only the decision gate **moved** the stall to `RollbackDecided`
   instead of removing it — the resume route held a *third* hand-kept copy of
   the source list. It now delegates to `rollback_source_is_supported`.
2. `NewState ⇒ fresh_db == Pending` assumed the crash happened after fresh-state
   allocation, so a rollback from `Preparing` carried the correct `NotRequired`
   and was refused for it. Now `fresh_db_rollback_is_possible`.
3. The fresh-db invalidation gate asserted `external_effects_may_remain`
   outright, so a rollback beginning before the transaction triggers reached
   `FreshDbInvalidationIntent` and **stalled two phases deeper than the test
   could see**. Now derived.

That third one changed the test, not just the code:
`admitted_rollback_resume_routes_always_have_a_consuming_successor` now walks
the **whole chain** to `RollbackComplete`, bounded so a non-advancing chain
fails rather than hangs. Checking only the first successor certifies that a
rollback *starts*; only the full walk shows it *finishes*, and only the second
keeps a machine bootable.

Also corrected during the fix: adding `Preparing` to the source list let a plan
claim `fresh_db: Pending` at a phase where nothing was allocated. The gate now
cross-checks the two.

**Suite 2754/0, zero production warnings.**

**The six left are a different problem.** All post-exchange, where recovery has
real work to undo — reverse the exchange, restore the previous state, repair
boot. Do not close them by widening a predicate; each needs a crash-matrix cell
proving the effect actually runs (task A2, and the phase hook only started
working on 2026-07-30).

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

### A2 RUN 2026-07-31 — the chain advances; a terminal stall remains

First guest-level run with the fixed binary (`2df2200c`) and the phase hook on
the coordinated route. The rollback **advances** where it previously froze:

    PHASE-1: CandidatePreserveIntent
    PHASE-2: CandidatePreserved
    PHASE-3: RollbackComplete
    STALL: ... at RollbackComplete requires FinalizeRollback
    recovery=PENDING  driver=stalled-at-RollbackComplete  state=absent

Against 26 consecutive attempts frozen at `CandidatePreserveIntent` before the
fix, that is the pre-exchange closure working outside a unit test.

**New defect: the terminal `FinalizeRollback` step is refused.** The chain now
reaches `RollbackComplete` and cannot finalize, so the machine still never
recovers — the stall moved, it did not disappear.

### A2b PARTLY FIXED 2026-07-31 (`27e8f548`) — three terminal stalls left

The five terminal and route gates (`usr_rollback_complete_route`,
`usr_rollback_finalization`, both ActiveReblit twins, and
`usr_rollback_fresh_db_invalidation_route`) each asserted
`rollback.external_effects_may_remain` outright — instances six through ten of
the same pattern. Deriving it fixed every transaction-trigger source, which now
walks the **entire** chain, `RollbackDecided` through `RollbackComplete`, and
finalizes. Suite 2754/0.

### A2c 2026-07-31 (`f3a924c9`) — one terminal stall left, cause identified

`rollback_finalization_plan_is_exact` held an eleventh hand-kept source list,
starting at `CandidatePrepared`. A rollback begun while allocating the fresh
state or preparing the candidate therefore walked the entire chain and then
could not finalize — recovery did everything asked of it except finish. Both
now finalize. Suite 2754/0.

### A2d CLOSED 2026-07-31 (`ab949007`) — no pre-exchange stalls remain

The last one, NewState @ `Preparing`, is fixed, and **the VM answered the
question rather than a guess**. The timed-out activate run below was not wasted:
its killed install rolled back from an early phase, walked the whole chain, and
stalled on `FinalizeRollback` — reproducing on a real guest exactly the case
pinned in-process. Production genuinely carries no candidate ID at `Preparing`,
so the gate was wrong, not the fixture.

Two more instances of the pattern, both assuming an allocation a pre-exchange
rollback never made:

- `record.candidate.id.is_some()` — now required only when one could have been
  allocated (`source >= FreshStateAllocated`).
- `fresh_db ∈ {Applied, AlreadySatisfied}` — now `NotRequired` when
  `fresh_db_rollback_is_possible` is false.

That is **thirteen** instances of one pattern across this work.

### A2e CLOSED 2026-07-31 (`a61eede9`) — the siblings had it too

Predicted from the pattern, then confirmed: `activate_archived_finalization_plan_is_exact`
and `active_reblit_finalization_plan_is_exact` both carried hand-kept source
lists starting at `UsrExchangeIntent`, so every pre-exchange source fell through
to `_ => false`. An ActivateArchived or ActiveReblit rollback begun before the
exchange walked its entire chain and then could not finalize — the identical
stall just fixed for NewState, sitting unfixed in both siblings. **Instances
fourteen and fifteen.**

Found by taking the pattern seriously enough to go looking, not by a failing
test or a VM run. That is the first time in this work the heuristic paid forward
rather than explaining a defect after the fact — **treat any remaining
hand-kept source list in this subsystem as a defect until shown otherwise.**

**A third over-widening, again caught only by an exclusion test.**
`startup_active_reblit_finalization_rejects_a_valid_terminal_lookalike_plan_and_wrong_topology`
asserted that a pre-exchange ActiveReblit rollback must be *refused* — the
machine failing to recover, mistaken for a boundary. Updated to assert it
finalizes; the route's boundedness still rests on
`admits_root_links_only_at_generation_*` and the wrong-topology half.

None of the three over-widenings committed in this work were caught by the
admission-side invariants added alongside them. That is structural, not luck:
loosening a gate never strands a chain, so every "nothing is stranded" test
stays green through it.

`admitted_rollback_resume_routes_always_have_a_consuming_successor` now asserts
its stranded list is **empty**, not pinned: every pre-exchange source, for every
operation, walks its chain to a terminal phase and finalizes. Suite 2753/1, the
single failure being the known load-sensitive `receipt_promotion` cluster that
also fails on an unmodified baseline and passes in isolation.

**One earlier entry here was wrong.** `(NewState, Preparing) dies at
CandidatePreserved` was not a product defect — the test gated that phase with
`rollback_complete_route_plan_is_exact`, which actually gates
`FreshDbInvalidated`. Wrong predicate, false stall.

**And the fix regressed something on the way in.** Delegating the source check
to `rollback_source_is_supported` accepted `RootLinksComplete` at *any*
generation, defeating a deliberate generation-18 pin. The new invariant tests
did not catch it — they assert chains are *admitted*, and loosening a gate never
strands anything. A pre-existing *exclusion* test did.

**Keep both families of test.** Everything added in this section asserts
admission; the two over-widenings committed here were caught only by tests
asserting refusal. They fail on opposite mistakes and this subsystem needs both.

**The test that existed to catch this waved it through.** The chain walk matched
`_ => true` for completed-action and terminal phases, so it gated the intents
and ignored precisely where the chain died. A VM run found it; the test written
that same day for that exact purpose did not. Third occurrence — an unchecked
segment is where it fails. Note also that `CandidatePreserved` has **two** legal
exits depending on whether a fresh row still needs invalidating; gating only the
completion route there produces a false stall for every NewState source.

The ActivateArchived and ActiveReblit terminal gates are still not exported and
so still unchecked by this test. Export them next; on the evidence so far,
assume they hide the same thing.

**Two reasons this cell still does not test what it names:**

1. `CAST-AT-PHASE` *still* never printed, so the phase-targeted cut did not
   fire even with the hook on `advance_record_binding`.

   **Ruled out 2026-07-31: the cmdline parsing is fine.** Replaying the guest's
   `sed`/`tr` pair against a real `cell_phase=ActivateArchived.CandidatePrepared`
   yields exactly `ActivateArchived:CandidatePrepared`, which is what the hook
   compares against. The env var is set correctly.

   **ANSWERED 2026-07-31: the phase machinery was never broken.** With the
   separated diagnostic (`0c224582`) and `PHASE_WAIT=600`, the cell prints:

       TIMED-OUT: ActivateArchived.CandidatePrepared not reached in 600s,
                  and the operation had not finished

   The hook fires, the env var is set, the `sed`/`tr` parse is correct. **The
   setup install never completes in the nested guest** — not in 120s, not in
   600s. Every "phase never reached" this harness ever printed was a statement
   about a guest that had not got there yet, and three separate diagnoses blamed
   the hook for what was a clock.

   **RECOVERY CONFIRMED WORKING 2026-07-31.** The same cell that reported
   `state=absent` and a frozen phase all morning now reports:

       driver=recovered-at-4  state=installed
       PHASE-1: CandidatePreserveIntent
       PHASE-2: CandidatePreserved
       PHASE-3: RollbackComplete

   The rollback walks its chain, finalizes, and the guest comes back with state
   installed. That is the day's fixes verified end to end outside the suite.

   **The marker is still not the blocker it appeared to be — and three
   diagnoses of it have now been wrong.** In order: the hook was on the wrong
   route (true, and fixed); the cmdline parsing (ruled out by replay); install
   throughput (falsified — 26s); and `>/dev/null 2>&1` in write mode discarding
   the marker (fixed in `crash-matrix-run.sh`, but the cell *still* times out,
   so that was not the cause either).

   **The instrument is now in place.** `park_for_phase_targeted_crash` writes a
   `cast-at-phase` witness file before parking, into the directory named by
   `CAST_CRASH_AT_PHASE_WITNESS`. Point that at a path on the guest's **durable
   disk** (under `/mnt/root`), never `/tmp` — the guest's `/tmp` is tmpfs and
   dies with the power cut, so a witness there cannot be read by the verdict
   boot that follows.

   Read it in the *verdict* boot, not the write boot: if the hook parks, the
   write boot never returns from the operation, so any check placed after the
   call is unreachable by construction. That subtlety is why the first attempt
   at this instrument was wrong.

   The two witnesses then separate the remaining possibilities cleanly:

   | file present | console line | meaning |
   |---|---|---|
   | yes | yes | works; the timeout is in the harness's wait logic |
   | yes | no | console plumbing ate the marker |
   | no | no | the hook is never entered — look at the target match |

   **First run of the instrument was INCONCLUSIVE — do not read it as
   evidence.** The cell reported `TIMED-OUT` with no console marker, and the
   witness appeared absent, but the loopback mount used to probe the disk
   silently failed (`ls` of the image root returned nothing), so the disk was
   never actually read. Absence of the file was not observed; absence of a
   readable mount was.

   Probe procedure, corrected — both mistakes were made on the first attempt:

   - The witness lands at **`root/cast-at-phase` inside the image**, not at the
     image root: the guest mounts `/dev/vda` at `/mnt` and writes to
     `/mnt/root`.
   - **Do not discard mount errors.** Run the mount without `2>/dev/null` and
     confirm the image root lists non-empty before drawing any conclusion from
     a missing file.

   **Do not propose a fifth cause without reading that table.** Four have been
   wrong already: hook on the wrong route (true, fixed), cmdline parsing (ruled
   out), install throughput (falsified at 26s), and `>/dev/null 2>&1` in write
   mode (fixed, but not the cause). Every one of them came from treating a
   composite observation as if it isolated a single step.

   Note the cell is *useful as-is*: it produces a real recovery verdict on the
   fallback cut, which is how the `recovered-at-4` result above was obtained.

   **Superseded note — it is not install throughput.** A plain
   `OPS=(install) CUTS=(control)` cell completes in **26 seconds wall clock**,
   `recovery=clean driver=recovered-at-1 state=installed`. The install is fast.
   The earlier "the setup install never completes" reading was wrong: it
   inferred a slow install from a timeout that happened somewhere in the
   sequence, without timing the install by itself.

   **Nothing hangs at all.** The full `stage_and_activate` sequence
   (install → remove → activate) finishes in **40 seconds** under a `control`
   cut. So neither "slow install" nor "hung remove" is right, and the cause of
   the 600s phase-cut timeout is still unidentified — do not guess a third time,
   instrument the write-mode guest directly and compare it against control mode.

   **But the control cell found something better than the timeout would have:**

       activate  control
         recovery=PENDING  driver=stalled-at-PreviousArchived  state=absent
         requires BeginRollback { source: PreviousArchived }

   **That is one of the six pinned post-exchange stalls, reproduced on a real
   guest** — `(ActivateArchived, PreviousArchived)`, startup decides to roll
   back and no authority admits the decision. It needs **no crash injection at
   all**: a plain install, remove, activate reaches it. The pinned list was
   derived in-process; this confirms it is live, and makes
   `(ActivateArchived, PreviousArchived)` the one to fix first, with a
   ready-made reproduction that does not depend on the phase-cut machinery.

### Post-exchange admission — LANDED 2026-07-31 (`16015ec5`), effects still missing

Every post-exchange source is now admitted for every operation, with the
generation derived from `expected_forward_generation` instead of the hand-kept
`(operation, phase, generation)` table — that table was itself an instance of
the pattern, and the reason ActivateArchived had no post-exchange admission at
all. **All six pinned decision-gate stalls are gone; both pinned lists now
assert empty.**

Two exclusion tests encoded the old scoping and were updated deliberately:

- `..._require_post_and_exclude_activate_archived` → renamed
  `..._require_post_for_every_operation`. The "not supported, record unchanged,
  still pending" it asserted *was* the brick — the one reproducible in 40s by
  `install → remove → activate`. Its genuine invariant is kept: a pre-exchange
  namespace at a post-exchange source is incoherent evidence and is still
  refused, for every operation.
- `..._boot_repair_required_prefix_boundaries_...` — its sibling loop excluded
  NewState and ActivateArchived at `BootSyncStarted`, two of the six stalls. The
  neighbouring `BootSyncComplete` rejection still holds, since that phase maps
  to `RollForward`, not `BeginRollback`.

> **ADMISSION ONLY — the recovery is not complete.** The reverse-exchange and
> previous-restore effects do not exist for ActivateArchived, and the
> boot-repair authorities remain ActiveReblit's alone. These chains are expected
> to advance and then stall where the effects are missing. This was a
> deliberate, requested step to make the next gap visible and reproducible.
> **An empty pinned list is not evidence that post-exchange recovery works.**
> The same warning is repeated at both gates and both tests in the source; do
> not delete it until a crash-matrix cell proves the effects run.

**Measured on a real guest immediately after landing (41s, control cut):**

    before:  stalled-at-PreviousArchived   requires BeginRollback { PreviousArchived }
    after:   stalled-at-RollbackDecided    requires ResumeRollback { RollbackDecided }

The decision now persists — the record advanced `PreviousArchived →
RollbackDecided` — and the chain stops one step further in, exactly as the
admission-only caveat predicts.

**The next gap, identified:** `route_evidence_is_exact`
(`usr_rollback_resume_route_authority.rs`) requires
`previous_archive == RollbackAction::NotRequired` for ActivateArchived. A
rollback from `PreviousArchived` legitimately carries `Pending` — the archive
really happened and must be reversed. **Instance sixteen of the pattern**, and
the same shape as the fifteen before it.

**But do not fix it the same way.** Admitting it routes to
`PreviousRestoreIntent`, which needs the actual previous-restore effect — the
work `previous-restore-recovery-identity.md` built in-process (task #5) but
never proved across a reboot (§A4). This is where predicate-widening stops being
sufficient and the effects have to exist. Wire the effect and the admission
together, and use this 41-second cell as the acceptance test: it needs no crash
injection, no phase-cut machinery, and no VM timing budget.

   Note the remove step only became possible on 2026-07-30 — before the
   first-install staging fix, a displaced `/usr` placeholder wedged the next
   operation — so it is the newest and least-exercised part of this harness.
2. `state=absent` with the marker absent means the harness fell back to killing
   the guest 120s after `CELL-READY`, which landed **inside the install**. So
   the rollback measured above is a *NewState* rollback, not ActivateArchived.

Fix (1) before drawing any ActivateArchived conclusion from this cell. The
harness's `KERNEL=` line was also still pointing at the root-only
`/boot/vmlinuz-*` — that fix had been written in this plan but never applied to
the script; it now prefers a readable `/tmp/vmlinuz` and exits loudly instead of
printing "phase never reached" for a guest that never booted.

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

### A3. ANSWERED 2026-07-31 — yes, and worse than the question assumed

**Do ActiveReblit and ActivateArchived share NewState's pre-exchange recovery
gap?** They did, and NewState had it too. The §1.4 scoping was not a
conservative subset of a working mechanism; it was the mechanism working for
exactly one measured window and silently bricking every other.

Measured, not inferred:

- **ActivateArchived** — 26 consecutive recovery attempts frozen at
  `CandidatePreserveIntent`, `state=absent`, on a real guest.
- **ActiveReblit** — stranded from `CandidatePrepared`,
  `TransactionTriggersStarted` and `TransactionTriggersComplete`.
- **NewState** — also stranded from `Preparing`, `FreshStateAllocating`,
  `FreshStateAllocated` and `CandidatePrepareStarted`. The operation the fix was
  scoped *to* was itself only half covered.

All fixed. Every pre-exchange source of every operation now decides, routes,
walks its whole chain, and finalizes; the stranded lists assert empty.

**The original instruction here was half right.** "Not by widening a shared
predicate" correctly predicted the danger — three over-widenings happened during
this work, every one caught by an *exclusion* test rather than by the admission
invariants added alongside. But "a crash-matrix cell per operation" was not what
found these. The head/tail asymmetry was visible in the source the whole time:
the gates that *decide* to roll back never carried the NewState restriction the
gates that *carry it out* did. One in-process test comparing the two ends found
in minutes what three crash-matrix sessions had not.

Keep both. The cells prove effects run; the paired-invariant tests prove the
contract is not self-contradictory. This gap was the second kind.

### A3b 2026-07-31 — the post-exchange span, asked the same question

The pre-exchange chain-walk test was pinned empty, and nothing asked the same
question of the sources where a rollback has physical work to do. Asking it
(`admitted_post_exchange_rollback_routes_always_have_a_consuming_successor`, all
eight post-exchange sources × three operations = 20 buildable cases) found
**six more instances of the pattern and two genuinely missing effects.**

The six were the same shape as the sixteen before them, in five separate gates:

| gate | hard-coded rule | derived from |
|---|---|---|
| resume route | nine `(operation, phase, source, generation)` tuples | `rollback_source_is_on_chain` |
| resume route, reverse, preserve, fresh-db, complete, finalize | `operation == ActiveReblit && source == BootSyncStarted` | `boot_rollback_is_possible(source)` |
| all six | `previous_archive == NotRequired` | `previous_restore_rollback_is_possible` |
| all six | `ActivateArchived ⇒ Rearchive` | `candidate_disposition_for` |
| reverse | `NewState ⇒ fresh_db == Pending` | `fresh_db_rollback_is_possible` |
| preserve, fresh-db, complete, finalize | four more source tuple tables | `rollback_source_is_on_chain` |

Each hand-kept table was a copy of the journal's own forward chain, and each was
incomplete in the way copies always are. Concretely, before this: an
`ActivateArchived` cut during its system triggers reached `RollbackDecided` and
stopped — the decision gate admitted it (landed `16015ec5`), the resume route
had no row for it. That is the §1.4 stall moved one phase later, which is
exactly what the ADMISSION ONLY caveat warned would happen.

**`rollback_evidence_is_on_chain(record)` now answers this for every gate.** A
phase the record's options never make it traverse is not a source it can claim;
a phase past `CommitDecided` is not a source at all. There is nothing left to
keep in sync.

#### The tables had one real job, and dropping it was caught immediately

Replacing them with chain membership alone lost the **generation** column, and
three exclusion tests failed on the next full run — the fourth time in this
epic that an exclusion test caught an over-widening the admission invariants
waved through. The generation is what ties a rollback to the exact forward
phase it began at: without it, a plan whose `source` was edited while its
generation stayed put was admitted.

So the check is kept and derived too, as `rollback_generation_is_reachable`:
replay the plan — forward to the source, one advance for the decision, then the
advances the plan's own action states account for — and require the record's
generation to land in that span. It is a span rather than a value because one
cost is genuinely ambiguous: `AlreadySatisfied` is legal both as a
decision-time observation (no intent phase, no advances) and as the outcome of
completing an intent (two advances). Taking both keeps it from ever refusing a
legal record, and it still rejects every edit above, which move the generation
outside the span entirely. `RollbackComplete` is the one phase with two
routes, and the plan says which: `boot == NotRequired` means it came straight
from the last ordinary action, a resolved boot means it came through the whole
repair tail.

`expected_rollback_complete_generation`'s hard-coded
`ROLLBACK_ADVANCES_TO_COMPLETE = 6` in the ActivateArchived finalization gate is
the same fact, still hand-written; it goes when that gate is derived (below).

One test needed a real fix rather than an update:
`startup_fresh_db_invalidation_plan_accepts_only_the_exact_new_state_pending_fresh_action`
swapped `plan.source` between three phases while leaving the generation, so it
had been asserting the gate admits evidence no legal record carries. It now
shifts the generation with the source.

One exclusion test was retired on purpose, not updated:
`sibling_and_legacy_plan_predicates_are_rejected` asserted that a NewState or
ActivateArchived boot-sync rollback prefix is refused by the shared gates, on
the premise that only ActiveReblit can crash during boot sync. It cannot — the
journal builds those plans — so refusing them *was* the stall. The shared gates
now admit them at the layout each phase implies and refuse the other; the
sibling gap that genuinely remains is the boot-repair tail, and it is pinned
below rather than disguised as a refusal there.

Four stalls remain, pinned, and **none of them is a predicate** — two missing
effects, not four bugs:

    NewState         @ BootSyncStarted       -> CandidatePreserved
    ActivateArchived @ PreviousArchiveIntent -> PreviousRestoreIntent
    ActivateArchived @ PreviousArchived      -> PreviousRestoreIntent
    ActivateArchived @ BootSyncStarted       -> PreviousRestoreIntent

The first is §B: `BootRepairRequired` and everything past it has authorities for
`ActiveReblit` alone, so a `NewState` or `ActivateArchived` cut during boot sync
reaches the first phase that would route there and has nowhere to go. The other
three are the previous-restore dispatcher, which is now built but not reachable
— PR1-BLOCKED, below.

### A3c 2026-08-01 — the four per-operation terminal gates

`usr_rollback_activate_archived_{finalization,complete_route}_authority` and the
ActiveReblit pair were untouched by the above and still carried every rule the
six shared gates had shed: their own source tuple tables,
`previous_archive == NotRequired`, the disposition by operation name, and — in
both ActivateArchived gates — a surviving `!rollback.external_effects_may_remain`,
the exact "disguised copy" this plan had already recorded as fixed everywhere.
It was not.

They were invisible because the walk's `_ => true` arm waved every
operation-specific terminal phase through. **A phase nothing calls is a phase
nothing checks** — that arm is now `terminal_gate_admits`, which dispatches to
the real predicate for each `(operation, phase)` pair.

Wiring it surfaced one bug no derivation would have found:
`active_reblit_finalization_plan_is_exact` demanded `boot == NotRequired` at
`RollbackComplete`. But `NotRequired` is only one of the two ways a rollback
legitimately arrives there — the other is through the boot-repair route
ActiveReblit owns, which leaves the action `Applied`, `AlreadySatisfied`, or
`Unverified`. So an ActiveReblit boot-sync crash could complete its repair and
still never finalize. Now only `PendingUnverifiable` is refused, because that
repair is still owed. The *route* gates keep the absolute check, correctly: a
plan with repair outstanding belongs on the repair route, not on completion.

**This one was load-bearing, and two tests had frozen it.**
`boot_repair_complete_route_matrix` asserted the record stays at
`RollbackComplete` on the next entry, and
`..._all_journal_faults_converge_without_finalization` said so in its name.
Neither was idempotence: `recovery_disposition` returns `FinalizeRollback` for
those records, so the journal was asking for a finalization the gate refused —
the §1.4 head/tail disagreement, at the last phase. The whole ActiveReblit
boot-repair path therefore ended with the journal record still on disk, which
fails the startup baseline on every later boot. Both tests now assert the
finalization; the second is renamed `..._converge_and_finalize`.

`expected_rollback_complete_generation` and its hand-written
`ROLLBACK_ADVANCES_TO_COMPLETE = 6` are gone, subsumed by
`rollback_generation_is_reachable`.

The NewState boot-sync stall also moved one phase earlier — `CandidatePreserved`
rather than `FreshDbInvalidated` — because that is the first phase whose plan
routes to `BootRepairRequired`, and the walk now asks there. Same stall, earlier
and more accurate detection.

### PR2-FIXED 2026-08-01 — it was the fixture's parking name all along

`classify_root_name` **already** accepts `.previous-slot-<state>-<token>-<index>`.
It requires the token to be the record's own predecessor token, which is exactly
how `select_previous_archive_slot` builds the name in production. The fixture
did not: `fixture_previous_archive_slot()` hard-coded `"a".repeat(32)`. So the
recorded slot named a directory the capture could never classify, and a restore
that worked perfectly reconciled as `Ambiguous`.

Same class as the two generation-incoherent fixtures §A3b turned up: **a fixture
fabricating a value that production derives, and a test suite that never
compared them.** Third one this epic.

The fixture now takes both the state and the token from the record. Nothing in
production changed.

**The dispatcher is wired on.** `PREVIOUS_RESTORE_DISPATCH_IS_WIRED` is `true`,
and `startup_new_state_previous_archived_fails_safe_pending_not_bricked` — the
test `previous-restore-recovery-identity.md` named in its exit criteria — now
watches the chain go `RollbackDecided -> PreviousRestoreIntent ->
PreviousRestoredToStaging` with the plan recording `previous_archive: Applied`.
The predecessor really comes back out of its slot.

**Two of the four pinned post-exchange stalls are closed.** What remains is §B
for both operations:

    NewState         @ BootSyncStarted -> CandidatePreserved
    ActivateArchived @ BootSyncStarted -> CandidatePreserved

### PR3 MEASURED 2026-08-01 — the restore runs on a real guest

`OPS=(activate) CUTS=(control)` against a `cast` built from this work:

    before (2026-07-31):
      driver=stalled-at-PreviousArchived        state=absent
      requires BeginRollback { source: PreviousArchived }

    now:
      PHASE-1: PreviousRestoreIntent
      PHASE-2: PreviousRestoredToStaging
      driver=stalled-at-PreviousRestoredToStaging  state=absent
      requires ResumeRollback { phase: PreviousRestoredToStaging }

**The chain advanced two phases and the predecessor came out of its slot on real
hardware.** That is the acceptance test for the effect, and it passes. Nothing
here is in-process any more.

**And it exposed the next gap immediately.**
`UsrRollbackResumeRouteAuthority::capture` admits
`matches!(record.phase, Phase::RollbackDecided | Phase::UsrRestored)`.
`PreviousRestoredToStaging` is a routing phase like those two — it carries no
outcome and exists to select the next intent — but it is not in the list, so
once the restore completes nothing carries the rollback onward.

**The in-process walk did not catch this, and the reason is embarrassing:** the
walk has no arm for `PreviousRestoredToStaging`, so it fell through `_ => true`.
The same catch-all mistake this plan already recorded as a standing hazard, made
again in the test written to enforce it. Enumerate the arm.

### PR4 FIXED + MEASURED 2026-08-01 — five phases on a guest

`PreviousRestoredToStaging` is now admitted by the resume route with its own
`route_evidence_is_exact` arm (`usr_exchange: Pending`, layout `Post` — the
restore is done, the exchange is not), and the walk has an explicit arm for it
instead of the catch-all.

That one routing phase was holding back **two** effects, not one. Re-running the
same cell:

    PHASE-1: PreviousRestoreIntent
    PHASE-2: PreviousRestoredToStaging
    PHASE-3: ReverseExchangeIntent
    PHASE-4: UsrRestored
    PHASE-5: CandidatePreserveIntent
    driver=stalled-at-CandidatePreserveIntent  state=absent

So on real hardware the rollback now un-archives the predecessor, routes, and
**reverses the `/usr` exchange onto it** — `previous_archive: Applied,
usr_exchange: Applied`. Where this started the morning of 2026-07-31 it stalled
at `PreviousArchived` having done nothing at all.

**Next, and it is a fresh finding, not a known gap:** it stalls at
`CandidatePreserveIntent` with `requires ResumeRollback { phase:
CandidatePreserveIntent }`, i.e. the candidate-preserve authority defers.
`usr_rollback_activate_archived::dispatch` does handle this phase, so this is a
*deferral* — evidence the gate refuses — not a missing consumer.

**The harness cannot show you which clause, and that is the first thing to
fix.** `eprintln!` diagnostics were added at every deferral point in
`UsrRollbackCandidatePreserveAuthority::capture`, staged to the guest, and run:
**none of them appeared**, including one placed unconditionally. They were never
missing — the driver loop is

    DRV=$(cast -D /mnt/root -y install bash-completion 2>&1)

so every line cast writes goes into `$DRV`, and the only thing echoed on a stall
is `tail -1 | cut -c1-200`. Anything a diagnostic prints is captured and thrown
away.

**The harness now surfaces it** — on stall it prints a `DIAG-BEGIN`/`DIAG-END`
block containing every `$DRV` line matching `$DIAG_GREP` (default `-DIAG`). The
markers are unconditional, so "no matches" is distinguishable from "output was
eaten". With that in place the deferral named itself on the first run:

    CP-DIAG reached-capture phase=CandidatePreserveIntent
    CP-DIAG namespace-begin UnexpectedParkingWrapper

### PR5 DIAGNOSED 2026-08-01 — the parking wrapper, one phase later

`candidate_preserve_topology_after_phase` refuses any snapshot that still holds
a `PreviousParking` (or `ArchivedCandidateParking`) wrapper, for every operation
except `ActiveReblit`:

    if record.operation != Operation::ActiveReblit
        && snapshot.wrappers().any(|w| matches!(w.role,
            ArchivedCandidateParking { .. } | PreviousParking { .. }))
    { return Err(UnexpectedParkingWrapper) }

That rule predates the previous-restore path and encodes an assumption that is
no longer true: **after a restore, a `PreviousParking` wrapper is exactly what
the namespace is supposed to contain.** The restore vacates the slot and leaves
it parked — deliberately, so ambient, replaced, moved, or populated directories
survive — and `classify_root_name` already treats it as a legal root entry. This
gate is the one place that still calls it unexpected.

This is the same residue PR2 chased, resurfacing one phase later, exactly as
predicted: *"leaving it means every subsequent capture at every later phase
trips on the same entry."* That prediction was right; the conclusion drawn from
it (retire the wrapper) was wrong. The wrapper stays; the gates learn it.

**Fixed narrowly, not widened.** `is_own_vacated_previous_parking` accepts a
parked slot only when all three hold: it belongs to this record's predecessor,
the record carries the `previous_archive_slot` that makes the restore reversible
at all, and the restore is settled. Stale residue from anything else is still
refused, and `ArchivedCandidateParking` is untouched.

**In-process the chain then ran further than expected**: not just past the
wrapper check but through candidate preservation itself —
`previous_archive: Applied, usr_exchange: Applied, candidate.action: Applied`,
with only `fresh_db` outstanding. One namespace clause was gating three effects.

**On the guest it still stalls at `CandidatePreserveIntent`**, and
re-instrumenting said why in one run:

    CP-DIAG reached op=ActivateArchived phase=CandidatePreserveIntent
    CP-DIAG ns-begin UnexpectedParkingWrapper

**Same error, different wrapper.** That one `match` returns
`UnexpectedParkingWrapper` for *two* roles, and the fix only relaxed one:

    TreeLocation::ArchivedCandidateParking { .. } => true,   // still unconditional
    TreeLocation::PreviousParking { state, .. } => !is_own_vacated_previous_parking(..),

`ActivateArchived` takes its candidate *from* an archived slot, so its rollback
leaves an `ArchivedCandidateParking` wrapper by the same mechanism that leaves
the previous one — and that arm was never revisited. My guess that the
difference was the archived-candidate *move path* was wrong; the difference is
one arm of the wrapper check, in the code I had just edited.

**Attempted and reverted 2026-08-02.** The mirror helper
(`is_own_archived_candidate_parking`: operation `ActivateArchived`, candidate
origin `Archived`, matching state *and* tree token) did not clear the guest
stall, and it broke an exclusion test —
`startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states`.
Both facts point the same way: the archived arm is **not** a straight mirror of
the previous one, and the refusal it encodes is load-bearing in a way the
previous arm's was not. Reverted rather than pushed; an unproven relaxation of a
namespace check is what this epic exists to undo.

**The exclusion test is the specification, and it is deliberate.** It creates
`.archived-candidate-slot-<candidate_state>-<candidate_token>-0` using **this
record's own** candidate state and token, and requires `Deferred`. So the
refusal is not a stale operation-name check like the sixteen before it: at
candidate preservation a *parking* name for the archived candidate means an
abandoned staging move, because a rearchived candidate belongs at its canonical
state name (`ArchivedStagedWithCanonicalSlot`). The mirror was wrong on the
merits, not merely untested.

**Re-instrumented, and the namespace is now known rather than assumed:**

    CP-DIAG offending-wrapper ArchivedCandidateParking { state: 1, token: "9d85…", index: 0 }
                              prev_id=Some(2) prev_archive=Some(Applied)
    CP-DIAG offending-wrapper PreviousParking { state: 2, index: 0 }
                              prev_id=Some(2) prev_archive=Some(Applied)

Two wrappers, and **only the first is the blocker** — `is_own_vacated_previous_parking`
already accepts the second (`state: 2` matches `prev_id`, `previous_archive` is
`Applied`). That fix is doing its job.

The remaining one is the candidate's own archived slot, state 1, still parked
after the whole forward activation and four rollback phases.

**The forward staging is not broken.** `MoveDirection::Stage::marker_after()` is
`MarkerLocation::Displaced`: a successful archived-candidate staging *parks* the
slot on purpose, so the canonical state name is free while the candidate is
live. A parked `ArchivedCandidateParking` wrapper during an in-flight
`ActivateArchived` is the modelled state, not residue. There is nothing to fix
on that path — and the instinct to go fix it was the fourth iteration of the
same error this section keeps recording.

**The two observations only look contradictory because the topologies differ.**
The exclusion test builds the archived-staged shape at its **canonical** slot
(`create_archived_staged_topology`) and then adds a parking directory *on top* —
canonical *plus* a stray parking name, which is genuine residue and must stay
refused. The guest has **only** the parked slot and no canonical one, which is
what a live activation looks like. The gate treats both as "any parking wrapper
present" and so cannot tell them apart.

**Verified before changing anything, and the roles were exactly as predicted:**

    CP-DIAG role ArchivedCandidateParking { state: 1, token: "3e46…", index: 0 }
    CP-DIAG role PreviousParking { state: 2, index: 0 }
    CP-DIAG role AmbientQuarantine("isolation")
    CP-DIAG role Staging
    CP-DIAG candidate_id=Some(1) previous_id=Some(2)

No `State(1)`. So `is_own_displaced_candidate_slot` now keys the refusal on the
*pair* — a parking name is residue only when a canonical slot for the same state
sits beside it — and the exclusion test still passes.

### PR6 2026-08-02 — past the wrapper, into the topology

Re-instrumenting **every** deferral point (not just the wrapper site) named the
next one in a single run:

    CP-DIAG d3-ns-begin CandidateWrapperMissing

So the parking-wrapper refusal really is behind us, and the block has moved to
`candidate_preserve_topology_*`, which cannot classify this namespace at all:
it looks for the candidate's wrapper and there is none, because the slot is
*parked*, not canonical.

**This is the same premise one level up.** The archived rollback topology is
`ArchivedStagedWithCanonicalSlot` — it models a candidate whose state slot is at
its canonical name. A coordinated `ActivateArchived` never presents that shape:
`MoveDirection::Stage` displaces the slot on purpose, and the rearchive
(`marker_after() == Candidate`) is what would restore the canonical name — the
very step being refused. The topology enum has no variant for "archived
candidate live, slot displaced", which is precisely the state every activation
rollback starts from.

**Fixed without a new variant.** `archived_topology` looked the slot up strictly
by `TreeLocation::State(state)`; it now accepts either that or
`ArchivedCandidateParking { state, .. }`, because those are the slot's two
modelled names and `Stage`/`Rearchive` move it between them. Every other check
is unchanged — `slot_identity` must still match state *and* token, the slot must
still be empty, staging must still contain the candidate — and `one_wrapper`
still requires exactly one match, so canonical *plus* parking remains the
conflict the refusal suite pins. No topology variant was added because the shape
is the same shape; only the name differed.

### PR7 2026-08-02 — admission clears, the effect does not

    PHASE-5: CandidatePreserveIntent
    PHASE-6: ?
    STALL: dispatch the exact startup ActivateArchived candidate-preservation
           checkpoint: dispatch exact startup ActivateArchived candidate preserv…

The rollback now gets *past* candidate-preservation admission — six phases — and
fails inside the dispatch instead. That is a different layer: the authority was
issued, so the namespace proof and plan predicate both passed.

**The harness was hiding the answer twice over, and both are fixed.** The
`cut -c1-200` kept the summary tidy and discarded the end of every error chain —
which is exactly where the cause lives, because these errors nest source-last.
It now prints the whole line, folded. And the phase regex only matched
`at <Phase> requires`, which a dispatcher error never contains, so the verdict
degraded to the one reading that says nothing; it now falls back to naming the
layer (`dispatch-ActivateArchived`).

With that, one run gave the whole chain:

    Error: install: establish clean system-client startup baseline:
      dispatch the exact startup ActivateArchived candidate-preservation checkpoint:
      dispatch exact startup ActivateArchived candidate preservation:
      consume and reconcile exact operation-specific candidate-preservation authority:
      revalidate the independent candidate-preservation namespace proof:
      capture or reconcile an exact archived candidate child-move effect:
      **the retained canonical archived candidate wrapper occurs 0 times**

### PR8 — the same canonical assumption, one layer down

The *effect* capture has its own lookup for "the retained **canonical** archived
candidate wrapper", and it is the identical assumption just fixed in
`archived_topology`: the slot is parked, not canonical, so the count is zero.
One rule, two copies, fixed one at a time — the shape this plan has recorded
sixteen times.

**Fix in the archived child-move capture** (`capture/archived_candidate_preserve.rs`
and its consumer in `usr_rollback_candidate_preserve_authority/archived_effect.rs`),
the same way: accept the slot at either `State(state)` or
`ArchivedCandidateParking { state, .. }`, keep every identity check, and keep
requiring exactly one match so canonical-plus-parking stays a conflict.

**The grep was run first this time, and it found a third.** Fixing the
fingerprint capture (`exact_wrapper`, plus the `other_root_wrappers` filter that
must exclude the same slot) moved the failure straight to
`exact_retained_wrapper` at `archived_candidate_preserve.rs:448` — visible only
because the two produce slightly different messages (`the retained canonical…`
became `retained canonical…`).

**The third copy is not a mechanical repeat, and was deliberately left.** It
builds the move's retained parents, and derives both `target_name` and
`target_path` from `state.to_string()` — the *canonical* name — so it is not
just a lookup but the rename **destination**. Whether a rearchive from a parked
slot should target the canonical name (because `Rearchive`'s `marker_after()` is
`Candidate`, so the identity layer renames the slot back) or the parking name
(and let the marker move afterwards) decides where a tree physically lands.
Getting that wrong moves a real `/usr` to the wrong place, which is the worst
class of change in this crate.

**The ordering is settled.** `move_archived_candidate` calls
`restore_displaced_slot_if_parked` as the **first** step of a `Rearchive`, so
the slot is back at its canonical name *before* the child move. The destination
name was therefore always right; only the lookup ran too early. Fixed with
`exact_retained_wrapper_matching`, which finds the wrapper at either name and
still demands exactly one — a rename does not change the inode, so the retained
descriptor is the same one either way.

### PR9 — the pin, not the name

    capture or reconcile an exact archived candidate child-move effect:
    pin activation-namespace directory at `/mnt/root/.cast/root/1`:
    No such file or directory (os error 2)

The wrapper is found now, but the capture still *pins* the retained parent at
the canonical **path**, which does not exist yet — the slot is parked at capture
time and only becomes `1/` when the rearchive unparks it.

So the two must be separated, and this is the distinction the whole PR8/PR9 pair
turns on: **pin at the slot's current path, keep the destination name
canonical.** `target_path = roots_path.join(state.to_string())` is what needs to
follow the wrapper; `target_name` stays `state.to_string()` because that is what
the slot will be called when the move happens.

`WrapperFingerprint` carries its own `name`, so this was a one-line change:
`target_path` now follows the matched wrapper. And the result is the most
useful failure of the sequence:

    pin activation-namespace directory at
    `/mnt/root/.cast/root/.archived-candidate-slot-1-6df27ea4…-0`:
    No such file or directory (os error 2)

**Neither name works, because the name is not stable.** The slot is at the
parking name when the capture pins it and at the canonical name after
`Rearchive` unparks it, so *any* path recorded at capture time is wrong by the
time `revalidate_value_identity` re-pins it. Chasing the right string is the
wrong shape of fix.

**Reading what actually pins settled it in one pass.** `clone_descriptor` is a
`try_clone` and `controlled_directory_witness` reads the fd — both take a path
only for the error message. The single by-path reopen is
`open_directory(&self.roots, &self.target_name, …)`, and its job is exactly one
proof: *the retained descriptor is still the directory reachable at this name*.
`target_name` had no other use — nothing renames by it — so holding it canonical
was simply wrong. Both name and path now follow the matched wrapper.

### PR10 — seven phases, and the effect ran

    PHASE-6: dispatch-ActivateArchived
    PHASE-7: CandidatePreserveIntent
    … at CandidatePreserveIntent requires ResumeRollback { … }; recovery effects
      remain blocked by [ActivationNamespaceRejected, PhaseNamespaceConflict,
      ExactNamespaceInventoryRequired]

The capture cleared, the child move dispatched, and the record came back to
`CandidatePreserveIntent` — so this is no longer a dispatch failure but a
namespace the *policy* will not classify afterwards.

`rollback_layouts` gives `CandidatePreserveIntent` two alternatives:
`PRE_EXCHANGE` and `preserved = { candidate: Destination, previous: Live }`.
The predecessor is Live (restored), so the suspect is `CandidatePlace::Destination`:
`candidate_destination` almost certainly expects the candidate at
`TreeLocation::State(n)`, and if the slot is still parked the candidate sits at
`ArchivedCandidateParking` instead — the same canonical-name assumption, now in
`policy.rs` rather than the capture.

**Measured, and it is *not* `candidate_destination`.** A DIAG on the
`PhaseLayout` branch of `assess_snapshot_layout` — the only place that reports a
candidate at an unexpected location — printed **nothing**. So the refusal
happens before layout selection is ever reached, which rules out the obvious
suspect and the whole "fifth copy of the canonical-name assumption" theory with
it.

What runs earlier, in order: `capture_snapshot` itself (the
`ExactNamespaceInventoryRequired` blocker points here), then
`CandidateCount != 1`, then `expected_layouts`, then `PreviousCount`. The
candidate token appearing at zero or two locations after the child move is the
most likely of those and would explain all three blockers at once.

**Second probe, and it narrows further: `assess_snapshot_layout` is never
called.** An *unconditional* print at the very top of that function — before any
branch — produced nothing either. The channel was live (the harness default
pattern `-DIAG` matches `NS-DIAG` as a substring, which is how the earlier
`CP-DIAG` lines came through), so this is absence of a call, not absence of
output.

That puts the failure entirely inside `capture_snapshot`, which is exactly what
`ExactNamespaceInventoryRequired` says and which no policy predicate can affect.
Every remaining theory about layouts, dispositions, and canonical names is
therefore dead for this stall.

**Third probe named it, as predicted, in one run.** Wrapping `capture_snapshot`
rather than instrumenting each `?`:

    CAP-DIAG enter phase=CandidatePreserveIntent
    CAP-DIAG error phase=CandidatePreserveIntent ParkingWrapperContainsTree {
      path: "…/.cast/root/.archived-candidate-slot-1-cfef1642…-0" }

### PR11 — the rearchive moves the candidate into a *parked* slot

`wrappers.rs:498` refuses any `ArchivedCandidateParking` or `PreviousParking`
wrapper that contains a `usr` tree: a parking name is assumed inert. After the
rollback's rearchive it is not — the candidate has been moved **into** the slot
while the slot still carries its parking name.

That is worth pausing on, because it contradicts the reading in PR8: the
ordering there said `MoveDirection::Rearchive` calls
`restore_displaced_slot_if_parked` *first*, so the slot should have been
canonical before the child move. The namespace says otherwise. **One of the two
is wrong, and the namespace is the evidence.** Either the unpark did not run on
this path, or it ran and something re-parked the slot, or the rollback's
rearchive is a different code path from the one whose ordering was read.

**Established by reading the callers, and it is the third possibility: two
different rearchive paths.**

`rearchive_archived_candidate` — the primitive whose ordering PR8 quoted, the
one that calls `restore_displaced_slot_if_parked` first — has exactly two
callers: the **legacy** `stateful_recovery.rs` and its tests. The coordinated
rollback never touches it. Its move is
`capture/archived_candidate_preserve/target_durability.rs`, which does

    renameat2_noreplace_once(staging, c"usr", target, c"usr")

against the retained `target` descriptor — the slot as captured, i.e. **still
parked**. There is no unpark anywhere on that path.

So PR8's ordering was correct about the primitive it read and irrelevant to the
path that actually runs. That is the fourth time this section has recorded a
conclusion drawn from the wrong copy of a routine.

**The fix is not in `wrappers.rs`.** The emptiness rule is right: a parking name
is inert, and a tree inside one means the namespace is genuinely inconsistent.
What is missing is the unpark. The coordinated rearchive has to restore the slot
to its canonical `<state>` name, as the legacy path does — otherwise a rolled-back
activation leaves the system with no canonical state directory at all, which is
worse than the stall it replaces.

**Done, and the chain completes.** `restore_canonical_slot_name` runs in
`attempt_move_once`, immediately before the `renameat2` and after the final
exact-PRE revalidation. It is a no-op when the slot is already canonical, so
every path that never parked is untouched, and the retained `target` descriptor
survives the rename — same inode — so all later revalidation through it holds.

    activate  control
      PHASE-1: PreviousRestoreIntent
      PHASE-2: PreviousRestoredToStaging
      PHASE-3: ReverseExchangeIntent
      PHASE-4: UsrRestored
      PHASE-5: CandidatePreserveIntent
      PHASE-6: dispatch-ActivateArchived
      PHASE-7: CandidatePreserved
      PHASE-8: RollbackComplete
      recovery=PENDING  driver=recovered-at-9  state=installed

## §A ACTIVATION ROLLBACK RECOVERS — 2026-08-02, on a guest

    2026-07-31:  driver=stalled-at-PreviousArchived   state=absent
    2026-08-02:  driver=recovered-at-9                state=installed

The `ActivateArchived` rollback now un-archives its predecessor, reverses the
`/usr` exchange, preserves the candidate, finalizes, **and the machine goes on
to install successfully** — `state=installed`, not merely "no longer stalled".
That is the first end-to-end activation-rollback recovery this epic has
measured, against 26 consecutive frozen attempts with nothing installed when it
started.

Eleven distinct defects stood between those two lines, and every one of them was
found by printing what was on disk rather than by reasoning about which
predicate looked wrong. Four times a conclusion drawn from reading code was
contradicted by the namespace; the namespace was right every time.

Full suite green alongside it: **2755 passed, 0 failed**, zero production
warnings — including the `receipt_promotion` cluster that had been failing
intermittently under full-suite parallelism.

**And the phase-targeted cut recovers as well**, so this is not limited to a
clean activation failure:

    activate  phase:ActivateArchived.CandidatePrepared
      PHASE-1: CandidatePreserveIntent
      PHASE-2: CandidatePreserved
      PHASE-3: RollbackComplete
      driver=recovered-at-4  state=installed

A real power cut inside the transition, parked at `CandidatePrepared`, now
rolls back and installs. Two cells, two cut modes, both `state=installed`.

**Still unproven and worth naming precisely:** the other `OPS` (`install`,
active-reblit), the other cut phases, `NewState`/`ActivateArchived` boot-sync
sources (§B — the two pinned stalls that remain), and §A4's cross-reboot matrix.
Two green cells are two green cells, not a matrix.

Note for whoever runs it: `DIAG_GREP` does **not** reach the guest.
`crash-matrix-run.sh` reconstructs the driver with `bash -c "$(declare -f run)"`,
so host environment does not propagate; the default `-DIAG` pattern is what
actually applies. Either tag every diagnostic with a `-DIAG` suffix, as these
did, or thread the variable through the `bash -c` line.

Worth noting the shape: **the fix was one line from complete and I re-ran
without re-reading the branch I had just changed.** The diagnostic caught it in
40 seconds, which is the argument for instrumenting before theorising, not
after.

That is the third time this epic that an instrument was installed and read as
evidence of absence when it was really absence of a channel — the `/tmp` witness
on tmpfs, the mount that silently failed, and now this. **Prove the channel
carries a known-present line before trusting a missing one.**

Still outstanding after that: the rest of the chain to `RollbackComplete`, and
§A4's reboot matrix.

### PR2, as it was first diagnosed — kept because the first two readings were wrong

With PR1 fixed the effect actually executes: the predecessor moves out of its
archived slot and back into staging. The reconciliation then reads the namespace
to decide the outcome — and gets:

    PR-DIAG ambiguous layout=Capture(UnexpectedRootName {
        name: ".previous-slot-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-0" })

**The restore leaves the emptied parking wrapper behind in the roots
directory**, and `capture_snapshot` rejects it as an unexpected root entry. So
the reconciliation classifies a move that *did* happen as `Ambiguous` and
refuses to record it — which is the correct conservative answer to the evidence
it is given, and the wrong outcome.

**First conclusion, and it was wrong.** I read
`finish_not_applied_previous_archive` — "retires an exact inert state slot …
moved back to a non-state parking name rather than deleted" — as the missing
counterpart, and wired it into the restore direction. It changed nothing,
because **retirement *produces* the parking name; it does not remove it.** It
renames `<state-id>` to `.previous-slot-<id>-<token>-<n>`, and when the slot is
already parked (which it is after adoption) it does nothing at all.

So the parked wrapper is not residue at all — it is the **intended** end state
of both directions, kept deliberately so ambient, replaced, moved, or populated
directories survive. The gap is on the other side: `capture_snapshot` does not
know that a `.previous-slot-*` entry in the roots directory is legitimate, and
reports it as `UnexpectedRootName`.

**Second conclusion, also wrong**, though closer: "the fix is in the namespace
capture, which does not know the parked wrapper is legitimate." It does know —
see PR2-FIXED above. What it does not accept is a parking name whose token is
not the record's, and only the fixture ever produced one of those.

The speculative retirement call was reverted — it was a no-op that only added a
failure path.

**Both wrong readings came from the same habit:** seeing a rejection and asking
what production should do differently, instead of asking whether the evidence
was real. The rejection was correct every time.

Note the identity round-trip test never saw this: it asserts the inode landed
in staging and never captures the namespace. Same shape as the lock seam — the
primitive is proven, its *surroundings* are not.

The gate is therefore back off behind `PREVIOUS_RESTORE_DISPATCH_IS_WIRED`.

### PR1-FIXED 2026-08-01 — the recovery identity no longer fights its own lock

The first blocker was:

    PR-DIAG attempt=Restore(Journal(AcquireLock { WouldBlock })) layout=LayoutChanged

`prepare_previous_restore_recovery` opened **its own** journal handle through
`JournalAcquisition::RecoveryNonblocking`, which `try_open`s the canonical lock.
The dispatcher is already holding that lock — it must, because the exact record
binding is what authorizes the effect.

`previous-restore-recovery-identity.md` predicted this exactly and nobody acted
on it: *"journal ← already held by the recovery dispatcher; must **not**
re-acquire"*. The seam was written down and then built the other way.

**The handle turned out to be pure overhead on that path.** It exists only to
prove a clean baseline, and recovery skips that proof by design — so it was
opened, locked, and dropped unread. `StatefulTreeIdentity::journal` is now
`Option`, recovery stores `None`, and `retained_journal()` unwraps for the
forward-coordinator and legacy paths, none of which a recovery identity reaches.

**Two more bugs fell out of the same read.**

The *second* `require_clean_baseline`, at the end of `prepare_candidate`, ran
unconditionally including for recovery — so that path could only ever have
succeeded with no journal record present, which is to say only in tests. Now
conditional like the first.

And `verify_previous_for_recovery` / `verify_candidate_for_recovery` — functions
whose names say recovery — called `require_no_journal`, demanding the absence of
the very record that makes recovery necessary. `require_no_journal` now returns
`Ok` when there is no handle to inspect: the guard exists to stop a *legacy*
effect running across an unreconciled crash, and for recovery that job is done
by `PreviousRestoreRecoverySeal`, which only the dispatcher that proved the exact
record can mint and which is required to construct the identity at all.

The first attempt made that case a panic — "a recovery identity reaches none of
these paths" — and two tests immediately proved it wrong by reaching one through
the legacy `restore_previous`. The claim was never checked before it was
asserted; the tests were.

Until PR2 the four pinned stalls stand, and the two `PreviousRestoreIntent` ones
are *not* closed. The walk asserts the admission is exact and still reports them
stranded, because nothing in production consumes the phase — saying otherwise
would be the `_ => true` mistake with extra steps.

    activation_namespace/rollback_previous_restore_proof.rs   namespace typestate
    usr_rollback_previous_restore_authority.rs                sealed admission
      + effect_reconciliation.rs                              the one-shot move
    startup_recovery/usr_rollback_previous_restore_dispatch.rs
    startup_recovery/usr_rollback_previous_restore_persistence.rs
    startup_gate.rs                                           before the reverse step

`policy.rs` already named the two legal layouts for the phase, so the admission
is a typestate rather than a boolean: `PREVIOUS_ARCHIVED` is `Apply` (the
compensating move still has to run) and `POST_EXCHANGE` is `Finish` (it already
did). The `Apply` path drives the sequence the retired
`previous-restore-recovery-identity.md` built and proved in-process —
`prepare_previous_restore_recovery` -> `adopt_previous_archive_attempt` ->
`restore_previous_with_journal` — and `adopt_previous_archive_attempt` finally
has the `pub(crate)` production caller its doc comment named.

**The namespace decides, not the syscall.** A move that reports failure but
left the predecessor in staging is `Applied`; one that reports success while the
namespace disagrees is `Ambiguous` and returns no retry capability.

**One accepted limitation, stated rather than hidden.** The `Finish` path
completes the journal with `AlreadySatisfied` and does *not* run the
parent-sync suffix a completed move normally runs: that suffix needs the
retained archive attempt, which died with the identity that made the move, and
adoption needs the archived slot this layout no longer has. It is covered by the
next action in the chain — the reverse exchange's durability boundary syncs the
staging parent and the installation root before it persists. If that ordering
ever changes, this becomes a real gap.

**Not yet proven on a guest, and not yet reachable.** The 41-second
`install -> remove -> activate` cell is the acceptance test and cannot run
against this code until the lock seam above is closed; §A4's reboot matrix is
still outstanding after that. An empty pinned entry is not a measurement.

### The gap, as it was found (kept for the method)

`PreviousRestoreIntent` is fully supported by the journal: `next_rollback_phase`
routes into it, `rollback_successor` records its outcome, `policy.rs` knows its
two legal namespace layouts, and `usr_rollback_resume_route.rs` names the phase
explicitly as a successor it will persist. `crate::transition_identity` even has
the physical half — `restore_previous_with_journal`,
`finish_applied_previous_restore_with_journal`, and a
`PreviousRestoreRecoverySeal` whose own doc comment says it "must only ever be
created by [the `PreviousRestore` rollback dispatcher]".

**That dispatcher was never written.** There is no
`usr_rollback_previous_restore_authority.rs`, no dispatch, no persistence
boundary; the seal's only callers in the whole workspace are tests. So the
record advances `RollbackDecided -> PreviousRestoreIntent` and stalls there
forever, and every tail gate's `previous_archive == NotRequired` was consistent
with that — it encoded a world where the restore never happens.

The remaining work is the client trio, modelled on the reverse trio
(437 + 87 + 402 + 52 lines): authority with its namespace proof and effect
lease, consuming dispatcher that mints `PreviousRestoreRecoverySeal`, and the
`PreviousRestoredToStaging` persistence boundary. It slots into `startup_gate.rs`
**before** the reverse step, since the restore precedes the reverse exchange in
the chain.

The physical sequence is already built and proven in-process; the dispatcher
only has to drive it under a sealed admission:

    StatefulTreeIdentity::prepare_previous_restore_recovery(
        installation, state_db, candidate_state, previous_state, &seal)?
        .adopt_previous_archive_attempt(installation, previous_state, &recorded_slot)?
        .restore_previous_with_journal(installation, previous_state, &seal)?

`adopt_previous_archive_attempt` is `pub(super)` today and needs widening to
`pub(crate)`; its `_for_test` twin already is. The recorded slot comes from
`record.previous_archive_slot`, which the presence invariant guarantees is
`Some` at exactly these phases — that is what D-PR1 was for.

The two admission typestates fall out of `policy.rs`, which already names the
two legal layouts for this phase: `PREVIOUS_ARCHIVED` (the predecessor is still
in its slot) is Apply, and `POST_EXCHANGE` (it is already back in staging) is
Finish, completing with `AlreadySatisfied` through
`finish_applied_previous_restore_with_journal`. Same shape as the reverse
gate's POST/PRE split.

Acceptance test, already established and needing no crash injection: the 41-second
`install -> remove -> activate` cell on a real guest. Then §A4's reboot matrix,
which `previous-restore-recovery-identity.md` left outstanding for exactly this
reason — in-process fixtures cannot prove cross-reboot behaviour.

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

### Started 2026-07-31 (`abc19913`)

`apply_stateful_blit` is deleted. It had **zero callers** — one occurrence in
the whole workspace, its own definition — and its body was already just an
`Err(FixedStagingCapabilityRequired)` stub. Nothing depended on it, so the
"34 blocking call sites" count below was slightly pessimistic: that entry point
was not blocking anything.

The rest of the estimate holds. The remaining 34 sites reach the legacy route
through `apply_stateful_blit_with_checkpoint` / `_with_capability`:

| file | sites |
|---|---|
| `tests/stateful_journal_and_identity_preflight.rs` | 8 |
| `tests/stateful_activation_recovery.rs` | 7 |
| `tests/stateful_quarantine_recovery.rs` | 7 |
| `tests/stateful_previous_tree_recovery.rs` | 5 |
| `tests/root_abi_preflight.rs` | 4 |
| `active_reblit_tests.rs` | 2 |
| `tests/stateful_candidate_metadata.rs` | 1 |

**That search is done — there are no more free deletions.** Nothing collapses
without the test ports first, so the remaining job is the real one below, not a
smaller one hiding inside it.

**Re-measured 2026-08-02, and §C had drifted — trust these numbers, not the
2026-07-31 ones:**

| symbol | then | now |
|---|---|---|
| `commit_stateful_staging` | 8 | 9 refs / 7 files |
| `require_no_journal` | 19 | 21 refs / 8 files |
| `quarantine_candidate` | 1 | 2 refs / 2 files |
| `apply_stateful_blit_with_capability` | 1 | 2 refs / 1 file |

The 34 call sites and their per-file split are unchanged and still exact.

**`candidate_quarantine.rs` no longer exists** — the deletion list below names a
file that is already gone, so re-check each target before removing it rather
than working from the list. `stateful_recovery.rs` and `legacy_boot_repair.rs`
are both still present.

### The blocker: two test helpers

| helper | trivial `\|_\| Ok(())` | fault-injecting |
|---|---|---|
| `active_reblit_tests::run` | 19 | 7 |
| `stateful_candidate_metadata::apply_fresh_candidate` | 6 | 2 |

**Re-pointing is not a signature swap**, even for the trivial 25: the legacy
helpers take a `vfs` tree and blit it, while the coordinated entries take an
already-materialized `fixed_staging::StatefulCandidate`. Every site needs a
materialization step.

**Measured 2026-08-02, and the gap is wider than that.** Compare the two entry
points directly:

    // legacy — one call does everything
    apply_stateful_blit_with_checkpoint(fstree: vfs::Tree<PendingFile>,
        state: &State, old_state: Option<state::Id>,
        system_snapshot: SystemModel, checkpoint: F)

    // coordinated
    execute_new_state_forward(identity: StatefulTreeIdentity,
        authority: JournalUsrExchangeAuthority, database: &db::state::Database,
        previous: NewStatePrevious, selections: &[Selection], summary: &str,
        run_boot_sync: bool, derive_metadata, transaction_trigger, system_trigger)

A ported site does not just materialize a candidate — it has to construct a
`StatefulTreeIdentity`, obtain a `JournalUsrExchangeAuthority`, and supply the
three trigger closures the coordinator drives. That is fixture-building, not
call-site rewriting, and it is the same for all 34 sites rather than only the
9 fault-injecting ones.

**So the E:M estimate on this section is wrong.** Budget it as its own piece of
work with a shared test harness built first — one helper that turns a
`StatefulTransitionFixture` into `(identity, authority, previous)` — and port
against that, or the 34 sites become 34 hand-rolled coordinator setups.

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
- **The full-suite-only failure cluster is still unexplained, and it grew.**
  `receipt_promotion::completion` (`Timeout`) has been joined by
  `boot_sync_complete_startup_storage_faults` (`AcquireLock WouldBlock` on
  reopen) and `boot_asset_snapshots::failed_batch_drops_prior_snapshots`
  (`EBADF`). All pass in isolation; all appear only under full-suite
  parallelism, and more of them appear when the machine is *less* loaded and
  more tests run concurrently. That points at shared-resource contention —
  descriptors and the journal flock — not at a durability defect. It is not
  diagnosed, and calling it a flake is not a diagnosis.
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
- **An admission predicate that names an operation is usually standing in for a
  phase comparison it should be deriving.** Found five times in two days:
  `operation == NewState` for pre-exchange sources, `!= ActivateArchived` for
  external effects (×3 copies), `NewState ⇒ fresh_db == Pending`. Every one
  encoded "the crash happened late" and stranded a rollback that happened early.
  Treat any surviving `match record.operation` inside an admission gate as
  suspect until shown otherwise.
- **Checking one step of a chain proves the chain starts, not that it ends.**
  The rollback-chain test verified only the first successor and would have
  certified a chain that dies at step three. Walk to the terminal phase.
- **A fixture that silently fails validation removes a case from coverage
  without removing it from the list.** Every post-exchange test record at
  `PreviousArchiveIntent` or later, and every one at `BootSyncStarted`, was
  rejected for a missing `previous_archive_slot` / `boot_publication_receipts`
  — the exact phases where a rollback has the most to undo. Separating
  "unbuildable" from "refused" in the assertion is what made it visible.
- **An empty pinned list means the gates agree, not that the path works.** Every
  pre-exchange list was empty while `PreviousRestoreIntent` had no implementation
  at all. Before trusting one, ask whether anything in the walk actually calls
  production code for that phase, or whether a `_ => true` arm waves it through.
- **A test that asserts "nothing happened on the next entry" may be asserting a
  stall.** Two ActiveReblit tests froze the boot-repair finalization refusal as
  if it were idempotence, one of them in its own name. The check that tells them
  apart is `recovery_disposition`: if it names an action, a gate refusing that
  action is a bug, not a boundary.
- **Fixtures that hold a pending diagnostic hold the journal lock and the state
  database.** A following entry that only reads is fine; one that finalizes
  blocks until the deadline and looks exactly like a hang. Drop the previous
  entry's result — and the clean startup itself — before asserting against the
  same root.
- **When a gate refuses, suspect the evidence before the gate.** Three times
  this epic a fixture fabricated a value production derives — two generations,
  one archive parking name — and each time the first instinct was to work out
  what production should do differently. The gate was right every time. Ask
  "could a real run produce this record?" before touching the predicate.
- **"This case is unreachable" is a claim, not a comment.** Writing it as an
  `expect` made a recovery identity panic the moment a test took the legacy
  entry point it supposedly could not reach. If a case is genuinely structural,
  make the type forbid it; if it is merely believed, return an error and find
  out.
- **A plan that records a constraint does not enforce it.**
  `previous-restore-recovery-identity.md` wrote "journal ← already held by the
  recovery dispatcher; must **not** re-acquire", and the identity layer was
  then built to open its own. Nothing caught it for a week, because the only
  callers were tests that hold no lock. A constraint about *who holds what* has
  to be a type or a test, not a bullet point.
- **A catch-all arm in an invariant walk is an exemption list you cannot see.**
  Four terminal gates kept every hard-coded rule the shared tail had shed,
  purely because `_ => true` covered them. Replacing it with a real dispatch
  found a bug in the first minute. Enumerate the arm; do not default it.
- **Tests that assert admission cannot catch over-widening.** Loosening a gate
  never strands a chain, so every "nothing is stranded" test stays green through
  it. All four over-widenings committed during this work were caught by older
  tests asserting a plan is *refused*. When touching an admission predicate, run
  the exclusion suites — and when adding one, add its refusal twin.
- **Before deleting a hard-coded table, name every fact it encoded.** The
  `(operation, phase, source, generation)` tables were mostly a stale copy of
  the forward chain, but the generation column was load-bearing and nothing
  else checked it. Deriving the structural part and dropping the rest silently
  removed a real constraint; three exclusion tests caught it on the next full
  run. Replace each fact, then delete.

And the method lesson that broke two deadlocks after repeated guessing failed:
**measure the value, do not derive it from assumed arithmetic.** Instrument and
print, then fix.

---

## Loose ends

`develop` is ~139 commits ahead of `origin/develop` and unpushed.

**The tree is not `cargo fmt`-clean.** `rustfmt.toml` sets `max_width = 120`,
but a good deal of committed code is wrapped at the default 100 — including
files nothing in this epic touched, such as `fixed_staging.rs`. Running
`rustfmt` over a file therefore reflows unrelated lines, and today's commits
carry some of that churn in `transition_identity`. Either run `cargo fmt` once
across the crate as its own commit, or stop formatting whole files by hand;
doing neither means every future diff mixes real changes with reflow.
