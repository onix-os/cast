# State-Activation Startup Reconciliation

[Back to the Phase 11 recovery hub](state-activation-recovery.md)

[Back to the canonical package-function plan](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)

This continuation owns phase-specific startup admission, the bounded rollback
prefix, diagnostic reconciliation, and interruption evidence. Phase order,
completion, and repository closure remain authoritative in `PLAN.md`.

## Reconciliation objective and rollback ordering

- [ ] Reconcile startup using exact phase-specific namespace and database
  evidence. Every pre-commit phase rolls back except a durably completed boot
  synchronization; `CommitDecided` and later roll forward. Resume rollback in
  its persisted order, never delete a fresh DB row before preserving its
  candidate, never guess through a foreign occupant, and retain an
  undeletable `BootRepairUnverified` record when boot side effects cannot be
  proved repaired.

## Admitted startup recovery ladder

  As of 2026-07-16, startup's diagnostic checkpoint remains deliberately
  read-only and fail closed. Immediately before it, the mutable startup gate has
  one sealed, bounded recovery ladder: the ActiveReblit restrictive
  replacement-mode normalizer, descriptor-bound forward exchange-parent
  durability normalization, rollback-decision persistence, journal-only
  rollback-resume routing, and the exact `/usr` reverse dispatcher. The same
  sealed route step now also moves exact `UsrRestored` to
  `CandidatePreserveIntent` on a separate later entry. Each step captures its own
  authority from the current canonical record and fresh database and namespace
  evidence; none converts the diagnostic inventory into mutation authority.
  Decision, routing, and reversal are separate persisted boundaries rather than
  a same-process rollback loop.

  Commit `3e1ba34` introduced the journal-only rollback-decision boundary. The
  decision path applies to NewState, ActivateArchived, and ActiveReblit.
  Exact `UsrExchangeIntent` + `PRE` records the `/usr` rollback action as already
  satisfied, while exact `UsrExchanged` + `POST` records it as pending. Exact
  `UsrExchangeIntent` + `POST` now yields a distinct consuming durability
  authority rather than a decision or diagnostic deferral. Every other
  phase/layout or incompatible evidence combination remains non-mutating. A
  private startup seal admits independently captured evidence retaining exact
  before/after/fresh descriptor-rooted namespace fingerprints and layout,
  stable database ownership and immutable metadata provenance, the
  cooperating-writer reservation, installation authority, and the complete
  source record. An opaque per-open binding also prevents equal-looking or
  byte-identical journals from another root from consuming that authority.

  Commit `72511b3` added the separate consuming parent-durability path. The
  durability normalizer checks that per-open binding before any filesystem
  effect, then syncs the retained `.cast/root/staging` directory followed by the
  retained installation root: the two exact parents of the atomic exchange.
  It never reopens either parent by path and contains no rename, exchange,
  reverse, database, trigger, cleanup, or root-link operation. After both
  barriers it repeats the complete journal/namespace/database evidence
  sandwich and converts through a private completion seal into ordinary
  rollback-decision authority with `/usr` reversal pending. A sync error or
  evidence race consumes the authority and leaves the exact
  `UsrExchangeIntent` record for a fresh startup to retry the idempotent
  durability suffix; it cannot retry the exchange in process.

  After final database/namespace/database and journal revalidation, the executor
  derives exactly one successor with `rollback_decision` and attempts exactly
  one conditional `advance`. It performs no namespace or database mutation,
  retries no uncertain write, and executes no rollback, roll-forward, cleanup,
  or trigger effect. The old authority and lock-bearing store are dropped before
  a descriptor-rooted reopen; the complete canonical record must reconcile to
  the exact source or exact decision, including all error-after-application
  outcomes, before startup reports the result.

  Commit `911dcbc` separated rollback routing from decision persistence.
  Startup deliberately permits only one recovery journal mutation per entry.
  An entry which persists `RollbackDecided` returns `RecoveryPending`; it does
  not immediately route that new record. A later entry reloads the decision and
  independently captures a sealed authority retaining the complete record,
  exact per-open journal binding, stable database ownership and immutable
  provenance, before/after/fresh descriptor-rooted namespace fingerprints, and
  the cooperating-writer reservation. The decision must contain the exact
  operation-specific rollback plan. A pending `/usr` action with exact `POST`
  layout selects `ReverseExchangeIntent`; an already-satisfied action with exact
  `PRE` layout selects `CandidatePreserveIntent`. Every other plan, phase,
  layout, binding, database, provenance, or namespace combination remains
  non-mutating.

  Commit `c7c97d4c` reuses that same sealed authority for one additional exact
  source: `UsrRestored` whose recorded forward rollback source is
  `UsrExchangeIntent` or `UsrExchanged`, whose `/usr` evidence is `PRE`, and
  whose `/usr` outcome is `Applied` or `AlreadySatisfied`. Previous archive and
  boot actions must be `NotRequired`, and candidate preservation must still be
  `Pending`. NewState requires fresh-row invalidation pending, quarantine, and
  possible external effects; ActivateArchived requires no fresh-row action,
  rearchive, and no external effects; ActiveReblit requires no fresh-row action,
  quarantine, and possible external effects. Exact journal binding, stable
  database ownership and provenance, and the descriptor-rooted namespace
  sandwich remain mandatory. A transition-quarantine wrapper at `UsrRestored`
  is rejected as a premature candidate-movement prefix rather than accepted as
  evidence for the route.

  After a complete database/namespace/database and journal revalidation, the
  route executor calls `rollback_successor(None)` exactly once and attempts one
  conditional journal advance. This yields `CandidatePreserveIntent` for an
  admitted `UsrRestored` source. It contains no reverse exchange, rename,
  non-journal filesystem write, database mutation, trigger, cleanup, candidate
  movement, or root-link effect. Before descriptor-rooted reopen it drops both
  the old authority and lock-bearing journal store. Success accepts only the
  exact successor; an uncertain advance is reported only after reopen
  reconciles the complete canonical record to the exact source or successor.
  There is no same-process retry or continuation into the selected intent.

  A later startup may admit only an exact `ReverseExchangeIntent` under a
  private reverse seal. Exact `POST` evidence yields a consuming Apply
  authority; exact `PRE` evidence yields a consuming Finish authority because
  the namespace is already restored. Both authorities retain the installation,
  journal binding, cooperating-writer reservation, complete source record,
  stable database ownership and provenance, and descriptor-rooted namespace
  proof. Apply makes exactly one retained descriptor-relative exchange attempt
  and recaptures the layout rather than trusting the raw syscall report. An
  applied layout continues even if the syscall reported an error; a semantic
  non-application or ambiguous layout terminates that startup entry and returns
  no reusable effect or journal authority. Finish makes no exchange attempt.

  Both successful paths complete staging-parent and installation-root
  durability in that order, revalidate all evidence, and derive the sole legal
  `UsrRestored` successor. The persisted outcome is exact: an exchange applied
  by this entry records `Applied`, while an already restored PRE layout records
  `AlreadySatisfied`. Persistence performs one conditional journal advance,
  then destroys the old effect authority and lock-bearing store before a
  descriptor-rooted canonical reopen. A storage error remains an error even
  when reopen proves whether the exact source or exact `UsrRestored` successor
  is durable; it never authorizes an in-process retry or later rollback action.

  The one-recovery-journal-mutation-per-entry rule therefore remains intact.
  One entry may persist `RollbackDecided`, a later one may persist
  `ReverseExchangeIntent`, and a later one may perform the admitted reverse and
  persist `UsrRestored`. Because the journal-only route ran earlier in that
  startup entry, the reverse entry stops there and returns `RecoveryPending`.
  One fresh later entry may route exact `UsrRestored` to
  `CandidatePreserveIntent`, again returns `RecoveryPending`, and performs no
  preservation effect. Thus no startup entry advances more than one phase.

