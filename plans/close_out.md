# Close-out plan — what remains after Phase 1

Written 2026-07-27, after Phase 1's durability epic closed. This supersedes the
ordering in `future_impl.md` for everything still open; the detail for each item
still lives in that file (and in `cleanup_legacy.md`), which this one points at
rather than duplicates.

## Where the project actually is

Phase 1 is **closed, and more strongly than its own exit criterion asked for**.
That criterion wanted crash-matrix proof *in-process*, with real-reboot proof
deferred to Phase 2. What exists now is real power-cut proof across an actual
reboot: a nested guest whose unsynced writes are genuinely lost
(`crash-matrix-durability-probe.sh`), forge running inside it, and an
interrupted install recovering to `state=installed`
(`crash-matrix-run.sh`).

All three operations publish a durable journal that startup reconciliation
resumes: NewState including first install (§1.1), ActivateArchived through the
archived-staging pair (§1.2/§1.2b), archived repair via its interruption marker
(§1.3).

Supporting state: forge suite 2759/0, production build at zero warnings, one
source of truth for phase ordinals, `develop` holds everything, branches cleaned.

## Order of work

The sequence below is not arbitrary. A leads because it is the last Phase 1
item and the harness that answers it already exists; B and C are cheap and
remove active misinformation; D is the real remaining engineering; E is the
dangerous one and should not start until D can test it.

---

### A. Close §1.4 — forward cleanup crash-safety · E:M R:high · **do first**

The last open Phase 1 item. It asks whether any step between a journal advance
and a physical cleanup can be stranded by a crash — specifically
`rotate_active_reblit_staging`, `archive_previous`, and the
`recover/preserve_*candidate` paths.

**This is now cheap, because it is exactly what the crash matrix answers.**
Rather than auditing by reading, drive each operation and cut power at the
window in question, then check whether recovery converges.

1. Settle the open **1–2s anomaly** first, because it is in this area and would
   confound everything else. `crash-matrix-run.sh` reports `driver=FAILED` at
   1s and 2s while 0s, 3s and 5s recover. Raise the driver invocation cap well
   above 15 and check whether the phase in the error keeps *advancing*.
   Recovery is incremental, so "still pending at N tries" and "cannot recover"
   are indistinguishable at a fixed N — this exact confusion already produced
   one false defect report. It is only a defect if the phase stops changing.
2. Add cut points targeted at journal phases rather than wall-clock delays. The
   `arm_*` fault hooks already exist for most boundaries; a cut armed at a named
   phase is worth more than a second-granularity sweep.
3. Extend `OPS` past install to ActivateArchived, ActiveReblit and archived
   repair. Archived repair first: it is the newest durability claim and has zero
   crash coverage.

**Exit:** each operation's cleanup either provably resumes, or a stranded window
is named with a reproduction.

**STATUS 2026-07-27: done, and it found a stranded window.** A crash during
transaction triggers (pre-`/usr`-exchange) is unrecoverable — no startup
dispatcher covers those phases, so the record falls through to
`PendingSystemTransition` forever. Full evidence in `future_impl.md` §1.4 and in
`crash-matrix-run.sh`. The 1-2s anomaly that prompted this turned out to be
timing variance plus a harness artefact (`nothing-staged`), not a defect.

**A2 — pre-exchange recovery route · E:M R:high · MOSTLY DONE 2026-07-27.**

The journal model already supported this: `rollback_allowed` permits
pre-exchange sources and the derived plan correctly carries
`usr_exchange: NotRequired`. Every blocker was in the startup dispatch layer,
and each was the same shape — a hard-coded post-exchange assumption, duplicated.

Fixed, in order (each verified by re-running the 5s cut and watching the chain
advance one phase further):

1. `rollback_decision_source_is_supported` — allowlist covering only
   post-exchange phases.
2. The decision authority's `(Phase, UsrExchangeLayout)` match — an
   `unreachable!()` for pre-exchange, plus an overloaded `None` that meant
   "parent durability required". Split into an explicit not-required case.
3. `rollback_source_is_supported` — **the same allowlist was duplicated across
   9 sites** in 7 rollback authorities. Extracted to one predicate in
   `startup_reconciliation`.
4. `rollback_usr_exchange_is_settled` — the check that the exchange needs no
   further action was duplicated across **11 sites**, and every copy omitted
   `NotRequired`. Also extracted to one predicate.

Chain now runs: `TransactionTriggersStarted -> RollbackDecided ->
CandidatePreserveIntent -> CandidatePreserved -> FreshDbInvalidationIntent ->
FreshDbInvalidated -> RollbackComplete`. It previously never left
`TransactionTriggersStarted`.

**Remaining: the final `FinalizeRollback` step.** The record reaches
`RollbackComplete` and stalls there. Note there are finalization authorities for
ActivateArchived and ActiveReblit but none named for NewState, yet post-exchange
NewState rollback does finalize (the 8s cut recovers) — so find how that path
finalizes and why it rejects a pre-exchange source. Expect the same shape as the
four above.

