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

**Correction 2026-08-02 (second pass): `candidate_quarantine.rs` *does* exist**,
at `transition_identity/candidate_quarantine.rs` (9.5K). The earlier note here
claiming it was already gone was wrong — it searched the wrong directory.
`stateful_recovery.rs` and `legacy_boot_repair.rs` are also present. Re-check
each deletion target against the tree; do not work from the list alone.

**Quarantine does not die with the legacy route.** Only the
`quarantine_candidate` *helper* is legacy-only (one non-test caller,
`stateful_recovery.rs:370`, gated behind `require_no_journal()`). The quarantine
**directory** is live coordinated production code —
`startup_reconciliation.rs:909` and
`startup_reconciliation/activation_namespace/capture/mod.rs:245` both write to
it. So `tests/stateful_quarantine_recovery.rs` (7 sites) cannot be blanket-
deleted as "legacy-only"; each test needs checking against the coordinated
capture path before it is dropped or ported.

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

**But the harness does not need building — it already exists.**
`transition_identity/journal_coordinator/tests/mod.rs` has `CoordinatorFixture`
plus `fixture()`, `fixture_with_exchange_authority()`,
`fixture_with_exchange_authority_and_previous_slot()`, `fixture_parts()` and
`fixture_parts_with_root_abi_mask()` — exactly the
`(identity, authority, previous)` construction each port needs, already used by
`new_state_forward.rs`, `usr_exchange_effect.rs` and
`root_abi_publication_persistence.rs`.

So the real first step is **visibility, not construction**: those helpers are
private to `journal_coordinator::tests`, and the 34 sites live in
`client/tests/`. Either lift them into a shared test-support module both trees
can reach, or `#[path]`-include them the way the existing suites already share
`startup_recovery/test_support.rs`.

**Corrected 2026-08-02.** `stateful_journal_and_identity_preflight.rs` was
called the natural pilot because it "already references
`JournalUsrExchangeAuthority`". It does not — it references
`JournalUsrExchangeAuthorityError`, asserting on error *types*. That was a grep
match read as a fact, and it is not a reason to pick that file.

### Pilot: `stateful_candidate_metadata.rs` — DONE 2026-08-02

Picked instead because its 9 tests funnel through **one** legacy call site
(`apply_fresh_candidate`), so porting the helper ports the whole file. Landed as
8 coordinated tests in
`journal_coordinator/tests/candidate_metadata_escapes.rs`:

| legacy test | coordinated port | refusal observed |
|---|---|---|
| `never_follows_lib_or_os_info_symlinks` | same | `UnsafeDirectory`/`UnsafeInput` (symlink) |
| `never_follows_output_symlinks` | same | `DestinationExists` (symlink) |
| `preserves_existing_output_inodes` | `never_replaces_existing_output_inodes` | `DestinationExists` (regular-file) |
| `final_name_races_are_no_replace` | same | `PublicationCollision` (EEXIST) |
| `rejects_post_trigger_mutation` (4 shapes) | `rejects_every_post_trigger_mutation` (**5**) | `FileChanged` / `UnexpectedHardlink` |
| `rejects_post_system_trigger_mutation` | same | `FileChanged` at `SystemTriggersStarted` |
| `candidate_usr_substitution_before_metadata` | `candidate_substitution_before_metadata_decorates_neither_tree` | `TreeMarker(DirectoryChanged)` |
| `candidate_usr_clone_failure_precedes_decoration` | same | fault consumed, no `lib` created |
| `successful_stateful_metadata_is_sealed_*` | `published_candidate_metadata_is_sealed` | mode/uid/nlink asserted |

Three things the port established that reading alone would not have:

1. **Every refusal is asserted by variant, not just by stage.** The first draft
   asserted only "it failed at metadata publication" — which the wrong failure
   would also satisfy. Printing the actual errors showed all five shapes were
   discriminated correctly, and the assertions now pin that.
2. **The `replace` shape was being caught by the wrong guard.** With
   `fs::write` the replacement landed at mode 0o664 and tripped `UnsafeMode` —
   the identity check never ran. Made canonical, it fell through to
   `UnexpectedHardlink`. A fifth **`substitute`** shape was added (fresh file,
   canonical mode, single link) so the pure-identity path is exercised; it is
   caught by `FileChanged`. That shape had no legacy ancestor.
3. **System triggers run after the exchange** — confirmed observationally, not
   from the phase table: a write to the *live* path was refused naming the
   *staging* path, which is only possible if they are one inode by then.

`decorate_stateful` has exactly one non-test caller
(`core/stateful_transition.rs:126`), so it dies with the legacy route and the
two tests that drive it directly had to be ported too, not left behind.

**The ninth test is not in that table and is not yet ported.**
`owned_metadata_proof_outlives_source_identity_and_rejects_named_substitution`
makes two claims. The substitution half is covered — by the new `substitute`
shape and by the existing `park_and_replace_metadata` tests in
`metadata_proof.rs`. The other half is that the proof stays valid after its
**source identity is dropped** (`drop(identity); proof.revalidate()`), which no
coordinated test asserts, because the coordinator owns the proof for its whole
lifetime and never exposes that seam. Judged structurally guaranteed rather than
demonstrated — that judgement is untested and should be revisited before the
file is deleted, not treated as settled.

Remaining order: port the other 26 sites across 6 files, then the deletions.

### Next target sized 2026-08-02: `active_reblit_tests.rs`

Highest leverage of what is left — 2 call sites, but 26 tests funnel through one
`run` helper, the same shape that made this pilot cheap. **It is probably mostly
deletion, not porting.** Its assertions split by quarantine-name prefix:

- `replaced-active-reblit-wrapper-` is **live coordinated production code**
  (`activation_namespace/capture/active_reblit_candidate_preserve.rs:366`,
  `active_reblit_commit_cleanup.rs:387`,
  `transition_identity/active_reblit_replacement_recovery.rs:29`) and is already
  asserted in ~20 files including six `usr_rollback_active_reblit` suites. That
  coverage exists; it does not need porting.
- `failed-active-reblit-` appears **nowhere outside `active_reblit_tests.rs`**.
  It is the legacy route's own failure disposition and dies with it.

So size that file per test against the existing coordinated suites before
porting anything — the pilot's lesson is that the expensive assumption is
"this needs porting" when the coverage is already somewhere else.

**Started 2026-08-02.** Driver landed
(`journal_coordinator/tests/active_reblit_forward.rs`) with three variants —
plain, `_with_transaction`, `_with_system` — plus **6 of 25** legacy tests
ported, all stressed 12–30× for flakiness before commit:

| # | legacy test | coordinated stage of refusal |
|---|---|---|
| 1 | `rotates_the_whole_old_wrapper_and_leaves_exact_empty_staging` | success + `complete_active_reblit_without_boot` |
| 2 | `refuses_missing_or_malformed_live_state_id_*` | `/usr exchange`, `LiveActiveStateProof` |
| 3 | `rejects_same_inode_state_id_rewrite_before_exchange` | `transaction triggers`, `PostEffectEvidence` |
| 4 | `rejects_same_content_new_state_id_inode` | same as #3 (folded into one test) |
| 5 | `exchange_preflight_rejects_last_moment_state_id_replacement` | `/usr exchange`, **`outcome: NotApplied`** |
| 6 | `system_boundary_corruption_reverses_and_preserves_bad_candidate` | `system triggers`, `PostEffectEvidence` |

Plus a clean-run record proof (g10) with no legacy ancestor.

**#6 changed meaning and the test says so.** Legacy self-reversed the exchange
and reported `StatefulTransitionUsrRestored`. The coordinated forward prefix
does not self-reverse — it refuses to record `SystemTriggersComplete` and parks
the journal for recovery. Both routes agree a corrupted live tree never becomes
a completed transition; only the disposition moved.

**Remaining 19, and what each needs:**

- **#7** `pre_boot_checkpoint_state_id_mutation_is_rejected_before_boot` — needs
  `run_boot_sync: true` and `into_active_reblit_boot_sync_handoff`; the current
  drivers all pass `false`.
- **#8–#15** staging-wrapper faults, scan, exhaustion, and two-reblits-one-client.
  #15 needs a *second* identity+authority against the same installation, which
  `fixture_with_exchange_authority` cannot produce — it builds a fresh
  installation each call. Needs a re-acquire helper over `fixture.installation`.
- **#16–#25** previous-slot parking.

### Duplicate check against `active_reblit_reservation.rs` — done 2026-08-02

I claimed four of the eight reservation tests "look like direct counterparts"
to legacy #16, #11, #13 and #10. **Checked, and exactly one is.** The general
error: the reservation tests operate on the *reservation unit*
(`reserve_for_transaction_triggers`), while the legacy tests drive the *whole
transition* and assert end-state namespace properties. Same vocabulary,
different scope.

| legacy | coordinated | verdict |
|---|---|---|
| #13 `staging_wrapper_name_exhaustion_*` | `preserves_foreign_name_exhaustion` | **DUPLICATE — delete.** The coordinated test is a strict superset: it exhausts all 256 names for *both* the replacement wrapper and the parked slot, asserts `NotApplied` for each, and byte-checks every occupant. Legacy only does the wrapper half. Its one extra assertion is `failed_usr_quarantines(...) == 1`, and `failed-active-reblit-` is the legacy route's own disposition, which dies with it. |
| #10 `queued_applied_suffix_faults_never_exchange_the_wrapper_twice` | `reports_applied_slot_after_durable_replacement` | **NOT a duplicate.** Both use the doubled-fault pattern, but on different subjects: coordinated doubles `SlotFaultPoint::RootsPostSync` (previous-slot parking), legacy doubles `OriginalPostSync`/`FinalRevalidation` (staging-wrapper rotation). The wrapper's never-apply-twice property is uncovered. |
| #11 `staging_wrapper_substitution_is_ambiguous_and_never_retried` | `reports_ambiguous_replacement_stage` | **NOT a duplicate.** Legacy substitutes the whole **staging directory**; coordinated renames the **replacement path**. Different injection subject, different error (`outcome: "ambiguous"` at commit cleanup vs `EvidenceSandwich`). |
| #16 `preserves_authorized_two_link_previous_marker_pair` | `handles_one_link_and_parks_two_link_previous` | **NOT a duplicate — half of it is.** The parking assertions overlap. But legacy then runs a *second, NewState* transition and asserts the parked slot's marker inode and `nlink == 2` survive it. That cross-transition durability claim has no coordinated equivalent. |