## Diagnostic reconciliation and namespace inventory

  The assessment then classifies every validated persisted phase as begin
  rollback, resume rollback, roll forward, finalize rollback, or manual
  boot repair; correlates the exact candidate and previous database rows with
  a before/after global transition audit; distinguishes allocation committed
  behind an older journal generation; and rejects phase-incompatible cleared,
  missing, foreign, or changing ownership. It also reads candidate metadata
  provenance in both database inspections. Fresh states require absence through
  `FreshStateAllocated`, admit either exact commit outcome only at
  `CandidatePrepareStarted`, and require the immutable pair afterward until
  exact database invalidation removes it; rollback derives the same rule from
  its recorded forward source. Existing-state operations require provenance
  from `Preparing`, so legacy absence is a typed blocker. A stable missing pair
  and a pair deleted between inspection reads are distinguished. Runtime tree
  witnesses are compared
  only when two epoch captures match the journal's creation epoch. Every known
  live, staging, state-slot, and quarantine name is reopened through a final
  directory-and-marker identity sandwich, while an otherwise valid two-link
  state-slot marker remains typed but unauthorized.

  The snapshot now includes a complete diagnostic activation-namespace
  inventory. Before and after the remaining startup evidence, it walks retained
  descriptors for `/usr`, `.cast/root`, and `.cast/quarantine` under aggregate
  entry, raw-name, operation, and deadline bounds, then reopens the public names
  and journal. It rejects foreign root/isolation ABI entries, access/default
  ACLs, noncanonical or changing wrappers, and orphan or multiply owned slot
  links. State-ID absence, canonical bytes, and corruption remain typed rather
  than collapsed. Every accepted link is bound to its exact tree inode, token,
  state, wrapper location, and transition role. The phase policy covers forward
  and rollback layouts, persisted action outcomes, archived rearchive versus
  quarantine, synthesized-empty absence, trigger-dependent isolation ABI,
  root-ABI completion, ambient archived states, and the phase-aware
  ActiveReblit replacement reservation.

  This diagnostic inventory is still not recovery authority, is not reused by
  any recovery executor, and exposes no mutation API. Inspection retains the
  installation, journal, and exact database
  capabilities through its final revalidation, then releases the mutable
  installation/global locks and exclusive journal before returning
  `RecoveryPending`; keeping that journal after the startup coordinator was
  released would permit a coordinator/journal ABBA deadlock. A retry must
  independently acquire locks in canonical order and reload the journal. The
  focused `make forge-startup-activation-namespace-test` lane proves 20 exact
  namespace contracts: 9 original inventory and policy contracts, 1
  isolation-ABI crash-prefix contract, and 10 partial-replacement contracts.
  `make forge-startup-reconciliation-test` continues to prove 9 database,
  provenance, epoch, substitution, retention, and lock-release contracts. The
  sealed coordinator reservation now makes replacement evidence optional at
  `CandidatePrepared` and mandatory from `TransactionTriggersStarted`; the
  complete isolation ABI is likewise mandatory once trigger intent is durable.
  Startup may normalize only the authenticated restrictive reservation prefix,
  and it still rejects a generic quarantine fallback after trigger intent. The
  focused `make forge-startup-usr-rollback-decision-test` lane passes 11/11
  contracts across the three operations and both admitted layouts, including
  all five journal-update fault points, mixed-root same-record rejection,
  database/provenance/namespace races, historical runtime epochs, and retained
  ActiveReblit reservation exclusion. The separate
  `make forge-startup-usr-exchange-parent-durability-test` lane passes 11/11
  contracts. Ten focused startup contracts cover exact ordering, retained
  parent identity, sync failures, evidence races, retry idempotence, mixed-store
  rejection, historical evidence, and ActiveReblit. Its eleventh contract runs
  all three operations through the real one-shot coordinator exchange at each
  of the three forward durability fault points, releases the failed authority,
  enters real startup, and proves the exchange syscall count remains exactly
  one while the exact pending-reverse decision is persisted without database or
  non-journal namespace changes. The additional
  `make forge-startup-usr-rollback-resume-route-test` lane passes 12/12 focused
  routing contracts. Its added matrix crosses all three operations, both
  admitted forward rollback sources, and both exact restored outcomes on PRE
  evidence. The lane also retains both decision successors, journal binding and
  mixed-root rejection, database/provenance/namespace and final-revalidation
  races, premature transition-quarantine rejection, historical epochs,
  ActiveReblit reservation retention, and all five journal-update fault
  prefixes with exact source/successor reopen reconciliation.

