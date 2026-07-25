# Recovery identity for the PreviousRestore rollback suffix — design note

**Status:** design, not implemented. Blocks Phase 1.1 Slice 2 (`plans/future_impl.md`).
**Audit date:** 2026-07-25
**Planned against:** `feature/feature_plan` at `88af9235`
**Risk:** high — this is the plan's "catastrophic if wrong" area (a wrong slot
reconstruction leaks or misparks a state slot on the rollback path).

## Why this exists

The PreviousRestore dispatcher must undo a completed predecessor archive after a
crash. Its physical primitives are built and tested:

- `StatefulTreeIdentity::restore_previous_with_journal` (Slice 1, `21d6c6fd`)
- `StatefulTreeIdentity::finish_applied_previous_restore_with_journal` (`47368525`)

Both are reachable **only in-process**. Neither can fire during recovery, which
is the only situation the dispatcher exists for.

## The blocker, as verified

Pinned by `a_fresh_identity_after_the_archive_cannot_restore_the_previous_tree`
(`client/tests/stateful_previous_tree_recovery.rs`, commit `88af9235`). It
archives, drops the archiving identity — reproducing the post-reboot state — and
shows recovery cannot proceed. Two independent walls, in order:

1. **No identity can be constructed.** All ten `prepare*` variants funnel through
   `tree_lifecycle.rs::prepare_candidate`, which hardcodes `previous_path =
   root.join("usr")` (:328) and `previous_store = open_or_synthesize_live_usr`
   (:357), and pins `candidate_path` (staging). That models exactly one
   topology: **candidate staged, previous live** — the pre-exchange shape.
   Post-archive the namespace is the opposite: candidate is live at `/usr`,
   staging is empty, predecessor is at `root/<previous_id>/usr`. Preparation
   dies at `pin named /usr directory → NotFound`.
2. **Even given an identity, restore refuses.** `move_previous` returns
   `PreviousArchiveAttemptMissing` for `Restore` when
   `previous_archive_attempt` is `None` (:264-269). That field is populated
   only by `create_previous_archive_attempt`, whose single caller (:271) is
   `Archive`-only, and the sole identity constructor sets it to `None`
   (`tree_lifecycle.rs:450`). It is per-process, in-memory state.

Note also: `prepare` takes the journal with `JournalAcquisition::LegacyBlocking`,
so two live identities deadlock. Recovery is necessarily sequential — the
archiving identity is gone, not competing.

## What must be built

### A. Recovery identity constructor

A constructor for the post-archive topology. It cannot reuse `prepare_candidate`
unchanged, because the previous store must be opened at the **archived slot**
rather than live `/usr`, and the candidate is already live.

- candidate store ← live `/usr` (already exchanged)
- previous store ← `root/<previous_id>/usr`
- `previous_classification` ← `Active(previous_id)` from the journal record
- journal ← already held by the recovery dispatcher; must **not** re-acquire
  blocking (see the deadlock note). Mirror
  `JournalAcquisition::CoordinatorNonblocking` with a recovery seal.
- `require_clean_baseline` must not apply: a journal record legitimately exists.

### B. Archive-attempt adoption

Rebuild `RetainedPreviousArchiveAttempt` from disk. Six fields; four are direct:

| field | source |
|---|---|
| `name` | `canonical_state_name(previous_id)` |
| `roots` | open `root/` |
| `staging` | `roots.open_child("staging")` |
| `slot` | open the **existing** published `roots/<previous_id>` (creation errors on this with `PreviousArchiveSlotExists`) |

The remaining two are the risk, because they drive slot retirement
(`finish_previous_slot_retirement` moves the slot back to a parking name):

- **`state_slot_marker`** — `Some` if the archive reused an activation wrapper,
  `None` if it created a fresh slot.
- **`parking_name`** — where retirement returns the slot.

## The crux: the parking evidence was consumed

A successful archive **renames** the slot from its parking name into the
canonical decimal state name. So post-archive the parking name is free, and the
namespace no longer records which one the slot came from, nor which family it
belonged to. There are two distinct families:

- reused wrapper → `archived_candidate_parking_name(state, token, index)`
  (`reusable_previous_slot.rs:29`)
- fresh slot → `previous_slot_parking_name(state, token, index)` =
  `.previous-slot-{state}-{token}-{index}` (`namespace_helpers.rs:59-69`)

Both are deterministic in `(state, previous_tree_token, index)`, and the token is
readable from the archived tree's retained marker — so candidate names are
enumerable. What is *not* directly recoverable is the original `index`, and the
family.