Net from that pass: 1 deletion, 3 still need porting. The lesson is the same one
the pilot taught — "looks like a counterpart" from a name match is not evidence,
and the error has consistently run toward assuming coverage exists.

### Then #10 and #11 turned out to be deletions anyway — for a better reason

Attempting to port them, all three injected faults **succeeded**, which read as
"the coordinated route is more robust." It is not. Adding
`staging_wrapper_rotation_faults_remaining()` (new, `fault_injection.rs`) showed
`unconsumed=3/3` and the before-exchange hook never firing: **the code path is
not taken at all.** A clean run under injected faults means nothing until you
prove the fault fired — the same trap as the `/tmp`-on-tmpfs and swallowed-`$DRV`
episodes earlier in this epic.

Confirmed structurally: `rotate_active_reblit_staging` has **exactly one
caller**, `client/core/stateful_transition.rs:531` — the legacy route. So
`staging_wrapper_rotation/legacy_lifecycle.rs::rotate` and its fault points
(`OriginalPostSync`, `FinalRevalidation`, `BeforeExchange`, `AfterExchange`,
the `*PreSync`/`*PostSync` family) are legacy-only and die with the route.

The split inside `staging_wrapper_rotation` is therefore:

| shared with the coordinated route | legacy-only |
|---|---|
| `reserve_with_journal`, `finish_preparation_with_journal` — reaching `ReplacementPreparationSync`, `FinalPreparationRevalidation` | `rotate` — the whole exchange, and `before_exchange()` |

**Revised verdict for the staging-wrapper tests (#8–#11): delete, do not port.**
Their preparation-point coverage already exists coordinated
(`retries_one_durability_unproven_fault`,
`reports_durable_final_checkpoint_failure`); their exchange-point coverage tests
code being deleted. #8 spans both halves — only its preparation points matter,
and those are covered.

A boundary regression guard is now in
`active_reblit_forward.rs::coordinated_active_reblit_never_reaches_the_legacy_rotation_exchange`;
it arms three exchange faults and asserts all three stay unconsumed. Delete it
together with `legacy_lifecycle::rotate`.

**#16–#25 are a different story — measured, and the opposite result.** Added
`active_previous_slot_parking_faults_remaining()` and probed all nine
`SlotFaultPoint`s through `fixture_with_exchange_authority_and_previous_slot()`
+ forward + completion:

    MarkerPreSync WrapperPreSync RootsPreSync BeforeRename AfterRename
    MarkerPostSync WrapperPostSync RootsPostSync FinalRevalidation
    → all nine  fired=true  =>  completed

Every point is reached **and** the transition resumes through it. That is the
real "resumed without a second move" property, and unlike the wrapper case the
green run means something. **#17 ported** as
`coordinated_active_reblit_resumes_every_previous_slot_parking_fault_without_a_second_move`,
asserting `remaining == 0` per point before anything else, then the structural
once-only claim: canonical gone, exactly one parked slot, exactly one entry in
it, and that entry is the original marker inode.

The contrast is the whole lesson of this section:

| | faults fire? | clean run means |
|---|---|---|
| staging-wrapper rotation | **no** — 3/3 unconsumed | the path was never taken → delete the tests |
| previous-slot parking | **yes** — 9/9 consumed | the path resumed correctly → port the tests |

Same surface symptom (transition succeeds under injected faults), opposite
conclusions. Only the consumed-count told them apart.

Note `installation.root_path(x)` resolves under `.cast/root`, **not** under the
live root — a scan of `installation.root` for parked slots finds nothing.

**#18 and #20 are duplicates too**, checked against the same file:
`preserves_foreign_name_exhaustion`'s *second block* fills all 256
`active_reblit_parked_slot_path` names, asserts `NotApplied`, `canonical.is_dir()`
and byte-preservation — that is exactly #18. And
`reports_applied_slot_after_durable_replacement` arms
`[RootsPostSync, RootsPostSync]` and asserts the move stays Applied with the
canonical gone — exactly #20.

**#19 needed porting and is done** (`..._slot_scan_skips_every_foreign_occupant_kind`).
Only the wrong-mode directory kind was covered coordinated
(`keeps_wrong_wrapper_mode_untouched`); the regular-file, dangling-symlink and
FIFO kinds were not. Two things it turned up:

- The tree token cannot be recovered by re-adopting the live marker in the
  two-link fixture — `adopt_or_create_before_journal` refuses a marker at
  `links=2` (`UnsafeMarker`). Read it back off the slot name the fixture
  planted instead.
- With strangers in the indexed namespace the parking still lands correctly at
  the first free index (4) carrying the original marker inode, but **commit
  cleanup defers** (`CommitCleanupDeferred`) rather than finishing. Legacy
  completed inline. Same fail-closed shift as the other dispositions; the test
  asserts the deferral rather than pretending it completes.

### Running tally for the 25

- **Ported (18 legacy → 18 coordinated tests):** #1–#7, #12, #14–#17, #19,
  #21–#25, plus a clean-run record proof and the rotation-boundary guard.
- **Confirmed deletions (7):** #8–#11 (legacy rotation exchange), #13, #18, #20
  (superseded by the reservation suite).
- **Remaining: none.** For the record, the two that looked blocked:

| # | legacy test | what it needs |
|---|---|---|
| ~~7~~ | ~~`pre_boot_checkpoint_state_id_mutation_is_rejected_before_boot`~~ | **DONE** — see below |
| ~~15~~ | ~~`two_successful_active_reblits_on_one_client_use_distinct_wrapper_slots`~~ | **DONE** |
| ~~16~~ | ~~`preserves_authorized_two_link_previous_marker_pair`~~ | **DONE** |

### #15 and #16: two re-acquire helpers closed both

The blocker was real but small — both needed a *second* transition over an
installation that had already run one, which `fixture_with_exchange_authority*`
cannot give (fresh installation per call, staging candidate consumed by the
first run). Two helpers cover it:

- `reacquire_active_reblit(&fixture)` — re-stages `staging/usr`, re-acquires the
  pre-journal authority, calls `prepare_active_reblit_identity`.
- `reacquire_new_state(&fixture)` — same, with the `payload-sentinel` the
  NewState fixture writes, via `prepare_unallocated_candidate`.

**#15** then asserts what the indexed wrapper naming is actually for: two
successive reblits land in *distinct* slots, and each wrapper holds the tree
that was live when its own transition started. A second run reusing index 0
would overwrite the first transition's preserved tree — the only copy of it.

**#16**'s parking half is asserted only as a precondition (it duplicates
`handles_one_link_and_parks_two_link_previous`). The unique claim is
cross-transition: the parked marker is a two-link pair shared with the tree it
names, and an unrelated later NewState transition must leave both links intact.
A successor that unlinked or re-created it would silently break the parked
slot's binding to its tree, and no single-transition test would notice.

**`active_reblit_tests.rs` is fully accounted for: 18 ported, 7 deletions, 0
remaining.**

## Next file: `tests/root_abi_preflight.rs` (4 sites / 4 tests)

**All 4 ported.** `every_live_root_abi_conflict_precedes_candidate_trigger_and_exchange_mutation`
→ `coordinated_root_abi_conflicts_are_refused_before_any_authority_is_taken`.

**The coordinated bound is tighter than the legacy one.** Legacy asserts the
conflict precedes candidate triggers and exchange mutation. Coordinated refuses
at `acquire_prejournal_for_test` — before a journal exists, before any trigger,
before the candidate is prepared. There is no transition to unwind because none
was created. Same two error shapes as legacy: `RootAbiLinkTypeConflict` for the
canonical name, `RootAbiStagingConflict` for the `.next` staging name.

**A trap worth recording, because it nearly produced a false finding.** Planting
the foreign entry *after* the fixture returns does not test root-ABI handling at
all. Creating an entry in the installation root changes the root directory's own
metadata, and the retained active-state lease revalidates that — so the run
fails with:

    LiveActiveStateProof { operation: "revalidate live active-state snapshot",
      error: "installation-root metadata changed during retained active-state lease" }

at the `/usr exchange` stage, *after* the transaction trigger has run. Read
truncated, that looks exactly like "the coordinated route catches root-ABI
conflicts late, after triggers" — a wrong claim about production behaviour. The
entry has to be planted before the lease is taken;
`prejournal_authority_over_root_entry()` in `tests/mod.rs` does that.

### Sizing the remaining 3 — done, and the guesses were wrong again

**My guessed counterpart for #2/#3 was wrong.**
`root_links_complete_retained_namespace_binding_races_fail_stop` mutates the
*journal's own namespace bindings* — `root`, `.cast`, `journal`,
`state-transition.lock` — and fires at `publish_root_abi`, **post-exchange**.
Legacy #2/#3 mutate a **root-ABI link entry** (`bin`) at the **pre-exchange**
boundary against a retained present/absent expectation. Different subject,
different side of the exchange. The EEXIST-at-publisher-index tests are
publication-time too, so they are not counterparts either.

**Where the pre-exchange check lives.** `live_root_abi.revalidate()` appears
only in `client/core/stateful_transition.rs` (lines 147, 281, 384 — the legacy
route) plus `ephemeral_transition.rs` and `external_materialization.rs`. The
coordinated route's root-ABI evidence is `require_published_root_abi_sandwich`,
at publication.

**What the coordinated route actually does with this mutation** — measured, by
rewriting `bin` inside `arm_before_retained_exchange_rename`:

    stage "/usr exchange", outcome: NotApplied
    RetainedExchangeCoordinatorEvidence(UsrExchangeAuthority(Client(
      LiveActiveStateProof { "revalidate live active-state snapshot",
        "installation-root metadata changed during retained active-state lease" })))
    foreign entry preserved verbatim

So the **safety property #2/#3 assert does hold**: the exchange is not applied,
the live tree is untouched, the foreign entry is preserved. But it is enforced
by the *root-lease* guard — any mutation of the installation root during the
lease — rather than by a root-ABI-specific comparison. That guard strictly
subsumes the root-ABI case, since replacing a root-ABI entry necessarily
changes root metadata.

**The caveat that blocked #2 was my own measurement error, now corrected.** I
recorded that `install_root_abi_subset` with mask `0x1F` "did not create `bin`"
because `existed_before=false`. It did create it. `bin` is a symlink to
`usr/bin`, the fixture has no `usr/bin`, so it is a **dangling** symlink — and
`Path::exists()` follows symlinks and reports false. Presence has to be checked
with `symlink_metadata`. The `present` arm had been valid all along; the probe's
readout was wrong, not the fixture.