**FIX VERIFIED, BUT THE BRANCH IS RED — DO NOT MERGE YET.** The 5s cut now
reports `recovered-at-7 state=installed`; a pre-exchange crash fully recovers.
The full suite is **2751 passed, 8 failed**.

One failure was already diagnosed and fixed: the first version of
`rollback_usr_exchange_is_settled` accepted `NotRequired` for *any* source,
which weakened a safety check across the whole rollback chain. `NotRequired` is
only coherent when the exchange was never possible — for a post-exchange source
the journal itself refuses to build such a record
(`InvalidRollbackRequirement { possible: true }`). Narrowed to pre-exchange
sources only; the exclusion tests that caught it now pass.

The remaining 8 are unassessed and each needs individual judgement — they are
NOT all the same shape:

- `client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_cleanup::restart::tests::component_process_kill::owned_cleanup_components_process_kills_recover_exactly`
- `client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_cleanup::restart::tests::receipt_replacement_reconstructs_fresh_authority_then_is_already_clean`
- `client::active_reblit_mounted_boot_topology::capture::publication_targets::owned_cleanup::restart::tests::receipt_stale_cleanup_reconciles_canonical_detached_and_already_clean`
- `client::startup_gate::usr_rollback_activate_archived::tests::exclusions::startup_activate_archived_complete_route_defers_every_inexact_plan_boundary`
- `client::startup_gate::usr_rollback_active_reblit::tests::complete_exclusions::startup_active_reblit_complete_route_preserves_operation_and_phase_ordering`
- `client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_bypasses_other_phases_and_sources`
- `client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_plan_requires_the_exact_operation_matrix`
- `client::startup_reconciliation::usr_rollback_fresh_db_invalidation_authority::tests::admission::startup_fresh_db_invalidation_plan_accepts_only_the_exact_new_state_pending_fresh_action`

At least two distinct causes are visible. `startup_candidate_preserve_admission_bypasses_other_phases_and_sources`
fails with `JournalReadDuringEffect(CanonicalChanged)`, which is not an
exclusion assertion at all. The `owned_cleanup::restart` ones are in a module
this change does not obviously touch — check whether they are pre-existing or
load-related before attributing them.

**RESOLVED 2026-07-27 — suite green at 2759/0, merged to `develop`.**

Triage of the 8 failures:

- **3 x `owned_cleanup::restart`** — not caused by this change. They pass 5/5 in
  ~4s in isolation and fail only under whole-suite contention. Initially
  misattributed by comparing runs under different load; the valid comparison is
  isolation-vs-isolation.
- **1 x fresh-db exclusion** — used `TransactionTriggersComplete` as its
  "unsupported source" case, which this change legitimately makes supported for
  NewState. Re-pointed at `Preparing`, still unsupported.
- **4 x remaining** — all one cause: a **third over-widening**. An earlier raw
  `matches!(rollback.usr_exchange, Applied | AlreadySatisfied | NotRequired)` in
  `candidate_preserve_plan_is_exact` was never converted to the shared
  predicate, so the operation/source narrowing never reached it and it accepted
  `NotRequired` unconditionally. Routing it through
  `rollback_usr_exchange_is_settled` fixed all four.

That third instance is the important one: it is exactly the failure mode this
epic exists to prevent — a corrupt record accepted at startup — and it survived
two rounds of narrowing because a raw check was left behind when the shared
predicate was introduced. **When extracting a predicate, grep for every raw copy
of the condition, not just the ones the compiler points at.**

**A second over-widening was found and fixed during triage.** The shared
predicates were applied to *all* operations, but only NewState's pre-exchange
window was ever measured. `rollback_source_is_supported` and
`rollback_usr_exchange_is_settled` now take the operation and permit
pre-exchange rollback for NewState only. ActiveReblit and ActivateArchived very
likely have the same gap — their pre-exchange phases map to `BeginRollback`
too — but extend deliberately, with a crash-matrix cell per operation, rather
than by widening a shared predicate.

**Before merging:** confirm each failure is either (a) a test encoding the old
"post-exchange only" contract, which this change deliberately reverses and which
should be updated, or (b) a real over-acceptance like the one already found. Do
not assume (a) — the over-widening above proves (b) happens.

---

### B. Finish `cleanup_legacy` §§3–4 · E:S-M R:med

§2 is done; §§3 and 4 are mechanical *except* for one real problem.

**The problem, found 2026-07-27:** three tests compare against
`StatefulTransitionCheckpoint::AfterTransactionTriggers`
(`client/tests/stateful_candidate_metadata.rs:155`,
`client/active_reblit_tests.rs:143` and `:198`). That variant is no longer
*constructed* — only the deleted `apply_stateful_candidate` emitted it. **Those
tests are silently passing without exercising what they name.** Decide whether
the coordinated route has an equivalent checkpoint and re-point them, or accept
the coverage loss explicitly. Do not just delete the variant and let the
comparisons disappear.

Same question, smaller, for `decorate_stateful`: four test files use it, and
each is a coverage decision rather than a deletion.