**One usable discriminator exists.** A reused wrapper is an authenticated
marker-only directory (`find_reusable_previous_state_slot` requires
`RetainedStateSlotMarker::open_expected` plus `require_exact_entries([marker])`),
whereas a fresh slot is created empty by `create_private_previous_slot`. After
the tree moves in, the published slot therefore contains `usr` **plus the state
slot marker** in the reuse case, and `usr` **alone** in the fresh case. That
distinguishes `state_slot_marker` reliably from on-disk evidence.

The `index` remains ambiguous. Retirement needs *a* valid free parking name, and
the comment at `previous_tree_move.rs:413-416` states the intent — reusing the
wrapper "returns its deterministic parking name to the free pool after a later
restore instead of leaking one wrapper per successful activation" — so for the
**reuse** case the specific name is accounting-relevant, not arbitrary.

## Decision D-PR1 — RESOLVED (user, 2026-07-25): persist in the journal record

**Adoption does not infer the parking name. The archive records it.**

The forward archive writes the parking name (and its family) into the journal
record before the publishing rename, so recovery reads exact evidence rather
than reconstructing consumed evidence. This is the only option that makes the
rollback exact; the accepted cost is a journal model change and a careful edit to
the crash-matrix-verified forward path (consistent with D1.1, which already
accepted editing that path with the crash matrix as the net). There is no
migration cost — see the mechanism below.

Ordering constraint: the parking name must be durable in the record **before**
the slot-publishing rename consumes it, otherwise a crash between the two leaves
the same ambiguity this decision exists to remove.

### Confirmed mechanism (verified against the codec, 2026-07-25)

The record is a serde JSON payload with `format` + `version`, not a SQL schema,
and it already carries an additive-optional precedent: `boot_publication_receipts`
(`model.rs:395-396`) uses `#[serde(default, skip_serializing_if = "Option::is_none")]`
and was introduced by the V2→V3 bump. This change follows that pattern exactly:

```rust
// model.rs, on TransitionRecord
#[serde(default, skip_serializing_if = "Option::is_none")]
pub(crate) previous_archive_slot: Option<PreviousArchiveSlot>,

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviousArchiveSlot {
    /// Where retirement returns the slot after a restore.
    pub(crate) parking_name: QuarantineName,
    /// Discriminates the two name families and drives `state_slot_marker`:
    /// a reused activation wrapper vs a freshly created slot.
    pub(crate) reused_wrapper: bool,
}
```

`parking_name` is already a validated `QuarantineName` at the producing site —
`previous_slot_parking_name` builds it via `QuarantineName::parse`
(`namespace_helpers.rs:64`) — so the journal model's existing newtype applies
unchanged, including its `MAX_QUARANTINE_NAME_BYTES` bound.

**No versioning work.** `os-tools` is unreleased and owes no compatibility to
any record in the wild (see `plans/cleanup_legacy.md` §1, which deletes the
existing `PAYLOAD_VERSION_V1`/`_V2` fallbacks outright). The field is simply
added to the record; records written before it are invalid, not migrated. Drop
the earlier plan of a version bump plus presence-by-version validation — the
only rule needed is the unconditional invariant:

- the field is present exactly for records at or past the archive phases when
  `options.archive_previous` holds, and absent otherwise.

Doing `cleanup_legacy.md` §1 first makes this a pure addition.

The options considered and rejected are kept below for the record.

### Options considered

1. **Lowest free index in the discriminated family.** Simple and deterministic.
   Risk: if the original index differed, a later activation could observe a
   different free-pool shape than it would have. Needs a check that reuse
   accounting tolerates renumbering.
2. **Persist the parking name in the journal record at archive time.** Removes
   the ambiguity entirely and makes recovery exact rather than inferred. Costs a
   journal model/schema change and a migration, and touches the crash-matrix
   verified forward path.
3. **Refuse adoption when the family/index cannot be proven.** Keep today's
   fail-safe `RecoveryPending` for the ambiguous case and auto-recover only the
   provable one. Safest, but leaves a manual-recovery hole exactly where the
   dispatcher was supposed to close it.

Option 2 is the only one that makes recovery *exact*; 1 and 3 both infer.

## Sizing

The earlier "~500 lines mirroring the reverse authority" estimate covered only
the authority stack. A + B are new work on top of it, in the most
safety-critical module in the crate. The authority stack itself is unchanged in
shape from the note in `phase1-newstate-durability` memory.

## Exit criteria

- Recovery can construct an identity for the post-archive topology, proven by
  flipping `a_fresh_identity_after_the_archive_cannot_restore_the_previous_tree`
  from asserting refusal to asserting a successful restore.
- Adoption round-trips both slot provenances (fresh + reused wrapper), each with
  its retirement asserted.
- `startup_new_state_previous_archived_fails_safe_pending_not_bricked` converges
  to `RollbackComplete`.
- VM reboot matrix (per the destructive-tests-in-VM rule) — in-process fixtures
  cannot prove cross-reboot behaviour.