Also note the mask is asserted to be exactly five bits — `0xFF` panics in
`root_abi_publication_support.rs:20`.

**#2 and #3 are both ported, as one test** —
`coordinated_root_abi_mutation_at_the_exchange_boundary_fails_closed`, over
`retained_present ∈ {true, false}`. Both arms give the same result:

    stage "/usr exchange", NotApplied
    "installation-root metadata changed during retained active-state lease"
    live usr inode unchanged, foreign entry preserved verbatim

It is written as a **lease proof, not a root-ABI proof**, because that is what
actually holds — the coordinated route has no root-ABI-specific pre-exchange
comparison, and claiming one would document a guard that does not exist. The
lease covers the whole installation root, which strictly subsumes the root-ABI
case: a root-ABI entry cannot be replaced without changing root metadata. The
fixture asserts `retained_present` via `symlink_metadata` so the two arms cannot
silently collapse into one again.

### #4 sized and ported — the guessed counterpart was close but not equal

`rejects_foreign_eexist_at_every_publisher_index_without_replacement` was the
right neighbourhood and still not the same test. It races a **symlink**, which
the no-replace link syscall reports as EEXIST. Legacy #4 races a **regular
file** — the name is occupied by something that is not a link at all, so the
conflict is one of *type*, not of existence, and legacy reports
`RootAbiLinkTypeConflict`.

Checked every publication race in the coordinated suite: all plant a symlink
(the journal-namespace races plant a directory, but at a different seam).
**Nothing raced a non-symlink at a publisher index.** That was #4's unique
coverage, now ported as
`journal_coordinator_root_links_complete_rejects_a_regular_file_at_every_publisher_index`
— all five indices, asserting the file is preserved by inode and never replaced
by a link, and that the namespace outside the root-ABI names is untouched.

**The "reverses usr" half is deliberately not ported.** Legacy reversed the
exchange inline and reported `StatefulTransitionUsrRestored`. The coordinated
route leaves the record at `UsrExchanged` and hands reversal to recovery; the
test asserts that instead via `assert_usr_exchanged_source`. Porting the legacy
error shape would have asserted a disposition the coordinated route does not
have.

**`tests/root_abi_preflight.rs` is fully accounted for: 4 ported, 0 deletions,
0 remaining.**

Pattern across both files sized so far: name-level mapping has now been wrong
**four** times — reservation pass (1 of 4 real), staging wrapper (verdict
reversed entirely), root-ABI #2/#3 (wrong side of the exchange), root-ABI #4
(wrong conflict kind). It has not once been right without checking. Treat a
matching name as a place to start reading, never as a verdict.

## The remaining scope, measured exactly

The four remaining files hold **58 tests but only 28 that touch the legacy
route**. The rest already run against other entry points and are unaffected by
the deletion. Only these 28 need a port-or-delete decision:

| file | tests using the legacy route |
|---|---|
| `stateful_journal_and_identity_preflight.rs` (9) | `unresolved_journal_evidence_blocks_marker_publication_before_activation`, `orphan_transition_row_blocks_marker_publication_before_activation`, `first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr`, `failed_first_install_can_retry_the_exact_marker_only_previous_baseline`, `duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees`, `recovery_rejects_same_content_marker_name_substitution_without_repair`, `recovery_rejects_whole_directory_same_token_substitution_without_exchange`, `missing_live_usr_between_identity_check_and_exchange_is_never_recreated`, `isolation_root_abi_conflict_fails_before_usr_exchange_and_preserves_foreign_entry` |
| `stateful_quarantine_recovery.rs` (7) | all seven |
| `stateful_activation_recovery.rs` (7) | `new_stateful_post_swap_failure_*`, `new_stateful_pre_swap_failure_*`, `previous_archive_never_replaces_a_racing_empty_destination`, `previous_restore_never_replaces_a_racing_empty_staging_destination`, `incomplete_fresh_reverse_retains_live_candidate_record_and_reopens`, `incomplete_previous_restore_retains_live_fresh_candidate_record_and_reopens`, `two_failed_active_state_reblits_use_unique_non_state_quarantines` |
| `stateful_previous_tree_recovery.rs` (5 of 26) | `applied_previous_archive_and_restore_faults_use_full_client_suffix_routing`, `fresh_identity_can_archive_after_a_complete_compensating_recovery`, `previous_archive_abort_retirement_faults_resume_in_production_recovery`, `retained_exchange_post_move_faults_run_the_swapped_recovery_path`, `retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange` |

`stateful_previous_tree_recovery.rs` is the surprise: 26 tests, only **5** on the
legacy route. The other 21 drive `transition_identity` primitives directly and
survive the deletion untouched. Sizing that file by its length would have
overstated it fivefold.

#### `retained_exchange_post_move_faults_run_the_swapped_recovery_path` — DELETE (verified)

Both sides read. Same seam, same fault points
(`StagingParentSync`, `InstallationRootSync`, `FinalRevalidation`), and the
coordinated version in `usr_exchange_effect.rs` is **strictly stronger** on the
physical claims: it asserts `retained_exchange_syscall_count() == 1` (the move
happened exactly once), the exact post-exchange layout, that the journal record
is unchanged, and that root links are absent. The legacy test asserts only that
an error came back.

Legacy's unique content is the *disposition* — `StatefulTransitionUsrRestored`
plus candidate quarantine-and-invalidate, i.e. inline reversal. That is legacy
behaviour by design; the coordinated route reports
`UsrExchangeEffectFailure::Exchange { outcome: Applied }` and leaves reversal to
recovery. Nothing portable remains.

#### The other four — starting points, NOT verdicts

`applied_previous_archive_and_restore_faults_use_full_client_suffix_routing`
(legacy line 490) arms `arm_retained_previous_move_fault`, which
`journal_coordinator/tests/new_state_forward.rs:201` also arms — shared hook,
same-claim unverified. The remaining three
(`fresh_identity_can_archive_after_a_complete_compensating_recovery`,
`previous_archive_abort_retirement_faults_resume_in_production_recovery`,
`retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange`)
show no fault hook in their opening lines and need reading in full.

### Environment: `$TMPDIR` must be 0700

`UnsafeInitialMaterializationParent { path: "<TMPDIR>", mode: 509 }` means the
temp dir is `0o775`. `509 = 0o775`. Several tests (e.g.
`self_upgrade_hardening::ephemeral_self_upgrade_returns_a_typed_error_without_mutating_either_root`)
require a private parent and fail immediately without it — **in isolation, on a
clean tree**, so it looks exactly like a regression you just introduced.

Fix: `chmod 700 "$TMPDIR"`. This recurs whenever the nix-shell is recreated with
a permissive umask, which is easy to hit after an environment restart mid-session.

Two process rules this cost half a session to relearn:

- **Never pipe a suite run through `tail`/`grep` before reading it.** It discards
  the failure names *and* masks the exit code, so a failing run reports success.
  Write full output to a file and capture `$?` separately.
- **Never run another `cargo` command against the same `target/` while a suite
  runs** (already in memory). Concurrent runs produced a phantom extra failure
  that did not reproduce.

### `stateful_quarantine_recovery.rs` (7) — DELETE, structurally

All seven drive `apply_stateful_blit_with_checkpoint` and assert on the
quarantine it performs on failure. That quarantine is
`TreeIdentity::quarantine_candidate`, whose **first statement** is
`self.require_no_journal()?`. A coordinated identity always holds a journal, so
the coordinated route can never reach it — this is not "no counterpart happens
to exist", it is structurally unreachable.

Confirmed earlier from the other direction too: the coordinated forward route
has no quarantine-on-failure at all. It parks the journal and recovery owns the
disposition; the quarantine *directory* is written by
`startup_reconciliation`, i.e. the recovery side, which has its own coverage
(`usr_rollback_candidate_preserve_authority` and the `activation_namespace`
capture suites).

**Caveat resolved, and it was a false alarm.** `archived_repair.rs:247` calls a
*different function of the same name*:
`ArchivedStateRepairIdentity::preserve_failed_candidate`
(`archived_state_repair/preservation.rs:23`), not
`Client::preserve_failed_candidate` (`stateful_recovery.rs:336`). Only the
latter reaches the quarantine. `quarantine_candidate` has exactly **one** caller
in the whole crate: `stateful_recovery.rs:370`.

Two same-named methods on different types is exactly the shape that makes a
grep-level check look conclusive when it isn't — worth the extra minute here.

**DELETED 2026-08-04**: `crates/forge/src/client/tests/stateful_quarantine_recovery.rs`
plus its `include!` in `tests/mod.rs`.

Deleting it orphaned two test-only hooks, `arm_quarantine_fault` and
`arm_quarantine_faults` (`transition_identity/fault_injection.rs` + the re-export
in `transition_identity.rs`); both removed in the same commit. Warning count
went 48 → 50 on deletion and back to **48** after removing them, so the deletion
left nothing dangling. Checking the warning delta is the cheap way to catch that
— a deletion that silently orphans helpers reads as clean otherwise.

Notes from the later ports:

- **#14's hook is reachable.** `arm_before_quarantine_slot_reopen` fires via
  `create_private_child`, which the coordinated reservation does call. The
  ported test asserts the hook fired before asserting anything else, for the
  same reason as the parking matrix.
- **#12 and #19 are the same proof on two namespaces** — wrapper quarantine and
  previous-slot parking. Both scans step over file/symlink/FIFO/directory
  occupants and take the first free index. Only the wrong-mode directory case
  existed coordinated (`keeps_wrong_wrapper_mode_untouched`).
- **#24/#25 folded into one test** over both trigger boundaries: the parked slot
  moved back to canonical is caught as `PostEffectEvidence` at whichever
  boundary the un-move happens on.

### #7: the boot-sync driver turned out to be four lines, not a fixture merge

The estimate above ("needs `into_active_reblit_boot_sync_handoff`") was right
about the mechanism and wrong about the cost. Reading the call chain suggested a
hard blocker: staging boot sync needs a `BoundActiveReblitBlsPublicationPlan`
(topology, attempts, stones, roots), which only `RenderFixture` in
`client/boot/` builds — and that fixture is `pub(super)` to a different suite,
so using it from `journal_coordinator::tests` meant widening a fixture designed
for BLS rendering and marrying it to `CoordinatorFixture`.

