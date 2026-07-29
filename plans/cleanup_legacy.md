# Legacy cleanup — audit and phased removal plan

**Status:** §§1, 5, 6 complete; §§2-4 blocked on Phase 1 durability closure
**Audit date:** 2026-07-25
**Planned against:** `feature/feature_plan` at `0bf2c75a`; executed on `feature/cleanup_legacy`
**Premise:** `os-tools` is unreleased with no installed base, so **no
backward compatibility is owed to anything**. Formats change directly; old
shapes get deleted rather than tolerated. See also `plans/future_impl.md`.

## Survey

`grep -rniE '\blegacy\b' crates/ bin/ --include=*.rs` → **222 hits across 81
files**. Not all are debt. Breakdown by nature:

| # | Category | Refs | Blocked by |
|---|---|---|---|
| 1 | Journal payload version compatibility | 27 | nothing — **done** |
| 2 | Legacy stateful transition path | ~1000 lines | Phase 1 |
| 3 | Legacy journal guards (`LegacyNoJournal`, `LegacyBlocking`) | 22 | category 2 |
| 4 | `legacy_boot_repair` | 65 lines | category 2 |
| 5 | `#[allow(dead_code)]` scaffolding debt | 208 | nothing — **done** |
| 6 | `TODO`/`FIXME` | 23 → 20, all filed | nothing — **done** |
| 7 | False positives (fixture paths, test locals) | ~45 | not debt |

---

## 1, 2, 5, 6 — shipped (detail removed 2026-07-29)

- **1. Journal payload version compatibility** — collapsed to a single current
  version; no migrations, per the no-back-compat rule.
- **2. Legacy stateful transition path** — `apply_stateful_candidate` and its two
  tests removed. The security objection was investigated and withdrawn: the
  coordinated route passes the same `TriggerScope::RetainedTransaction` and
  sources `isolation_root` from `retained_isolation_root()` as a
  `&RetainedRootAbi`, so the TOCTOU invariant those tests probed at runtime is
  now enforced structurally. Deleting removed a proof, not a defence.
- **5. `#[allow(dead_code)]` scaffolding debt** — audited.
- **6. `TODO`/`FIXME` audit** — filed.

**Two findings from that work still govern §§3-4 below:**

1. **The 12 remaining "orphans" are not orphans.** Deleting them builds clean in
   production and fails with 7 errors under `--tests`: every one is reachable
   from `#[cfg(test)]` code. Their annotations say so. A dead-code warning from
   the production build proves nothing on its own.
2. **`LegacyNoJournal` was pinned by live production code**, not just tests —
   activation used it until the coordinated route was wired on 2026-07-29. That
   wiring is merged but not yet proven at guest level (`close_out.md`), so
   confirm before assuming §§3-4 are unblocked.

## §§2-4 UNBLOCKED 2026-07-29 — the legacy route has no production callers

Measured on `feature/wire_state_activate`, once `cast state activate` moved to
the coordinated route:

- `Client::apply_stateful_blit` (the `pub` entry) has **zero callers anywhere**
  in the workspace.
- `apply_stateful_blit_with_checkpoint` is called **only from `#[cfg(test)]`**
  (`root_abi_preflight.rs`, `active_reblit_tests.rs`, and the recovery suites).
- The one remaining production call to `commit_stateful_staging`
  (`stateful_transition.rs:210`) is reached only through those test entries, and
  passes only `Fresh` or `ActiveReblit` — **no production code passes
  `StatefulCandidateOrigin::Archived` any more.**

So the whole legacy stateful transition route is now dead production code, and
§§3-4 follow mechanically from removing it. That is the state finding 2 above
warned to confirm rather than assume; it is now confirmed by measurement.

**Scope of the removal, and why it is not a one-liner.** Deleting the route also
deletes the only caller of a large test surface that still asserts live
properties — ActiveReblit reblit behaviour and root-ABI preflight among them —
reached through the legacy entry. Each of those needs the same treatment the
archived-activation tests just got: confirm the coordinated counterpart proves
the property, then port or delete. The archived-activation half of that work is
done (13 tests retired, 1 re-pointed); the ActiveReblit and root-ABI halves are
not.

**Confirmed 2026-07-30: all three production operations are coordinated.**

| operation | production entry |
|---|---|
| NewState | `apply_new_state_candidate` (`state_planning.rs:170,189`) |
| ActiveReblit / verify | `apply_active_reblit_candidate` (`verify.rs:295`) |
| ActivateArchived | `apply_activate_archived_candidate` (merged 2026-07-30) |

