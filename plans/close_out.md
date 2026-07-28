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

Verify 0.3 and 0.5, then mark the section closed. Cheap, and it stops the next
reader planning around problems that no longer exist.

---

### D. Widen the crash campaign · E:XL R:high · §2.1

The harness is proven; what remains is coverage, not invention. Largely
subsumed by A above, but as its own effort:

- All four operations crossed with all journal phases
- Verdicts keyed by the guest's `boot_id` (already read by the harness)
- Enough invocations for incremental recovery to converge, with the phase
  tracked so a genuine stall is distinguishable from slow progress

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