**None of that was needed.** `into_active_reblit_boot_sync_handoff` runs
`require_system_trigger_same_store_evidence` in its *own* preflight, so a
mutated live `.stateID` is refused at the handoff — strictly before any boot
effect and before a plan is ever required:

    baseline  handoff = Ok("minted")
    mutated   handoff = Err(Preflight { Identity(LiveUsr {
                              "revalidate retained state ID",
                              "retained state ID inode metadata changed" }) })

The driver is `run_active_reblit_for_boot` — the existing forward with
`run_boot_sync: true`. The port asserts both arms: without the baseline, a
handoff failing for any unrelated reason would satisfy the mutated arm.

The lesson is the mirror of the `legacy_lifecycle::rotate` one. There, reading
suggested the code was reachable and it was not. Here, reading suggested it was
unreachable without heavy scaffolding and it was one call away. Both were
settled in minutes by running the cheap experiment first.

**The governing fact for the remaining 24 — measured, and it is not obvious:**
`execute_active_reblit_forward` is *not* the whole transition. At
`SystemTriggersComplete` (g10) the namespace is in a deliberately intermediate
state:

| | at `SystemTriggersComplete` | after `complete_active_reblit_without_boot` |
|---|---|---|
| old live tree | in **staging** (`staging/usr`, original inode) | inside the quarantine wrapper |
| quarantine wrapper | present but **empty** — a reserved name | holds `usr` |
| staging | contains `usr` | **empty** |

So a legacy assertion ported against the forward prefix alone fails, and the
prefix's own state must not be asserted as if it were final — it is a
half-finished namespace by design. Every remaining port needs
`.complete_active_reblit_without_boot()` (or the boot-sync handoff) driven
before the legacy end-state assertions apply.

I first read this intermediate state as "the coordinated topology is different
from the legacy one." It is not — it is the same end state reached in two
steps, and the difference was that I had only driven one of them.

`SystemTriggersCompleteCoordinator` has no `record()`; the continuations are
`complete_active_reblit_without_boot`, `into_active_reblit_boot_sync_handoff`,
`archive_previous_tree`, and the NewState pair.

**Two more behavioural differences, both found by porting rather than reading.**

1. **The wrapper name is reserved *before* validation.** The legacy route
   validated the live tree first and reserved nothing on refusal, so its tests
   assert `wrapper_quarantines(&fixture).is_empty()`. The coordinated route
   reserves the wrapper during the forward prefix and refuses later, at the
   `/usr exchange` stage — so a refused transition **does** leave a wrapper.
   The invariant that survives is that the reserved wrapper is **empty**: a
   reserved name is inert, a populated one would mean a tree was rotated out
   from under a transition that then failed. Any ported test asserting "no
   wrapper" must be rewritten this way, not deleted and not left as-is.

2. **An unlinked live `.stateID` is covered by two guards that race.**
   `validate live state-ID inode policy` sees `links=0`; `revalidate live
   active-state snapshot` sees the retained metadata change. Which reports
   first is timing-dependent — **measured 2/25 runs** taking the other branch,
   in isolation, on an otherwise idle tree. Both are `LiveActiveStateProof`
   refusals and the transition stops either way, so this is benign, but any
   test that pins the specific guard for the *missing* case is flaky by
   construction. The *malformed* case is deterministic (a rewritten `.stateID`
   is well-formed at a new inode, so only snapshot revalidation can catch it)
   and is safe to pin. 0/30 after relaxing only the missing case.

   This is a distinct phenomenon from the known
   `receipt_promotion::completion` full-suite-only cluster — it reproduces in
   isolation and now has an explanation.

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

### `stateful_activation_recovery.rs` (7) — sized 2026-08-04

Every one of the seven asserts a **legacy-only inline disposition**, not a
coordinated outcome:

| assertion | tests | status on the coordinated route |
|---|---|---|
| `Error::StatefulTransitionUsrRestored` | `new_stateful_post_swap_failure_*`, `previous_archive_never_replaces_a_racing_empty_destination` | does not exist — the route parks at its phase and recovery reverses |
| `Error::StatefulCandidatePreserved` | `new_stateful_pre_swap_failure_*` | does not exist — no inline quarantine |
| `Error::StatefulTransitionRecoveryFailed` | `incomplete_fresh_reverse_*`, `incomplete_previous_restore_*` | does not exist — legacy recovery's own failure type |
| `assert_fresh_candidate_quarantined_and_invalidated` | both `new_stateful_*_swap_failure_*` | quarantine-on-failure is legacy-only (`quarantine_candidate` is gated behind `require_no_journal`) |

So the *disposition* half of all seven dies with the route, exactly as it did in
`stateful_quarantine_recovery.rs`.

**But do not delete on that alone.** Several carry a *physical namespace* claim
that is independent of the disposition and may have no coordinated equivalent:

- `previous_archive_never_replaces_a_racing_empty_destination`
- `previous_restore_never_replaces_a_racing_empty_staging_destination`
- `two_failed_active_state_reblits_use_unique_non_state_quarantines`

"Never replaces a racing empty destination" is a no-replace rename property, not
a disposition — the same class of claim that made root-ABI #4 a real port rather
than a duplicate. The last two produced no error-marker in the scan and need
reading in full.

**Next step:** for each of those three, separate the disposition assertion
(delete) from the namespace assertion (check against
`previous_tree_move` suffix tests and `usr_rollback_candidate_preserve_authority`,
then port whatever is uncovered).

#### The three namespace claims — resolved 2026-08-04

| legacy test | outcome |
|---|---|
| `previous_archive_never_replaces_a_racing_empty_destination` | **PORTED** — `coordinated_previous_archive_never_replaces_a_racing_empty_destination` |
| `previous_restore_never_replaces_a_racing_empty_staging_destination` | **PORTED** — `coordinated_previous_restore_never_replaces_a_racing_empty_staging_destination` |
| `two_failed_active_state_reblits_use_unique_non_state_quarantines` | **DELETE** — see below |

The two racing-destination proofs are mirrors of each other: the archive moves
`live → <state>/usr`, the restore moves it back to `staging/usr`, and **both
directions** must refuse rather than replace an empty directory raced into the
destination. An empty dir is the shape most likely to look safe to overwrite.
Each asserts the occupant by inode *and* that it is still empty — a replacing
rename and an unlink-then-recreate would each pass a weaker check.

Only the no-replace half was ported. Both legacy tests also assert inline
reversal (`StatefulTransitionUsrRestored` / `StatefulTransitionRecoveryFailed`),
which the coordinated route does not do.

**The restore returns `Ambiguous`, not `NotApplied`** — with the destination
occupied it cannot distinguish "my rename never happened" from "it happened and
something recreated the source", so it declines to claim either. That is the
honest outcome; the physical assertions are what pin the guarantee. Asserting
`NotApplied` there (my first attempt) fails.

**Why the third is a deletion:** its uniqueness claim is about *failed-candidate
quarantines*, and quarantine-on-failure is legacy-only — `quarantine_candidate`
is gated behind `require_no_journal`, so no coordinated transition reaches it,
and `failed-active-reblit-` appears nowhere outside the legacy tests. The
uniqueness property that *does* survive is the wrapper-slot one on the success
path, already covered by
`coordinated_two_successive_active_reblits_use_distinct_wrapper_slots`.

**`stateful_activation_recovery.rs` is now fully accounted for: 2 ported, 5
deletions.**

### `stateful_journal_and_identity_preflight.rs` (9, 5 resolved / 4 left) — sized 2026-08-04

**CLOSED 2026-08-04.** All 9 ported or resolved and deleted (commit
`49afdd14`); the file survives with its other 8 tests. Full suite after the
deletions: **2781 passed, 0 failed** (= 2790 − 9). No helper was orphaned.

One full run before that showed a single
`coordinated_active_reblit_wrapper_scan_skips_foreign_types_and_uses_next_index`
failure — a journal-lock `WouldBlock` inside `fixture_parts`. It did not
reproduce: the test passes in isolation, the pre-deletion baseline ran clean,
and the repeat run with the deletions applied ran clean. Nondeterministic
contention, not a consequence of the deletions. It belongs with the known
full-suite-only cluster and is still unexplained — do not treat it as settled.

**Correction 2026-08-04 — do NOT delete this file.** An earlier note here said
the whole file gets deleted once the 9 resolve. That is wrong: the file holds
**17** tests, not 9, and they span three route families.

| tests | entry point | fate |
|---|---|---|
| the 9 sized below | `apply_stateful_blit_with_checkpoint` | port, then delete individually |
| 5 (`candidate_pre_journal_namespace_substitution…`, the four `first_install_rejects_*`/`first_install_marker_retry_*`) | `prepare_stateful_tree_identity` | **keep** — shared guard via a thin client helper |
| `candidate_pre_journal_legacy_hardlinked_archived_payload…`, `archived_live_root_abi_conflict…` | `activate_state{,_with_checkpoint}` | **keep** unless that API is itself legacy — verify |
| `ephemeral_root_and_isolation_root_abi_conflicts…` | `apply_ephemeral_candidate` | **keep** — ephemeral path, unrelated route |

So the 9 legacy tests get deleted one at a time as each port lands, and the file
survives with 8. This is the third time a deletion list has been wrong; the
standing rule holds — **re-verify every deletion target against the tree, and
check what else lives in a file before deleting the file.**

Scanned by asserted error type. Unlike `stateful_activation_recovery.rs`, this
file splits into **two distinct groups**, and only one of them is disposition:

**Group A — shared identity-preparation guards (3). Likely portable or already
covered; these are NOT legacy dispositions.**

| test | asserts |
|---|---|
| `unresolved_journal_evidence_blocks_marker_publication_before_activation` | `StatefulTreeIdentityPreparationFailed` |
| `orphan_transition_row_blocks_marker_publication_before_activation` | `StatefulTreeIdentityPreparationFailed` |
| `duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees` | `DuplicateTreeToken`, `StatefulTreeIdentity`, `StatefulTreeIdentityPreparationFailed` |

These fire in `StatefulTreeIdentity::prepare*`, which the **coordinated route
also calls** — `fixture_parts_with_root_abi_mask` builds its identity through
exactly these functions. So the guard is shared, not legacy. Check against the
`transition_identity` suite (158 tests) before porting; the plan's counterpart
table already points "marker/token substitution refusal" there.

**Group B — legacy dispositions (5). Delete the disposition half; check each for
a residual physical claim first, as in `stateful_activation_recovery.rs`.**