The seven orphaned *functions* delete cleanly (verified — production build still
compiles). Only the five enum variants cascade. Watch the name collisions:
`Stateful` and `Transaction` exist in more than one enum and
`postblit::RetainedTransactionKind::Stateful` is live.

Then §§3 and 4 follow mechanically: `ArchiveJournalGuard::LegacyNoJournal`,
`JournalAcquisition::LegacyBlocking` and `legacy_boot_repair` collapse once the
last legacy caller is gone.

---

### C. Retire stale Phase 0 · E:S R:low

Phase 0 no longer describes reality and is actively misleading:

- **0.1** — its test
  (`startup_reconciliation_database_phase_matrix_is_exact`) passes.
- **0.2** — the `forge-focused-tests.mk` line it names is already gone.
- **0.4** — asks to resolve 18 compiler warnings; there are zero.

**DONE 2026-07-27.** All six verified individually; 0.3 (flake pins 1.94.1
exactly) and 0.5 (host-scratch helper) were already satisfied bar one script,
whose `${TMPDIR:-/tmp}` fallback is now routed through
`lib/host-scratch-root.sh`. Phase 0 marked closed in `future_impl.md` with the
evidence for each item.

---

### D. Widen the crash campaign · E:XL R:high · §2.1

The harness is proven; what remains is coverage, not invention. Largely
subsumed by A above, but as its own effort:

- All four operations crossed with all journal phases
- Verdicts keyed by the guest's `boot_id` (already read by the harness)
- Enough invocations for incremental recovery to converge, with the phase
  tracked so a genuine stall is distinguishable from slow progress

**Started 2026-07-27, and it found the harness's real limit.** An `activate`
operation (install, then `cast state activate 1`) now exists in `OPS` and runs,
but its cells cannot be trusted: the setup install takes several seconds, so a
wall-clock cut at 10-16s most likely lands inside the *install* rather than the
ActivateArchived transition — and the `recovered-at-9` signature it reports is
the same one NewState rollback produces.

**Phase-targeted cuts landed 2026-07-27.** `CAST_CRASH_AT_PHASE` (accepting
`Phase` or `Operation:Phase`) makes `transition_journal::store::advance` print
`CAST-AT-PHASE` and park once the record is durable at that phase; the harness
cuts power on the marker. Parking rather than aborting is the point — aborting
leaves the page cache intact, so unsynced writes survive and the cell proves
nothing. Verified against the known defect: `phase:TransactionTriggersStarted`
on `install` cuts exactly there and recovery converges.

**Still open, and now precisely stated:** `OPS=(activate)` does not produce an
ActivateArchived transition. Targets at both
`ActivateArchived.TransactionTriggersStarted` and
`ActivateArchived.CandidatePrepared` report `CELL-OP-DONE` without the marker
printing, so `cast state activate 1` in the guest is not driving the journal
route those targets name. Diagnose that first — until it does, the `activate`
cells say nothing about whether ActivateArchived shares NewState's pre-exchange
gap.

Original note, still true of any operation added before targeting worked:

**So the next step for D is not more operations, it is phase-targeted cuts.**
Wall-clock delays cannot isolate one operation's window when the setup preceding
it takes seconds. The `arm_*` fault hooks already exist for most journal
boundaries; arm one at the phase under test and cut there. Every operation added
before that lands will produce cells that look green without testing what they
name — the same trap as the earlier `state=absent` column.

Specifically still unmeasured: whether ActiveReblit and ActivateArchived share
the pre-exchange recovery gap. The §1.4 fix was deliberately scoped to NewState
because that is the only operation whose window was measured.

**§2.1a is closed** — the `receipt_promotion::completion` cluster was
test-side `future_deadline()` helpers hard-coding short windows that contention
exhausted. Suite is green at 16 and 24 threads.

---

### E. Real startup boot repair · E:XL R:critical · §2.3

The plan's own note calls this the most dangerous item in the project, and that
still holds. **Do not start it before D can exercise it.** A boot-repair path
that cannot be crash-tested is exactly the thing this epic exists to avoid
shipping.

§2.2 (live `Ready`-branch boot regression) is the natural warm-up.

## Standing hazards worth remembering

Every one of these cost a misdiagnosis during Phase 1, and all are the same
shape — a hard-coded value silently encoding structure:

- Phase ordinals were duplicated across three tables, two invisible to a
  `.ordinal()` grep. **Now collapsed to one** — keep it that way.
- Generation expectations were hard-coded in production (`root_abi_publication`,
  the rollback finalization authority). **Now derived** — keep them derived.
- Test-side deadlines that do not scale with load look like durability failures.
  `timeout_policy::tests::the_test_build_actually_scales_budgets` guards the
  production budgets; the `future_deadline()` helpers are generous on purpose.

And one method lesson, which broke two separate deadlocks after repeated
guessing failed: **measure the value, do not derive it from assumed
arithmetic.** Instrument and print, then fix.

## Loose end

`develop` is 139 commits ahead of `origin/develop` and unpushed.