## Deterministic restart and process-death evidence

  The real reverse-dispatch lane added through commits `e69ad276`, `50cb98f8`,
  `86c6c900`, `ecd58020`, and `e8c952f9` now passes twelve dispatcher contracts
  plus two coordinator-origin contracts. Its in-process parent-durability
  restart matrix crosses all three operations,
  POST and PRE, and all three injected interruption points: staging-parent sync,
  installation-root sync, and final PRE capture. POST also covers ordinary
  success and error-after-application syscall reports; PRE correctly makes no
  exchange attempt. After a physical reverse, an injected failure leaves
  `ReverseExchangeIntent` canonical; a fresh startup observes PRE, completes
  the durability suffix without a second exchange, and persists
  `UsrRestored(AlreadySatisfied)`. A third startup performs the separate
  journal-only route to `CandidatePreserveIntent`, with no reverse redispatch
  or candidate effect.
  Its journal restart matrix crosses all three operations, POST and PRE, and all
  five conditional-update fault points. Canonical reopen finds only the exact
  source or exact `UsrRestored` successor. If the source survived, fresh startup
  finishes the already restored layout without exchanging again and stops; if
  the successor survived, that startup routes it to `CandidatePreserveIntent`.
  A later entry performs the same route for the source-survived case. Neither
  restart path mutates the
  database, root links, or non-journal namespace after the failed entry.

  A separate contract drops the failed startup result, old `Installation`,
  reservation, and database connection, then opens a fresh `Installation` and
  fresh descriptor-anchored state-database handle. Across all three operations,
  both a final-PRE-capture fault and a journal temporary-sync fault then converge
  from PRE without a second exchange. This individual contract remains an
  in-process handle-reopen simulation; the two process matrices below provide
  the real process-death coverage.

  Commit `ecd58020` re-executes the exact test binary as separate crash and
  recovery processes and sends genuine `SIGKILL` at four reverse boundaries:
  after the retained exchange but before semantic recapture, after the staging
  parent barrier but before the installation-root barrier, before the final PRE
  capture, and before final persistence revalidation. Crossing all three
  operations gives 12 process-death cases. The parent drops its original
  installation, database, journal, and reservation handles first; each child
  opens fresh installation and database handles. Recovery must see physical
  PRE, attempt no second exchange, persist exact
  `UsrRestored(AlreadySatisfied)`, then route it on another startup to
  `CandidatePreserveIntent` without a preservation effect. A
  15-second deadline surrounds every child, and timeout cleanup kills and reaps
  a hung process rather than blocking the lane indefinitely.

  Commit `e8c952f9` applies the same crash/recovery process boundary to all three
  operations, both POST and PRE starting layouts, and five successful
  conditional journal-update durability points: temporary fully synced,
  canonical exchanged, first directory sync, displaced file unlinked, and
  final directory sync. This is a 3 x 2 x 5 matrix of 30 genuine `SIGKILL`
  cases. At the first boundary the canonical record is still the exact
  `ReverseExchangeIntent`, while the temporary contains the proposed
  `UsrRestored` successor. Fresh open discards that temporary; since the
  namespace has already reached PRE, restart derives
  `UsrRestored(AlreadySatisfied)`. This intentionally differs from a killed
  POST attempt's discarded temporary `UsrRestored(Applied)` record. At each of
  the other four boundaries the canonical successor is already published, so
  recovery preserves its exact original `Applied` or `AlreadySatisfied`
  outcome, removes any displaced temporary residue, performs no second
  exchange, and makes exactly the separate journal-only route to
  `CandidatePreserveIntent`. Crash and recovery again use
  fresh-process handles and strict child deadlines.

  The same lane classifies all four raw exchange report/layout combinations,
  rejects ambiguous post-attempt evidence, and freezes exact `Applied` versus
  `AlreadySatisfied` outcomes. Its evidence-race cross-product injects database,
  journal, and namespace changes during admission and immediately before the
  effect for all three operations under both POST and PRE. The immediate
  post-effect POST matrix covers journal and namespace races for all operations
  plus the reconstructible NewState database race. Final durable revalidation
  crosses that same evidence/operation set under both POST and PRE. Admission
  and pre-effect races perform zero exchanges and zero journal advances.
  Post-effect and final races may leave the physical PRE layout but never
  advance through conflicting evidence. They preserve the injected change,
  consume every mutation capability, and never retry the exchange in process.
  Every safely reversible case repairs the exact evidence and then proves a
  fresh, independently authenticated startup converges. Archived and
  ActiveReblit database-provenance deletions remain fail-closed only because
  their cleared ownership correctly makes the sole safe restoration API reject
  reinsertion.

  The two coordinator-origin contracts cross all three operations against,
  respectively, all three forward exchange-durability fault points and all five
  forward exchange-completion journal fault points. Separate startup entries
  persist the rollback decision, route it to `ReverseExchangeIntent`, execute
  exactly one reverse exchange, stop at the exact `UsrRestored` successor, then
  route a later entry to `CandidatePreserveIntent`.
  The forward and reverse syscall count is exactly two, the database is
  unchanged, and the later route neither redispatches nor performs candidate
  effects. The restored non-journal namespace comparison is semantic: it covers
  names, identities, modes, link counts, lengths, and payloads while deliberately
  excluding kernel rename timestamps, which a forward/reverse exchange cannot
  preserve.