| test | disposition asserted |
|---|---|
| `failed_first_install_can_retry_the_exact_marker_only_previous_baseline` | `StatefulCandidatePreserved` |
| `missing_live_usr_between_identity_check_and_exchange_is_never_recreated` | `StatefulCandidatePreserved` |
| `isolation_root_abi_conflict_fails_before_usr_exchange_and_preserves_foreign_entry` | `StatefulCandidatePreserved` + quarantine-and-invalidate |
| `recovery_rejects_same_content_marker_name_substitution_without_repair` | `StatefulTransitionRecoveryFailed` |
| `recovery_rejects_whole_directory_same_token_substitution_without_exchange` | `StatefulTransitionRecoveryFailed` |

Two of these name a physical claim in the test name itself — **"is never
recreated"** (missing live `usr`) and **"preserves foreign entry"** (isolation
root-ABI). Those are namespace properties, not dispositions, and are the same
class that made root-ABI #4 and the two racing-destination proofs real ports.
The isolation-root-ABI one is a likely sibling of the already-ported
`coordinated_root_abi_mutation_at_the_exchange_boundary_fails_closed`, but at the
*isolation* root rather than the installation root — verify, do not assume.

**Group C — one success-path test, unclassified:**
`first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr`
asserts no error and needs reading in full. First-install is the D1.5 path that
opens this plan, so check whether the coordinated first-install work already
covers it.

#### The two `recovery_rejects_*` tests — resolved 2026-08-04

Ported into `usr_exchange_effect.rs` as **three** tests, because measuring the
coordinated route split the legacy pair's single claim into a real boundary.

New driver `reverse_exchange_intent_after_applied_exchange` takes an applied-
but-faulted exchange (`RetainedExchangeFaultPoint::FinalRevalidation`) through
pending-reverse and rollback routing to the exact `ReverseExchangeIntent`, then
hands back the fixture plus both directory identities. The existing recovery
tests all take that route to prove it *advances*; these take it to prove it
*stops*.

| substitution planted at the live tree | coordinated route |
|---|---|
| whole directory swapped, same marker frame + same `.stateID` | **refuses** |
| marker replaced by a hardlink to an outside file, same bytes | **refuses** |
| marker rewritten with identical bytes at a new inode, `nlink=1` | **accepts, reverses** |

**The third row is a deliberate difference from the legacy route, not a gap.**
The durable record identifies the previous tree by `usr_runtime_identity` — the
*directory's* `(st_dev, inode, mount_id)` (`transition_journal/model.rs:340`) —
plus `tree_token`. An inert marker rewrite changes neither, so no durable
evidence distinguishes it and the reverse exchange has what it needs to be
correct. The legacy refusal came from holding a live descriptor on the marker
across the whole transition and revalidating by inode; **crash recovery cannot
hold one**, because it starts in a fresh process after a reboot. That
strictness was not portable, and what it caught here was an inert rewrite
rather than a hazard. The hardlink row is the case that *does* matter — the
tree's identity becomes writable from outside the tree — and it is refused.

So the legacy `recovery_rejects_same_content_marker_name_substitution_without_repair`
is a **legacy-route artifact**: delete it with the other 8, and do not treat its
refusal as a requirement on the coordinated route.

Required a new non-asserting entry point,
`reverse_exchange_intent_refusal_reason`, in
`startup_recovery/forward_origin_test_support.rs`. All five existing helpers
there assert an *advance* to a hard-coded phase and cannot express a refusal.
It returns the reason rendered because `startup_gate::Error` is private to the
client facade — the same shape `prejournal_authority_over_root_entry` already
uses.

#### The two first-install tests — resolved 2026-08-04

`first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr` —
the *exchange* half was already covered by
`journal_coordinator_new_state_synthesized_empty_exchange_applies_once_and_retains_empty_previous`.
The **synthesis shape** was not, which is the "synthesizes, syncs, marks" half
of the legacy name. Added to that test: the synthesized previous is a directory
at 0o755 owned by the effective uid, containing *exactly* `.cast-tree-id`, and
its token differs from the candidate's. A synthesized tree sharing the
candidate's token would make the two indistinguishable to every later identity
check.

`failed_first_install_can_retry_the_exact_marker_only_previous_baseline` —
ported as `a_first_install_retry_adopts_the_exact_marker_only_synthesized_baseline`
in `identity_preflight_guards.rs`.

The legacy test reached its second preparation by injecting a failure before the
exchange, but the failure was only its *mechanism* for getting two preparations
in a row against one installation. On the coordinated route that mechanism does
not transfer — a failed transition leaves a durable record, so a literal retry
would have to run the whole rollback chain to clean first, and the test would be
measuring recovery rather than the claim. Preparing twice directly (via
`reacquire_new_state` after re-staging) isolates the actual claim: what the
*second* preparation does with an existing marker-only `/usr`. A fresh token
there would orphan the durable baseline the first attempt committed to.

**Generalizable point:** when a legacy test injects a failure, check whether the
failure is the claim or just the setup. Twice in this file it was only setup,
and reproducing it on the coordinated route would have cost far more and tested
something else.

#### Group A: the apparent coverage is a same-name-different-type false match

`DuplicateTreeToken` appears in `active_reblit_boot_state_roots_tests/runtime_and_identity.rs:28`
— but as **`ActiveReblitBootStateRootsError::DuplicateTreeToken`**, a different
type from the `transition_identity::Error::DuplicateTreeToken` the legacy test
asserts (`stateful_journal_and_identity_preflight.rs:508`). It is not coverage
of the same guard.

**That is the third same-name-different-type false match in this file's sizing**,
after `preserve_failed_candidate` (`Client::` vs `ArchivedStateRepairIdentity::`)
and the two `FinalRevalidation` fault-point enums. When a grep "confirms"
coverage, check the *type*, not just the identifier.

So Group A is **not** covered by that hit and is most likely three genuine
ports. The guards fire in `StatefulTreeIdentity::prepare*`, which the
coordinated route calls, so the code under test survives the route deletion —
these need coordinated tests, not deletion.

**Next step for Group A**, in order:
1. ~~`duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees`~~ —
   **PORTED 2026-08-04** as
   `duplicate_permanent_tree_tokens_block_the_exchange_and_retain_both_trees` in
   the new `journal_coordinator/tests/identity_preflight_guards.rs`. 8/8 green.

   The `fixture_parts` route was the wrong shape: it prepares the identity
   internally and unwraps, so there is no seam to plant the duplicate frame
   before preparation. The test builds the installation inline instead —
   publish the candidate's own marker, copy that frame byte-for-byte onto the
   live tree at `MARKER_MODE` (0o444), then call
   `StatefulTreeIdentity::prepare` and read the error.

   Copying an *already-published* frame is what makes both tokens equal without
   either marker looking forged, so the guard at `tree_lifecycle.rs:567` is what
   refuses — not an earlier validity check. That raise site is the only one for
   the variant, confirming this is the legacy test's guard.

   The retention half is asserted in full: both marker frames, both `.stateID`
   files, the candidate DB row, and journal absence. Existing
   `fixture_parts(Archived, Active, ...)` is the standing negative control —
   the same setup minus the planted frame, and it prepares successfully.

   New file because 8 tests from this legacy file remain and will want the same
   home; it is `include!`d from `tests/mod.rs`.
2. ~~`unresolved_journal_evidence_blocks_marker_publication_before_activation`~~ and
3. ~~`orphan_transition_row_blocks_marker_publication_before_activation`~~ —
   **PORTED 2026-08-04**, both into `identity_preflight_guards.rs`. 8/8 green
   across the whole 155-test coordinator module.

   Both guards are `require_clean_baseline` (`transition_identity.rs:1000`),
   called from `prepare_candidate` at `tree_lifecycle.rs:452-459` — *before*
   `candidate_name_authority.retain` (461) and before any marker store opens
   (466). So "before marker publication" is structural, not incidental.

   **Fourth same-name-different-type false match, avoided.** Eight distinct
   types in the tree carry an `UnresolvedJournal` or `OrphanTransitionRow`
   variant. The two coordinated hits that a name-grep surfaces
   (`root_abi_publication_persistence.rs:190`, `usr_exchange_effect.rs:912`) are
   both `client::JournalUsrExchangeAuthorityError::UnresolvedJournal` — not
   `transition_identity::Error::UnresolvedJournal`. There was no coordinated
   coverage of either guard.

   **The legacy "unresolved journal" test does not test `UnresolvedJournal`.**
   It plants undecodable bytes (`b"not-a-canonical-transition-record"`), so
   `journal.load()?` fails to decode and the refusal is `Error::Journal`. The
   legacy test never noticed because it only asserted the client's outer
   `StatefulTreeIdentityPreparationFailed` wrapper, which is satisfied by any
   inner failure. The port pins `Error::Journal` for that shape and adds
   `a_durable_pending_record_blocks_a_second_transition_from_preparing` for the
   real guard — a decodable pending record, planted by running
   `begin_transition` and dropping the coordinator (dropping it matters: a live
   coordinator still holds the canonical lock, so the second attempt would
   refuse on `WouldBlock` instead of on the record).

   **Added a negative control**,
   `a_clean_preflight_fixture_prepares_and_publishes_both_markers`. Every guard
   test asserts a refusal plus "no marker published"; without pinning that the
   bare fixture *does* prepare and *does* publish both markers, all of them
   could pass vacuously on some unrelated fixture failure. This is cheap and
   should be the default shape for any future guard port in this file.

### `stateful_previous_tree_recovery.rs` (5 of 26) — sized 2026-08-04

Verified by entry point, not by name: only 6 call sites in the file touch the
legacy route (`apply_stateful_blit_with_checkpoint` at 440, 482, 580, 891, 920
and `commit_stateful_staging` at 613), and they fall inside exactly the 5 tests
the earlier scan named. **The other 21 drive `RetainedPreviousMove` /
`RetainedExchange` primitives directly** — shared, route-independent, and
untouched by the deletion. As with the preflight file, do not delete the file.

| test | verdict |
|---|---|
| `retained_exchange_post_move_faults_run_the_swapped_recovery_path` | **DELETE — already covered** |
| `retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange` | **DELETE — already covered** (sizing corrected) |
| `previous_archive_abort_retirement_faults_resume_in_production_recovery` | **PORT (reduced)** |
| `applied_previous_archive_and_restore_faults_use_full_client_suffix_routing` | **PORT (reduced)** |
| `fresh_identity_can_archive_after_a_complete_compensating_recovery` | **PORT** |

**Already covered.** `retained_exchange_post_move_faults_run_the_swapped_recovery_path`
faults the forward exchange at `StagingParentSync` / `InstallationRootSync` /
`FinalRevalidation` and expects recovery to `UsrRestored`.
`journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored`
does the same three points across **all three** candidate kinds and drives real
startup recovery through the full chain. A strict superset; its remaining
assertions are the `StatefulCandidatePreserved` disposition.

