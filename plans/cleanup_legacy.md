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

## 1. Journal payload version compatibility · E:S R:low · **DONE**

**Outcome:** collapsed to a single payload version; zero `PAYLOAD_VERSION_V1`/
`_V2` references remain anywhere. Removed: both legacy constants, the
`matches!(self.version, V1 | V2 | PAYLOAD_VERSION)` fallback, all three
version-conditional validation branches, two dead version guards in
`successors.rs` (already unreachable after `validate()`), and the three
now-unused `PayloadVersion*Mismatch` error variants.

Invariants were preserved rather than dropped, as the caution below required:

- The V1 "cannot reach `BootRepairComplete` / cannot carry applied boot
  rollback" rules were *capability* statements about the old format, not
  invariants of the current one — current records legitimately do both. Deleted.
- The receipt-entry rule kept its real content (`None -> Some` only at
  `BootSyncStarted`); only the always-true version clause was dropped.
- `validate_boot_publication_receipts` kept its unconditional presence rule.

Tests: deleted the wholly-legacy ones (`payload_v1_remains_decodable...`,
`canonical_v2_full_frame...`, `legacy_payloads_freeze...`,
`typed_boot_sync_complete_successor_rejects_legacy_payload_versions`,
`startup_legacy_boot_sync_started_remains_rollback_eligible`,
`startup_legacy_v2_boot_sync_complete_without_receipt_pair_stays_forward_pending`)
and trimmed the legacy portions out of four mixed tests. Deleted the dead
helpers `build_legacy_boot_sync_started`, `legacy_boot_sync_complete_fixture`,
`assert_legacy_ready`.

The golden-frame fixtures were regenerated and renamed
(`transition-journal-v1-rollback-decided.*` →
`transition-journal-rollback-decided.*`). Note the old golden encoded a
*V1-shaped* record — `BootSyncStarted` with no receipts — which the single
version makes invalid; the golden now locks a currently-valid record.

### Original entry

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

## 2. Legacy stateful transition path · E:L R:high · **UNBLOCKED 2026-07-26**

**`apply_stateful_candidate` now has no production caller.** Both §1.1e
blockers are fixed (the namespace policy via D1.5, the in-flight marker via a
guarded clear at terminal finalize), so `state_planning.rs`'s first-install arm
routes through the coordinator like every other stateful transition.

What remains is the 616-line `client/core/stateful_transition.rs` definition
plus two test callers in `client/tests/fixed_staging_transition.rs` (:248,
:339). Removal is a deletion rather than a migration, but it is **not** mechanical:
the two test callers are security proofs, and deleting them silently drops
coverage. Surveyed 2026-07-26 — both are portable, and the exact work is:

1. `retained_state_id_write_never_targets_a_substituted_usr` (:228) proves the
   state-ID write follows the retained descriptor rather than a substituted
   `usr` pathname. Its hook `before_retained_state_metadata` already fires from
   **shared** code (`core/state_metadata.rs:89`), so the coordinated route
   triggers it unchanged. Port = retarget the call to
   `apply_new_state_candidate`; no production change.

2. `stateful_trigger_preparation_never_follows_a_replaced_isolation_root` (:260)
   proves trigger preparation does not follow a swapped isolation-root symlink.
   Its hook `after_stateful_isolation_root_retention` fires **only** from
   `stateful_transition.rs:174` — the legacy path itself — so deleting that file
   removes the sole call site and the proof with it. The coordinated route does
   create an isolation root, at `core/state_planning.rs:62`. Port = move the
   `#[cfg(test)] after_stateful_isolation_root_retention()` call to immediately
   after that `create_root_links`, then retarget the test.

Order: port both tests and verify they still fail-on-regression against the
coordinated route *before* deleting `stateful_transition.rs`. Deleting first
would make the ports unverifiable. §§3 and 4 follow once the path is gone.
Do it against a clean full-suite baseline — see §2.1a, which currently makes
full-suite results ambiguous. What remains is the 616-line `client/core/stateful_transition.rs`
definition plus two test callers in `client/tests/fixed_staging_transition.rs`
(:248, :339).

Removal is now a deletion rather than a migration, but it is not mechanical:
those two tests cover fixed-staging behaviour that needs either a coordinated
equivalent or an explicit decision that the coverage moved elsewhere. Do this
against a clean full-suite baseline, and expect §§3 and 4 to follow immediately
once the path is gone.

Original blocker analysis retained below.

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

**Blocker verified against the code (2026-07-25), not assumed.** Deleting this
today would remove shipping functionality outright:

- `state_planning.rs:115` drives **ActivateArchived** through
  `commit_stateful_staging(..., StatefulCandidateOrigin::Archived, ...)`, and
  there is **no coordinator replacement whatsoever** — no
  `execute_activate_archived` exists anywhere in the tree. Archived-state
  activation would simply cease to work.