Nothing else reaches `commit_stateful_staging`.

### The triage list — this is the actual remaining work

Removing the route deletes these 34 tests. They are the only callers, but several
assert properties whose coordinated equivalent is **not** obvious, so each needs
checking before deletion. Grouped by risk:

**Likely already covered by the coordinated suites** (`journal_coordinator`
`root_abi_publication_*` is 15 tests; `usr_rollback_*` authorities are dozens):

- `root_abi_preflight.rs` (4): live/absent root-ABI replacement at the exchange
  boundary, post-exchange publication conflict reversal.
- `stateful_previous_tree_recovery.rs` (5): archive/restore fault suffix routing.
- `stateful_activation_recovery.rs` (7 remaining): fresh/previous reverse
  retention, racing empty destinations, quarantine uniqueness.

**Needs a named counterpart before deleting — no obvious coordinated twin:**

- `first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr`
- `missing_live_usr_between_identity_check_and_exchange_is_never_recreated`
- `duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees`
- `recovery_rejects_same_content_marker_name_substitution_without_repair`
- `recovery_rejects_whole_directory_same_token_substitution_without_exchange`
- `orphan_transition_row_blocks_marker_publication_before_activation`
- `unresolved_journal_evidence_blocks_marker_publication_before_activation`
- the 7 in `stateful_quarantine_recovery.rs` (deterministic collision handling,
  durability-fault resumption, marker recreation refusal)

That second group is why this is not a delete-and-run. Each is a substitution or
adversarial-namespace proof, which is the class of test this epic exists to keep.

Order once the triage is done: production route (`apply_stateful_blit`,
`commit_stateful_staging`, `stateful_recovery.rs`), then the guards and
`legacy_boot_repair`, which collapse on their own.

---

## 3. Legacy journal guards · E:S R:med · **follows category 2**

**State:** `ArchiveJournalGuard::LegacyNoJournal`
(`transition_identity/previous_tree_move.rs:36`) and
`JournalAcquisition::LegacyBlocking` (`tree_lifecycle.rs:24`) — 22 non-test
references. Both exist solely so the legacy path can assert "no journal is
present" / take the journal blockingly. Every coordinator caller already uses the
sealed non-blocking variants.

**Action:** when category 2 goes, both enums collapse to their coordinator
variant, and `require_no_journal` disappears with them. Mechanical once the last
legacy caller is gone.

**Note:** `LegacyBlocking` is *why* two live identities deadlock — see
`plans/previous-restore-recovery-identity.md`. Removing it simplifies the
recovery-identity constructor.

---

## 4. `legacy_boot_repair` · E:S R:med · **follows category 2**

**State:** `client/legacy_boot_repair.rs` (65 lines), exported at
`transition_identity.rs:113`, with a live caller at
`core/stateful_recovery.rs:139` and an error variant at `core/error.rs:787`.

**Action:** delete with its only caller (category 2). The coordinator boot route
supersedes it.

---

## 7. False positives — leave alone

- `crates/mason/src/planner/tests/**` (~39 refs): fixture stone paths under a
  repository pool literally named `legacy/`
  (e.g. `../../../legacy/pool/d/dash/...`). This is test data, not code debt.
  Rename the pool only if the word keeps causing false hits in audits.
- `repository/handle_outdated.rs`, `repository/manager/tests/refresh.rs`: local
  test bindings named `legacy_uri` / `legacy_cache` describing the *scenario*
  under test (upgrading an outdated index URI). Correct naming; keep.

---

## Sequencing

```
1. Payload version collapse        ── DONE
5. dead_code audit                 ── DONE
6. TODO/FIXME triage               ── DONE (filed as future_impl.md §7.4)
        ↓ (Phase 1 must land first)
2. Legacy stateful transition path ── the big one
3. Legacy journal guards           ── mechanical after 2
4. legacy_boot_repair              ── deleted with 2
```

Categories 1, 5 and 6 are complete. Categories 2-4 are gated on
Phase 1 durability closure and must not be started before it — the legacy route
is currently the only proven path for real transitions.

## Exit criteria

- One payload version; no version-conditional parsing anywhere.
- One transition route (the journal coordinator); `stateful_transition.rs` and
  `stateful_recovery.rs` gone.
- No `Legacy*` variant in `ArchiveJournalGuard` or `JournalAcquisition`.
- Every surviving `#[allow(dead_code)]` names the caller that will use it.
- No `TODO`/`FIXME` without a plan reference.