**The "reverse-exchange gap" was a sizing error — corrected 2026-08-04, and it
is a DELETE, not a port.**

The sizing above said no coordinated test faults *during* the reverse exchange.
Attempting the port disproved it. Arming
`RetainedExchangeFaultPoint::StagingParentSync` before the reverse and entering
startup produced a **clean pass** — which would have been reported as "the
coordinated route survives reverse-exchange durability faults". The new
`retained_exchange_fault_armed()` accessor showed the truth:
`still_armed=true, syscalls=2`. The reverse rename ran and **the fault point
was never reached**.

The cause: `exchange_reverse` and `finish_applied_reverse`
(`tree_lifecycle.rs:668`, `:683`) both take
`ExchangeJournalGuard::LegacyNoJournal` — they are the *legacy* reverse. The
coordinated recovery route does not call them. Its reverse durability suffix
lives in `client/startup_recovery/usr_exchange_parent_durability/` with its own
fault type, **`UsrExchangeParentDurabilityFaultPoint`** — same variant names
(`StagingParentSync`, `InstallationRootSync`), different type. **Fifth
same-name-different-type instance.** This time it inverted a sizing verdict
rather than a coverage claim, because I searched for coverage *by fault-point
type* and found none.

The claim is already covered, exactly, by
`startup_usr_exchange_parent_durability_retry_is_idempotent_and_never_reexchanges`:
it faults the parent-sync suffix and re-enters startup three times, asserting
`retained_exchange_syscall_count() == 0` at every step — a stronger form of
"finish without a second exchange" than the legacy test's.

**Kept from the attempt:** `retained_exchange_fault_armed()`
(`transition_identity/fault_injection.rs`), now asserted in
`reverse_exchange_intent_after_applied_exchange` so the forward durability
tests prove their fault *fired* rather than merely that they stayed green. This
is the third time the armed-versus-consumed distinction has changed a verdict
(staging-wrapper rotation, previous-slot parking, and now this). **Any test
that arms a fault and then observes success must check consumption.**

**Why two are "reduced".** All four slot-retirement fault points
(`BeforeSlotRetire`, `AfterSlotRetire`, `RootsAfterSlotRetireSync`,
`FinalSlotRetirementRevalidation`) appear **only** in this file — but lines
313–348 are `retained_previous_restore_retirement_faults_resume_without_a_second_rename`,
one of the 21 **surviving** primitive tests. So deleting the legacy 5 does not
strand those points; the primitive keeps them. What the two legacy tests add is
route-level resumption — that the *dispatcher* resumes a retirement fault, not
just that the primitive can. `recovery_sealed_restore_resumes_its_durability_suffix_under_a_retained_journal`
covers dispatcher resumption for the restore suffix, but only at the **move**
points (`SourceParentSync`, `DestinationParentSync`, `FinalRevalidation`), never
the retirement ones. Port that narrow claim; do not re-port the fault-point
matrix.

**The cross-transition claim.**
`fresh_identity_can_archive_after_a_complete_compensating_recovery` requires an
installation to be reusable *after* a completed compensating recovery — a new
transition can still archive the same previous state. Coordinated
cross-transition tests exist (`active_reblit_forward.rs:1024`, `:1103`, and the
new `identity_preflight_guards.rs:283`) but every one of them starts from a
**successful** prior transition, never from a completed rollback. Genuine port:
drive a rollback to terminal, then `reacquire_new_state` and archive.

**Order to work in (revised 2026-08-04):** two deletions are now settled
(`retained_exchange_post_move_faults_*` and
`retained_reverse_exchange_post_move_faults_*`), leaving **2 real ports**:
the cross-transition one first, then the reduced retirement claim — which the
two remaining tests likely collapse into a single test, since both reduce to
"the dispatcher resumes a retirement fault".

**Before sizing anything else against a fault-injection seam, check which
*type* of fault point the coordinated route uses.** There are at least two
parallel families (`RetainedExchangeFaultPoint` on the legacy exchange,
`UsrExchangeParentDurabilityFaultPoint` on the coordinated recovery suffix)
with overlapping variant names. Grepping a variant name proves nothing.

## The ActivateArchived rollback cannot complete — found 2026-08-04

Surfaced while porting `fresh_identity_can_archive_after_a_complete_compensating_recovery`.
Reproducer: `journal_coordinator_a_completed_rollback_leaves_the_installation_reusable`
in `usr_exchange_effect.rs`, `#[ignore]`d with the full trail in its comment.

**Symptom.** An ActivateArchived rollback reaches `CandidatePreserveIntent` and
stops: recovery-pending, **no blocker named**, no advance, forever. Measured
trail `[ReverseExchangeIntent, UsrRestored, CandidatePreserveIntent x30]`.

**Cause.** `archived_topology` requires the candidate's marker to have
`marker_links() == 2` — a state-slot hardlink
(`candidate_preserve_proof.rs:715`). Two independent facts make that
unsatisfiable:

1. **Nothing creates state-slot links.** Every non-test reference to
   `.cast-state-slot-` in the crate is a read (`starts_with`, `strip_prefix`,
   inspection, error text) or a doc comment. No product code constructs that
   name or hardlinks it. The only creators in the tree are test fixtures.
2. **The forward path forbids two links anyway.** Every step of the forward
   ActivateArchived flow is a strict `nlink=1` reader —
   `begin_candidate_prepare_through_staging`, `prepare_archived_isolation`, and
   the exchange preflight each refuse `UnsafeMarker { links: 2 }`. Verified by
   planting the link at each position via the new
   `coordinator_from_exchange_fixture_after_begin` hook.

`archived_topology` is also the odd one out among the three:

| topology | required `marker_links()` |
|---|---|
| `new_state_topology` (:673) | 1 |
| `active_reblit_topology` (:773) | 1 |
| `archived_topology` (:715) | **2** |

**Why it is silent.** `UsrRollbackCandidatePreserveAuthority::capture` returns
`Deferred`, which the gate maps to `Dispatch::Unhandled` — pending, nothing
reported. Four of its five deferral sites discard the underlying error with
`Err(_)` (`usr_rollback_candidate_preserve_authority.rs:213`, `:217`, `:224`,
`:228`), so a *permanent* deferral is undiagnosable by construction. This is
worth fixing on its own merits, independently of the predicate.

**Consistent with** the guest-side `CandidatePreserveIntent` deferrals behind
tasks #18, #26 and #27, which circled this phase repeatedly without pinning a
cause.

**Two follow-ups, both product changes:**
1. Fix the predicate — almost certainly `== 1`, matching its two siblings — or
   establish what should have been creating the link. Needs a decision, not a
   guess: the `nlink=2` concept is real elsewhere (`tree_marker.rs:317`,
   `transition_identity.rs:919`) and belongs to ActiveReblit previous-slot
   parking.
2. Make deferral carry its reason, so a permanent one is diagnosable.

**Caveat.** Verified that no code constructs the slot-link *name*. Not
exhaustively traced whether such a link could arise another way — e.g. a
directory rename carrying a link created under an older format.

## Deletion pass — started 2026-08-04

**Done:** `tests/root_abi_preflight.rs` deleted whole (4 tests, all previously
ported; file held no shared helpers) plus its `mod` line. Suite after:
**2777 passed, 0 failed, 7 ignored.**

**Remaining, and two are not what the count implies.** Re-checking each target
before deleting caught this — the standing rule keeps paying:

| file | targets | note |
|---|---|---|
| `stateful_activation_recovery.rs` | 7 of 8 | 8th (`archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree`) has no legacy call site — verify before touching the file |
| `stateful_previous_tree_recovery.rs` | 2 of 5 | only the two settled deletions; 3 are still open ports, one blocked on task #29 |
| `active_reblit_tests.rs` | 1 test + 1 helper | `:561` is a ported test; **`:67` is inside `fn run(...)`, a shared helper** — its callers decide its fate, it is not a straight removal |
| `stateful_candidate_metadata.rs` | 0 tests + 1 helper | **`:408` is inside `fn apply_fresh_candidate(...)`, a helper**, not a test at all |

So the honest remaining count is **10 legacy-route tests**, not 19: 7 + 2 + 1,
plus two helper call sites that die only when their last legacy caller does.
The earlier "~19" counted call sites and assumed one test each.

### Deletion pass, second measurement — the count was undercounted twice

**Deleted so far (2026-08-04):** `root_abi_preflight.rs` whole (4), seven of
eight in `stateful_activation_recovery.rs` (the 8th,
`archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree`,
uses `activate_state` and survives), and the two settled deletions in
`stateful_previous_tree_recovery.rs`. **13 legacy tests gone.** Suite: **2768
passed, 0 failed, 7 ignored.**

**The remaining count is bigger than reported, and for a reason worth naming.**
Counting *call sites* undercounts whenever a test file routes the legacy call
through a shared helper. Two files do:

| file | legacy call sites | actual legacy-route tests |
|---|---|---|
| `active_reblit_tests.rs` | 2 | **~26** — `fn run(...)` (:67) wraps the call and has **26 callers**; 25 `#[test]`s in the file |
| `stateful_candidate_metadata.rs` | 1 | **~8** — `fn apply_fresh_candidate(...)` (:408) has **8 callers**; 10 `#[test]`s |
| `stateful_previous_tree_recovery.rs` | 4 | 3 (one test uses two call sites) |

So the real remaining figure is roughly **35–37 legacy-route tests**, not 10.
Both earlier numbers ("~19", then "10") were derived from call sites and are
withdrawn.

This does *not* mean 35 unported claims. The plan already records
`active_reblit_tests.rs` as "18 ported, 7 deletions" and
`stateful_candidate_metadata.rs` as "8 ported" — those verdicts stand; the
originals were simply never removed. The remaining work there is mostly
mechanical deletion, but it must be done per test against the recorded verdict,
not in bulk.

**Method note for the rest of the pass:** count `#[test]` attributes and trace
shared helpers before estimating. A single `grep` for the route entry point
measures how many *places call it*, which is not the quantity anyone cares
about.

### Deletion pass — COMPLETE except the open ports (2026-08-04)

