# Durability close-out — what actually remains

Successor to `close_out.md`, `cleanup_legacy.md` and
`previous-restore-recovery-identity.md` (retired 2026-07-30; full text in git at
`fe12e530`). Only open work is kept here.

**State of `develop` at time of writing:** suite green 2752/0, production build at
zero warnings, all three stateful operations on the coordinated journal route.

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

And the method lesson that broke two deadlocks after repeated guessing failed:
**measure the value, do not derive it from assumed arithmetic.** Instrument and
print, then fix.

---

## Loose end

`develop` is ~139 commits ahead of `origin/develop` and unpushed.