- `state_planning.rs:185` drives **NewState** (package install/update) through
  `apply_stateful_candidate`. A coordinator route does exist
  (`execute_new_state_forward` → `apply_new_state_candidate`) but is **not
  wired as the default**; its only caller today is an integration test.
- Archived-state repair (1.3) likewise has no coordinator route.

So §2 is gated on Phase 1.1 **Slice 5** (wire NewState live), plus Phase 1.2 and
1.3 being built from scratch, plus the crash matrix — and per
`destructive-tests-in-vm`, the crash matrix needs the VM (which **is** reachable
at `192.168.122.148`; confirmed 2026-07-25).

**Phase 1 progress toward unblocking this (2026-07-25):** §1.1a's shared
applicability rules and prospective probe are shipped, and §1.1b's NewState
commit-cleanup authority is built and tested (admission gate, record advance,
same-store + reopened successor revalidation). Remaining before Slice 5 can even
be attempted: §1.1b's persistence step, then wiring §1.1a. A latent bug was also
found — the coordinated NewState route currently fails at commit cleanup on a
real system — see `future_impl.md` §1.1b. Full detail there; this section stays
blocked until Slice 5, 1.2 and 1.3 all land. §§3 and 4 are in turn
gated on §2: the `Legacy*` guard variants and `legacy_boot_repair` each still
have live callers inside the legacy route.

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

## 5. `#[allow(dead_code)]` scaffolding debt · E:M R:low · **DONE**

**Outcome: every one of the 208 allows now names why it exists (0 undocumented).**

Two measurements corrected the original assessment; both are recorded because
the wrong version of each is an easy trap to fall into again.

**Correction 1 — the debt was far smaller than it looked.** The original "only
~13 carry a rationale" was an artefact of grepping the *preceding* line. Most
rationales are *trailing* comments on the same line
(`#[allow(dead_code)] // consumed by ...`). Counting both forms: **186 of 208
were already documented**, leaving 22. Those 22 have been annotated, in these
groups:

- 8 retained-capture structs in `activation_namespace/capture/model.rs` — their
  fields hold `File` descriptors open for later revalidation and are never read
  individually.
- 7 shared `#[path]` test-support modules included by several test parents, each
  of which consumes only a subset.
- 2 archived-state-repair fault-injection arms; 1 compile-time signature pin;
  1 `getdents64` kernel ABI layout struct; 1 retained accessor; 1 `mason`
  deadline wrapper.

**Correction 2 — "most allows are stale" is FALSE. Do not mass-delete them.**
It is tempting to assume an allow on an item that compiles without warnings is
unnecessary. Measured directly: neutralising all allows and building with
`--tests` suggested 165 of 199 were stale. Removing those 165 produced **376
warnings in the production build against a baseline of 0**. The reason is that
`cargo build -p forge --tests` compiles `cfg(test)`, so test-only items look
used; in the production build (504 dead items with allows neutralised) they are
not. Any future audit must check **both** build configurations. The experiment
was reverted in full.

### Original entry

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

## 6. `TODO`/`FIXME` audit · E:S R:low · **DONE**

**State:** was 23 across `crates/` and `bin/`; **20 remain**. An earlier example
was already closed by `1218c00a` (`cli/repo.rs` canonical output).

**Deleted as stale (done):**

- `registry/plugin/active.rs:5` and `registry/plugin/cobble.rs:12` — bare
  `// TODO:` markers with no content at all.
- `dag/src/lib.rs:220` — `// TODO: How tf do i get node value from A to E?`, a
  scratch note inside a test whose following assertions already answer it.

**Substantive — needs a decision, do not silently "fix":**

- `vfs/src/tree/mod.rs:147` — `// TODO: Reenable` above a commented-out
  `return Err(e)`. Duplicate-path detection is currently **downgraded from an
  error to an `eprintln!` warning**. Re-enabling it is a real behaviour change
  (installs that currently succeed with duplicate reports would start failing),
  so it needs an explicit decision rather than a cleanup sweep. **D-CL6:** should
  duplicate paths fail closed?

**Remaining 19 — genuine future-work notes**, spread across `dag` (cycle
breaking), `vfs`, `forge` (`prune`, `sync`, `cache`, `postblit`, `util`,
`registry`, `cli/search`, `cli/repo` API overhaul), `mason` (`draft/metadata`
gitlab/github version parsing, `build/job/phase`), `stone`/`libstone`
(encoding, error types), `container` (error granularity, mount syscalls). None
block anything; each is a small independent improvement.

**Filed (done):** all 20 remaining comments are now recorded in
`plans/future_impl.md` §7.4, grouped by nature (correctness/behaviour, parsing
gaps, API/ergonomics, blocked-on-upstream, cosmetic). **No `TODO` or `FIXME` in
the tree lacks a plan reference.**

**Open:** D-CL6 is the only one needing a decision before it can be actioned;
the rest are small independent improvements that block nothing.

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