| file | outcome |
|---|---|
| `tests/root_abi_preflight.rs` | deleted whole (4) |
| `tests/stateful_activation_recovery.rs` | 7 of 8 deleted; 8th survives on `activate_state` |
| `tests/stateful_candidate_metadata.rs` | 8 of 10 deleted with `apply_fresh_candidate` and its now-orphaned helpers; the 2 survivors drive `decorate_stateful` directly |
| `client/active_reblit_tests.rs` | **deleted whole** (25 tests + `fn run`) — the plan's "18 ported, 7 deletions" accounted for every test in the file; module was self-contained, nothing exported or referenced |
| `tests/stateful_previous_tree_recovery.rs` | 2 of 5 deleted; **3 remain, all open ports** |

**46 legacy tests removed this session** (4 + 7 + 8 + 25 + 2). Suite:
**2735 passed, 0 failed, 7 ignored** — clean at every step.

**No test outside `stateful_previous_tree_recovery.rs` touches the legacy route
any more.** The remaining call sites are:

- `tests/stateful_previous_tree_recovery.rs` (4 sites / 3 tests) — the open
  ports: the cross-transition one blocked on task #29, plus the two reduced
  retirement claims
- product code: `client/core/stateful_transition.rs` (the definitions),
  `core/state_metadata.rs`, `core/state_planning.rs`,
  `new_state_boot_transition.rs`, `journal_coordinator/new_state_forward.rs` (2)

So #10's remaining shape is: **3 ports, then excise the route from those five
product files.** The test-side work is otherwise done.

## CORRECTION 2026-08-05 — the "ActivateArchived rollback defect" was a fixture error

The section above titled "The ActivateArchived rollback cannot complete" is
**wrong and is withdrawn.** The predicate is correct; my fixture was not.

`archived_topology`'s `marker_links() == 2` requirement *is* satisfiable.
`RetainedIdentity::prepare` — the path taken for `ExistingId` candidates, i.e.
ActivateArchived — calls `adopt_or_create_before_journal_for_transition()`,
the single `nlink=2`-tolerant reader, and then authorizes the extra link
(`tree_lifecycle.rs:536`). Every later read is strict and passes *because* the
retained marker was authorized during preparation.

**The ordering is the requirement.** The archived candidate must already carry
its slot link when preparation runs, exactly as production does — the tree came
*out of* that slot. Two wrong orderings, both measured:

| fixture ordering | result |
|---|---|
| link planted after preparation | never authorized; `UnsafeMarker links=2` at `begin_candidate_prepare_through_staging`, then `prepare_archived_isolation` |
| no link at all | rollback topology unsatisfied; silent deferral loop at `CandidatePreserveIntent` |
| **link planted before preparation** | **passes** |

**Where the reasoning went wrong.** I established that no product code
constructs a `.cast-state-slot-` name and concluded the requirement was
unsatisfiable. The first half was true and the second did not follow: the link
is *pre-existing state the candidate arrives with*, not something the
transition creates. Absence of a creator in this crate says nothing about
whether the input can legitimately have one. Two other signals should have
stopped me — `tree_lifecycle.rs:536` exists precisely to authorize such a link,
and `adopt_or_create_before_journal_for_transition` documents itself as
tolerating `nlink=2`. Both were read and neither was weighed.

**Generalizable:** "nothing in this crate creates X" is not evidence that X
cannot be present. Check whether X is an input before concluding it is
impossible.

**Task #30 stands, and is the real finding.** The deferral discards its reason
(`usr_rollback_candidate_preserve_authority.rs:213`, `:217`, `:224`, `:228`),
which is exactly why a fixture ordering error presented as a product liveness
bug and took most of a session to unwind. A deferral that named "candidate
marker has 1 link, expected 2" would have ended this in minutes.

**Port complete.** `journal_coordinator_a_completed_rollback_leaves_the_installation_reusable`
passes, is no longer `#[ignore]`d, and asserts the full claim: journal absent,
previous live at its original identity, candidate rearchived into its slot, and
a fresh transition acquirable afterwards. Legacy original deleted. 6x161
stress-clean; suite **2735 passed, 0 failed, 6 ignored**.

## Candidate-preserve deferrals now carry a reason — 2026-08-05 (task #30)

`UsrRollbackCandidatePreserveAdmission::Deferred` was a unit variant; four of
the five sites that produced it discarded the underlying error with `Err(_)`.
The gate turns a deferral into `Dispatch::Unhandled` — a recovery-pending
result with no blocker — so a *permanent* deferral was invisible: every restart
looked identical and named nothing.

`Deferred` now carries `UsrRollbackCandidatePreserveDeferral`:

| variant | site |
|---|---|
| `RollbackPlanAbsent` | record has no rollback plan |
| `NamespaceInspectionBegin(String)` | inspection could not start (topology, binding) |
| `DatabaseIncompatibleOrPlanInexact` | database disagrees with the record |
| `DatabaseChangedDuringCapture` | database moved between the two passes |
| `NamespaceInspectionFinish(String)` | namespace moved, or failed topology, during capture |

The two that previously swallowed an error now render it into the payload. All
three gates (`usr_rollback_activate_archived`, `usr_rollback_active_reblit`,
`usr_rollback_new_state`) log it at `warn` before returning `Unhandled`.

**The existing deferral tests were strengthened rather than merely repaired.**
They asserted "some deferral"; they now pin *which*, and all four predictions
held on the first run — a database-clear hook yields
`DatabaseChangedDuringCapture`, a namespace-change hook yields
`NamespaceInspectionFinish`, and both topology refusals yield
`NamespaceInspectionBegin`. That mapping is now executable documentation of
which guard owns which failure.

This is the change that would have prevented the task-#29 detour: the stall
reported nothing, so a fixture ordering error read as a product liveness bug.
A deferral naming "candidate marker has 1 link, expected 2" ends that in
minutes.

**Suite: 2734 passed, 1 failed, 6 ignored.** The single failure
(`workflow_registry_reads_reject_a_sibling_transition_after_public_preflight`)
is **pre-existing and not caused by this change** — a stashed baseline fails
with the identical count, and the test passes 3/3 in isolation. It belongs with
the known full-suite-only nondeterministic cluster, still unexplained.

## Retirement ports done — the test side of #10 is COMPLETE (2026-08-05)

Both legacy tests reduce to one claim — *the dispatcher resumes a slot-retirement
fault* — so they collapsed into
`recovery_sealed_restore_resumes_its_slot_retirement_suffix`, plus one sibling
for the point that behaves differently.

**Reachability was settled before any assertion was written**, per the standing
rule. Result, and it contradicts what the legacy test names imply:

| point | consumed during `archive_previous_tree` | consumed during the restore | restore outcome |
|---|---|---|---|
| `BeforeSlotRetire` | no | yes | fails, `Applied` |
| `RootsAfterSlotRetireSync` | no | yes | fails, `Applied` |
| `FinalSlotRetirementRevalidation` | no | yes | fails, `Applied` |
| `AfterSlotRetire` | no | yes | **succeeds** |

**The retirement suffix belongs to the restore, not the archive.**
`archive_previous_tree` completes with the fault still armed at every one of the
four points. The legacy names
(`previous_archive_abort_retirement_faults_...`) point at the archive and are
misleading.

`AfterSlotRetire` is the one a restore survives: by then the retiring rename is
durable, so the remaining work is re-derivable and the fault has nothing left to
invalidate. It gets its own test —
`recovery_sealed_restore_survives_a_fault_after_the_retiring_rename` — which
asserts the fault *was consumed*, so "tolerated" cannot be confused with "never
reached". Folding it into the loop would have forced an `expect_err` that does
not hold.

New accessor `retained_previous_move_faults_remaining()`
(`transition_identity/fault_injection.rs`), the previous-move counterpart of
`retained_exchange_fault_armed()`. **Fourth time the armed-versus-consumed
distinction has decided a verdict** (staging-wrapper rotation, previous-slot
parking, reverse exchange, now this).

**No test in the crate calls the legacy route any more.** Every remaining
`apply_stateful_blit_with_checkpoint` / `commit_stateful_staging` site is
product code:

- `client/core/stateful_transition.rs` (the definitions)
- `client/core/state_metadata.rs`
- `client/core/state_planning.rs`
- `client/new_state_boot_transition.rs`
- `transition_identity/journal_coordinator/new_state_forward.rs` (2)

Suite **2735 passed, 0 failed, 6 ignored**; 6x163 stress-clean on the
coordinator module.

**#10 reduces to one step: excise the route from those five files.**

## Crash matrix: first real ActivateArchived phase cut — 2026-08-05 (task #9)

**The harness had been measuring nothing since `fe529969`.** Two output-
swallowing bugs, found in sequence on the approved VM:

1. **BusyBox `grep`.** The guest init piped the operation through
   `grep -a --line-buffered "CAST-AT-PHASE"`. BusyBox grep supports *neither*
   flag, so the pipeline died with a usage message immediately after
   `CELL-READY` and the install never ran. Signature: qemu burning **15s of CPU
   in 10:45 of wall clock** — an idle guest, not a slow one. That filter was
   added in `fe529969`, the same commit that recorded the last end-to-end
   success, so every cell run after it was inert.

2. **`| tail -2` on a parking command.** `tail` holds its whole input until EOF,
   and a phase-targeted `cast` never reaches EOF — parking is the entire point.
   So the pipe swallowed `CAST-AT-PHASE` for exactly the command the marker
   exists to observe.

That is now **three** distinct ways this harness has hidden the marker from
itself (`>/dev/null 2>&1`, BusyBox `grep`, `tail`), and four wrong diagnoses
across the epic. **Standing rule: any command that can park writes straight to
the console, unpiped, unredirected.** A flooded serial log is cosmetic; it is
also what settled all three.

**First meaningful verdict:**

    activate  phase:ActivateArchived.CandidatePrepared
              recovery=PENDING driver=recovered-at-4 state=installed
    PHASE-1: CandidatePreserveIntent
    PHASE-2: CandidatePreserved
    PHASE-3: RollbackComplete

The marker fired — proof by construction, not inference: the runner prints
`TIMED-OUT` or `NOT-ON-CHAIN` whenever `CAST-AT-PHASE` is absent from the
console, and it printed neither.

Read correctly: a cut at `CandidatePrepared` leaves a record at
`RollbackDecided`; the read-only path *correctly* refuses (`recovery=PENDING`
is right, not a failure), and the mutating driver walks the chain to
`RollbackComplete` in four invocations, ending `state=installed`.

**This independently corroborates the task-#29 correction.** The rollback walks
*through* `CandidatePreserveIntent` and completes on a real guest — the exact
phase my in-process fixture stalled at. Confirmation from a different
instrument that the predicate was never the problem.

