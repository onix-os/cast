# Legacy cleanup — audit and phased removal plan

**Status:** plan; nothing removed yet
**Audit date:** 2026-07-25
**Planned against:** `feature/feature_plan` at `0bf2c75a`
**Premise:** `os-tools` is unreleased with no installed base, so **no
backward compatibility is owed to anything**. Formats change directly; old
shapes get deleted rather than tolerated. See also `plans/future_impl.md`.

## Survey

`grep -rniE '\blegacy\b' crates/ bin/ --include=*.rs` → **222 hits across 81
files**. Not all are debt. Breakdown by nature:

| # | Category | Refs | Blocked by |
|---|---|---|---|
| 1 | Journal payload version compatibility | 27 | nothing — do now |
| 2 | Legacy stateful transition path | ~1000 lines | Phase 1 |
| 3 | Legacy journal guards (`LegacyNoJournal`, `LegacyBlocking`) | 22 | category 2 |
| 4 | `legacy_boot_repair` | 65 lines | category 2 |
| 5 | `#[allow(dead_code)]` scaffolding debt | 208 | partly Phase 1 |
| 6 | `TODO`/`FIXME` | 23 | nothing — audit |
| 7 | False positives (fixture paths, test locals) | ~45 | not debt |

---

## 1. Journal payload version compatibility · E:S R:low · **do first**

**State:** `codec.rs:15-23` defines `PAYLOAD_VERSION_V1 = 1`,
`PAYLOAD_VERSION_V2 = 2`, `PAYLOAD_VERSION = 3`. `validation.rs:137-155` accepts
all three and carries version-conditional rules (V1 may not reach
`BootRepairComplete`, may not carry applied boot rollback, etc.).
`validation.rs:742` gates receipt entries on the version. Surface spans 8 files
including 5 test modules.

**Action:** collapse to a single current payload version. Delete
`PAYLOAD_VERSION_V1`/`_V2`, every `matches!(self.version, ...)` fallback, and all
version-conditional validation branches. Records that do not carry the current
version are simply invalid.

**Why first:** it is self-contained, has no dependency on Phase 1, and it
directly unblocks the `previous_archive_slot` field in
`plans/previous-restore-recovery-identity.md` — that design currently spends a
version bump and a presence-by-version rule purely on compatibility that is not
owed. With this done, the field is just *added*.

**Care:** the version-conditional rules encode real phase invariants (e.g. boot
rollback status vs phase). Deleting the *version* condition must not delete the
*invariant* — re-express each as an unconditional rule where it still holds.

---

## 2. Legacy stateful transition path · E:L R:high · **blocked on Phase 1**

**State:** the untethered pre-journal route still drives real transitions:
`state_planning.rs:115` (`commit_stateful_staging`) and `:185`
(`apply_stateful_candidate`), implemented across
`core/stateful_transition.rs` (612 lines) and `core/stateful_recovery.rs`
(411 lines). Phase 1 is building the journal-durable coordinator replacement;
`plans/future_impl.md` explicitly says to keep the legacy path until the
coordinator route passes the full crash matrix.

**Action:** once NewState (1.1), ActivateArchived (1.2) and archived-repair (1.3)
all run through the coordinator and the crash matrix is green, delete the legacy
route and its recovery machinery outright.

**Do not start before Phase 1 lands.** This is the safety net for every
transition today.

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

## 5. `#[allow(dead_code)]` scaffolding debt · E:M R:low · **incremental**

**State:** 208 allows — 199 in `forge`, 5 `container`, 4 `mason`. Only ~13 carry
a rationale comment; the rest are unexplained.

Two distinct kinds, and they need opposite treatment:

- **Forward scaffolding** — built ahead of its caller, e.g.
  `restore_previous_with_journal` and
  `finish_applied_previous_restore_with_journal`. These are *intentional* and
  get their caller when Phase 1 lands. Keep, but every one must state *which*
  future caller justifies it.
- **Genuinely dead** — no planned caller. Delete.

**Action:** audit in slices by module. For each allow: either (a) attach a
one-line rationale naming the future caller, or (b) delete the item. A blanket
sweep is the wrong tool — the two kinds are indistinguishable without reading
the surrounding design.

**Exit:** every remaining `#[allow(dead_code)]` names its future caller.

---

## 6. `TODO`/`FIXME` audit · E:S R:low · **independent**

**State:** 23 across `crates/` and `bin/`. Known example now resolved:
`cli/repo.rs` canonical-output TODO (closed by `1218c00a`).

**Action:** trirage each into: fix now (small), file into `future_impl.md`
(real work), or delete (stale). No TODO should survive without a plan reference.

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
1. Payload version collapse        ── do now, unblocks the restore design
6. TODO/FIXME triage               ── anytime, independent
5. dead_code audit                 ── incremental, by module
        ↓ (Phase 1 must land first)
2. Legacy stateful transition path ── the big one
3. Legacy journal guards           ── mechanical after 2
4. legacy_boot_repair              ── deleted with 2
```

Categories 1, 5 and 6 are available immediately. Categories 2-4 are gated on
Phase 1 durability closure and must not be started before it — the legacy route
is currently the only proven path for real transitions.

## Exit criteria

- One payload version; no version-conditional parsing anywhere.
- One transition route (the journal coordinator); `stateful_transition.rs` and
  `stateful_recovery.rs` gone.
- No `Legacy*` variant in `ArchiveJournalGuard` or `JournalAcquisition`.
- Every surviving `#[allow(dead_code)]` names the caller that will use it.
- No `TODO`/`FIXME` without a plan reference.