## Remaining recovery campaign

  This is still only the authenticated `/usr` rollback prefix. No startup
  executor yet handles the effects of `CandidatePreserveIntent`, candidate
  preservation, fresh-row invalidation, the remaining rollback actions,
  roll-forward, boot repair, or cleanup. The exact reverse prefix now has both deterministic
  in-process contracts and genuine process-termination coverage. It still has
  no reboot or power-loss proof: `SIGKILL` preserves the kernel-visible state at
  termination and cannot establish which pre-fsync rename survives a power
  cycle. The complete campaign required below therefore remains open, as do
  this item and all six broad Phase 11 work items.
- [x] Add database ownership probes that distinguish matching, cleared,
  missing, and foreign transition rows, plus a bounded global orphan-token
  audit. Journal absence with any non-null transition token is corruption, not
  permission to start another transaction.
- [ ] Add deterministic process-kill and fault-injection coverage at every
  journal fsync, database mutation, rename/exchange, trigger boundary, archive,
  quarantine, and boot boundary. Reopening after each injected interruption
  must converge to exactly one authenticated live tree and one terminal
  outcome without deleting or overwriting a foreign entry.
  The reverse `/usr` prefix now covers 12 execution-boundary and 30
  journal-update-boundary `SIGKILL` cases with fresh-process reopen. This item
  remains unchecked because the other phases and true power-loss-equivalent
  durability outcomes are not yet covered.