**Still to run:** ActiveReblit and archived-repair cells. The harness is only
now capable of measuring them.

### ActiveReblit cell — driver added, damage vector still wrong (2026-08-05)

`MATRIX_OPS` / `MATRIX_CUTS` are now env-overridable, and `stage_and_reblit`
exists: install, damage the live tree, then `cast state verify -y`, which is the
only production path to `Operation::ActiveReblit` (`client/verify.rs:295`).

**The cell runs but reports `NOT-ON-CHAIN`** — verify prints `No issues found`,
so no reblit is attempted and the phase is never reached. Two damage targets
tried, both wrong:

| target | result |
|---|---|
| `find /mnt/root/usr -type f \| head -1` | picks tree metadata (`.stateID`, `.cast-tree-id`, `lib/os-release`) — not in the state's VFS, so not an issue |
| `find /mnt/root/usr/share -type f \| head -1` | identical `No issues found`; the package may not install under `share/`, or the path was empty |

Verify has two halves and only the second can trigger a reblit: assets are
checked by hash in the content store (`verify.rs:61`), while
`MissingVFSPath` is raised only for paths in the *state's VFS* under
`installation.root.join("usr")` (`verify.rs:129`). The damage must therefore be
a file the installed package actually owns.

**Next step:** enumerate the package's VFS instead of guessing at the
filesystem — `cast state query` or the stone contents — and delete a path that
is provably in it. Print the victim *and* verify's issue count in the cell, so a
no-op damages loudly instead of scoring `NOT-ON-CHAIN`, which reads like a real
answer about the phase when it is really a statement about the setup. That
failure mode — a setup no-op scoring as a verdict — is the same one
`stage_and_activate` had when it activated an already-active state.

**Archived repair** has no cell yet; `verify` handles archived states in the
same pass, so the same damage vector likely unblocks both.

### ActiveReblit cell — DEFECT FOUND 2026-08-05 (task #31)

The damage vector was never the problem. Two "identical" `No issues found`
results were **stale logs**: `pkill -f crash-matrix-run.sh` issued over ssh
matches the ssh command string itself, so it killed its own shell before
`nohup` launched, and I then read the previous run's log twice and drew a
conclusion from it. Byte-identical output including the same asset hashes was
the tell, and I explained it away as determinism. **Kill patterns must exclude
self — use `pgrep -f "[c]rash-matrix-run"`.**

With a genuinely fresh run:

    reblit  phase:ActiveReblit.CandidatePrepared
            recovery=PENDING driver=stalled-at-CandidatePreserveIntent state=absent
    PHASE-1: CandidatePreserveIntent
    STALL: ... at CandidatePreserveIntent requires ResumeRollback{...};
           recovery effects remain blocked by []

**ActiveReblit rollback advances once and stops.** `state=absent` — the package
never returns. This is a real durability failure, not slow-but-converging
recovery: the driver retries to its cap and the phase stops changing, which is
this plan's own criterion for a genuine stall.

**It is ActiveReblit-specific.** The ActivateArchived cell at the same phase
walks `CandidatePreserveIntent -> CandidatePreserved -> RollbackComplete` and
reaches `state=installed`.

`blocked by []` is an empty blocker list — the silent-deferral signature #30
addressed. The staged guest binary already contains #30, so the deferral now
carries a reason; it is simply not printed without tracing on. **Next step:
export `RUST_LOG=warn` in the guest init and read the `candidate preservation
deferred` line, which names which of the five deferral variants fires.** That is
precisely what #30 was built for, and it should identify the guard in one run.

The package *does* install under `/usr/share/bash-completion/` (verified by
extracting `pkg.stone` on the VM — note `cast extract` needs its parent at
0700), so `find /mnt/root/usr/share -type f | head -1` is a valid target and the
cell's damage instrumentation (`DAMAGE-TARGET` / `DAMAGE-OK`) can stay.

### ActiveReblit stall narrowed — 2026-08-05 (task #31)

Reproduced on **three** independent runs (distinct transition ids each time), so
it is not the stale-log artefact that muddied the first attempts.

`RUST_LOG=warn` was a false start: cast configures tracing from `--log`
(`tracing_common::logging::init_log`) and ignores the env var. The driver
invocation now passes `cast --log warn`.

**The #30 deferral warn did not appear — and that is informative.**
`usr_rollback_active_reblit.rs:155` handles `Operation::ActiveReblit`, `:160`
handles `Phase::CandidatePreserveIntent`, and its `Deferred` arm is
instrumented. Silence means `capture` did **not** return `Deferred`. Two
candidates remain:

1. `capture` returns `NotApplicable` via `!rollback_evidence_is_on_chain(record)`
   (`usr_rollback_candidate_preserve_authority.rs:199`). `NotApplicable` is
   deliberately un-instrumented because it normally means "not my case" — but
   the record here *is* ActiveReblit at `CandidatePreserveIntent`, so on-chain
   evidence failing is itself the defect.
2. The chain never reaches this gate; another dispatcher claims or drops the
   record first (`usr_rollback_resume_route.rs` also names this phase).

One run separates them: log the `NotApplicable` branch and repeat the cell.

**#30 is validated by this even though it did not name the cause.** It converted
"stalls, reason unknowable" into "stalls, and provably not via any of the five
deferral paths" — which is what eliminated half the search space in a single
run.

**Harness rule learned the hard way:** kill matrix runs with
`bash /tmp/cmkill.sh` in its *own* ssh invocation. `pkill -f` / `pgrep -f` over
ssh matches the ssh command string itself and kills the shell before it acts.
That silently produced two stale-log readings here, and I drew a conclusion from
one of them before noticing the output was byte-identical across "different"
runs.

### ActiveReblit stall LOCALIZED — the gate is never reached (2026-08-05)

Fourth reproduction, distinct transition id. Two instrumented paths inside
`UsrRollbackCandidatePreserveAuthority::capture` were checked and **both stay
silent**:

- all five `Deferred` variants (#30 instrumentation)
- the off-chain `NotApplicable`
  (`usr_rollback_candidate_preserve_authority.rs:199`), newly instrumented

**The instrument was calibrated before the negative was trusted** —
`/tmp/cast --log warn list installed -D /tmp/nonexistent-root` emits a
tracing-formatted line on stderr, and the driver captures stderr into the string
the STALL block prints. A warn would have shown. This matters: the same session
had already been misled four times by uncalibrated instruments, so a silent
result was not accepted as evidence until silence was shown to be meaningful.

**Therefore `capture` is never called.** The
`usr_rollback_active_reblit.rs:160` `CandidatePreserveIntent` arm is not
reached; something upstream in the startup-gate chain claims or drops the
ActiveReblit record first. `usr_rollback_resume_route.rs` also references this
phase and is the prime suspect.

**Next step:** warn at each dispatcher's entry with operation+phase, run the
cell once, and read which handler sees the record first. The ActivateArchived
chain reaches its gate and completes for the same phase, so diffing the two
routings should isolate it immediately.

### ActiveReblit stall: the claimant is upstream of the reblit dispatcher (2026-08-05)

Fifth reproduction. An **unconditional** `tracing::warn!` placed immediately
before `usr_rollback_active_reblit::dispatch` (`startup_gate.rs:644`) — no
predicate at all, it fires whenever control reaches the line — **did not
print**.

So `CleanSystemStartup::enter` returns before line 644. Combined with the
earlier negatives, the picture is now:

| checked | result |
|---|---|
| all five `Deferred` variants in `capture` | silent |
| off-chain `NotApplicable` in `capture` | silent |
| unconditional probe before the reblit dispatcher | **silent** |
| log path itself (positive control) | **works** — `cast --log warn` emits on stderr, driver captures it |

The claimant is therefore one of the stages *above* `startup_gate.rs:644`:
the ActiveReblit boot-sync chain (`:276`, `:303`, `:329`, `:355`, `:381`),
`dispatch_usr_rollback_previous_restore_and_reopen` (`:570`),
`dispatch_usr_rollback_reverse_and_reopen` (`:605`), or
`usr_rollback_activate_archived::dispatch` (`:626`).

Note the shape: every one of those returns `Err(Error::RecoveryPending(...))`
on `Handled`, which is exactly the error the guest reports. A stage that
*recognises* the record and hands back `RecoveryPending` without advancing it
produces precisely this stall — the record's phase never changes, so the driver
sees the same error forever.

**Next step:** bisect with the same probe. Put an ActiveReblit-scoped warn
before each of those call sites and run the cell once; the last one that prints
is the claimant. The probe is already committed and scoped to
`Operation::ActiveReblit`, so it is safe to leave in place while bisecting.

**Method note:** the positive control is what makes each silence load-bearing.
Without it every one of these negatives would be the same uncalibrated-
instrument trap this epic hit four times.

### RETRACTION 2026-08-05 — the ActiveReblit localization is not established

The two preceding sections concluded (a) `capture` is never called and (b) the
claimant sits upstream of `startup_gate.rs:644`. **Both are withdrawn.** They
rested on forge-side `tracing::warn!` probes staying silent, and that inference
does not hold.

A bisect with **eight** probes — seven stage markers plus an *unconditional* one
before the first dispatcher in the chain (`active_reblit_boot_sync_started`,
`:269`) — produced **no output at all**. A probe that fires whenever control
reaches a line, on the first stage of the chain, cannot legitimately be silent
for a record the chain exists to handle. The likelier reading is that
**forge-side tracing never reaches the guest console**, which voids every probe
result in this investigation.

**The positive control was mis-targeted, and that is the lesson.** It emitted
`ERROR cast: ...` — target `cast`, the binary's own. That proves the subscriber
and the driver's stderr capture work *for that target*. It says nothing about
whether events from the `forge` library arrive. `init_log` does call
`.with_default(level)` (`tracing_common/src/logging.rs:23`), so forge events
ought to pass — making the silence a contradiction that has to be resolved
before it can be used as evidence.

This is the **fifth** uncalibrated-instrument failure in this epic, and the
first where the calibration itself was wrong rather than absent. A positive
control must exercise *the same target, sink and filter path* as the
measurement, not merely the same binary.

**Next step, before any further bisecting:** unconditional `tracing::warn!` at
the very top of `CleanSystemStartup::enter`, before any fallible call.
Prints => forge tracing works, silences are real, resume the bisect.
Silent => the instrument is broken and every probe result so far is void.

**The defect itself is unaffected and remains solid:** six reproductions,
distinct transition ids, `state=absent`, ActiveReblit-specific.
