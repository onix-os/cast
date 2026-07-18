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

  As of 2026-07-18, startup's diagnostic checkpoint remains deliberately
  read-only and fail closed. Immediately before it, the mutable startup gate has
  one sealed, bounded recovery ladder: the ActiveReblit restrictive
  replacement-mode normalizer, descriptor-bound forward exchange-parent
  durability normalization, rollback-decision persistence, journal-only
  rollback-resume routing, and the exact `/usr` reverse dispatcher. Separate
  later entries route exact `UsrRestored` to `CandidatePreserveIntent`. NewState
  entries prepare the quarantine target, preserve the candidate, invalidate the
  exact fresh database row, advance through `FreshDbInvalidated` to
  `RollbackComplete`, and finally authenticate terminal journal absence.
  ActiveReblit instead preserves its whole replacement wrapper, then uses its
  exact cleared existing-state row, retained provenance, and preserved-wrapper
  proof to advance directly from `CandidatePreserved` to `RollbackComplete`.
  A separate later entry may finalize that exact terminal record and pass the
  same continuously locked journal store into shared clean admission; the
  completion-route entry never redispatches its successor.
  Each entry recaptures authority from the current canonical record and fresh
  database and namespace evidence, admits at most one preparation/effect
  checkpoint, at most one journal advance, or one terminal deletion, then
  returns without dispatching its successor. Inexact NewState or ActiveReblit
  terminal evidence and terminal records for unsupported operations remain
  diagnostic and fail closed; diagnostic inventory is never converted into
  mutation authority.

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

## Candidate-preservation admission foundation

  This section records the capabilities as they landed behind test-only seals.
  Its no-production-caller statements are commit-local history; the current
  production wiring is summarized under `Remaining recovery campaign`.

  Commit `7e0618dc` adds a sealed, read-only admission boundary for exact
  `CandidatePreserveIntent` evidence. The seal has only a test-only constructor,
  and the focused static gate proves that production has zero seal-construction
  and zero authority-capture call sites. Admission retains the exact per-open
  journal binding, active-state reservation, installation and state-database
  handles, complete record, database ownership and immutable provenance, and
  independent before/after namespace fingerprints. Revalidation checks the
  journal binding first, sandwiches fresh database and namespace evidence, and
  consumes neither the staged nor the already-preserved typestate.

  The complete admission matrix covers NewState, ActivateArchived, and
  ActiveReblit; rollback sources `UsrExchangeIntent` and `UsrExchanged`;
  recorded `/usr` outcomes `Applied` and `AlreadySatisfied`; and staged and
  already-preserved layouts. Staged evidence yields a private Apply typestate,
  while an already-preserved crash prefix yields a private Finish typestate.
  Those names classify evidence only: neither typestate exposes an operation
  which moves a candidate or advances the journal. NewState admits the exact
  staged topology, including one empty transition-quarantine target left by a
  create-before-move crash, or the exact preserved quarantine topology. Every
  existing NewState target in either topology must have permissions exactly
  `0700`.
  ActivateArchived requires its canonical two-link state-slot relationship.
  ActiveReblit requires its unique reserved replacement wrapper and retains its
  exact, possibly nonzero wrapper index across staged and preserved evidence.

  Admission rejects an occupied NewState destination; missing, wrong,
  duplicate, or transferred archived slots; missing, duplicate, or wrong
  ActiveReblit reservations; a generic ActiveReblit quarantine; and an empty
  transition wrapper for archived activation or ActiveReblit. NewState and
  archived activation also reject unmodeled previous- or
  archived-candidate-parking wrappers. Empty or foreign canonical wrappers for
  a current previous-state ID are refused, as are current candidate-ID wrappers
  outside the exact archived destination; unrelated state wrappers remain
  admissible only when their complete fingerprints remain stable. The
  ActiveReblit reservation index is retained only in the startup-reconciliation
  topology, and its topology accessors remain test-only rather than becoming a
  client-wide API. Wrong phases, unsupported rollback sources, or any mismatch
  in the operation-specific rollback plan never yield preservation authority.
  Commit `3da2b3d5` also proves that all fifteen non-`0700` modes otherwise
  accepted by the general controlled-directory policy are refused, with no
  evidence mutation, for both staged-empty and already-preserved NewState
  layouts. POSIX access or default ACLs on the wrappers fail closed during
  namespace capture; arbitrary wrapper xattrs are not inspected and are not
  claimed absent.

  The focused
  `make forge-startup-usr-rollback-candidate-preserve-admission-test` lane
  retains a 24/24 admission inventory. Besides the full
  operation/source/outcome/layout
  matrix, it accepts historical runtime evidence, rejects a different open
  journal binding, invalidates database, provenance, and namespace changes,
  and defers or fails closed across initial-capture and fresh-revalidation
  races. Static gates forbid any effect or dispatcher surface, mutable
  filesystem operation, database mutation, journal successor or advance,
  trigger, cleanup, raw descriptor authority, or synchronization call in this
  boundary. It therefore establishes no production constructor, mutation,
  persistence, dispatch, effect, or durability claim.

### Historical test-sealed NewState target selection, creation, normalization, and move reconciliation

  Commit `d3bf0cd8` adds the first consuming preservation checkpoint without
  connecting it to production startup. Its initial effect path admits only the
  exact staged candidate plus an already-created, empty journal quarantine
  target. Already-preserved Finish authority remains non-mutating, and neither
  ActivateArchived nor ActiveReblit has an effect path.

  Commit `4f9e79cd` adds a policy-free, one-attempt directory-creation adapter,
  but gives it no production caller. Commit `fe880cde` separately models an
  absent NewState target, every owned restrictive-mode residue that can remain
  after interrupted preparation, and the canonical empty private target.
  Restrictive residues retain exact identity while their contents and ACL state
  remain deliberately unknown; they are not promoted to inspected empty
  wrappers. Unsafe modes, foreign ownership, and wrong target types still fail
  closed.

  Commit `c1418ad0` lets the private test seal select a different opaque lease
  for each exact prefix: Create for absence, Normalize for restrictive residue,
  and Move only for the canonical empty private target. At that checkpoint
  Create and Normalize had no operational methods, while Move retained the
  earlier one-shot operation. A payload-bearing restrictive residue may select
  Normalize evidence without claiming emptiness and without changing the
  payload. Archived activation and ActiveReblit remain fieldless Unsupported
  results.

  Every selection begins by checking the opaque binding of the journal opened
  for this startup entry and repeats the full retained evidence sandwich. Both
  consuming NewState effects also begin binding-first and repeat the exact
  installation, database, journal, plan, and retained-namespace evidence around
  their final PRE. Only the move path syncs the retained staged candidate tree.
  Creation neither syncs nor moves that candidate. The move sync is a
  candidate-data safety barrier, not a claim that the move or its changed
  parents are durable afterward.

  Commit `c998ad82` separates that namespace preparation from the irreversible
  move. After candidate sync and final PRE capture return an opaque prepared
  namespace authority, consumption checks the open-journal binding first again,
  then repeats the journal, database, installation, and plan evidence checks
  immediately before the one-shot move. A database or journal race during the
  potentially slow candidate sync therefore produces an error with zero move
  attempts. Preparation failures retain the trailing evidence observation but
  cannot produce move authority. Commit `3da2b3d5` makes exact target mode part
  of both the projection and final PRE evidence, so a last-moment change from
  `0700` to `0755` likewise fails before the move.

  Exact PRE authority permits one descriptor-relative `RENAME_NOREPLACE`
  attempt from staged `usr` into the empty quarantine wrapper. There is no loop
  and no in-process retry. The raw operation report is diagnostic only: a fresh
  full namespace capture alone classifies the result as `Applied`,
  `NotApplied`, or `Ambiguous`. Only `Applied` retains opaque authority for the
  later durability checkpoint; the other results are fieldless and carry no
  descriptor, lease, or retry capability. Database, journal, installation, and
  plan evidence are checked again after namespace use, including error-after-
  application and misleading-success outcomes.

  Commit `5ce3c2c9` separately consumes only the absent-target Create lease.
  After a final exact absent-target PRE and another binding-first full evidence
  check, it attempts descriptor-relative creation exactly once under the
  retained quarantine parent, using the exact journal-derived name and requested
  mode `0700`. It has no loop, retry, adoption, residue normalization, candidate
  sync, candidate move, or same-entry continuation.

  The raw creation report is diagnostic only. Fresh full namespace evidence
  classifies a completely unchanged fingerprint as `NotApplied`; a stable
  transition from absence to the exact restrictive residue or canonical empty
  private target as `RestartRequired`; and every other target, parent,
  namespace, or capture result as `Ambiguous`. `RestartRequired` describes the
  safe observed prefix rather than proving which actor created it. All three
  results are fieldless and retain no descriptor, retry, normalization, or move
  capability. Database, journal, installation, and plan evidence is checked
  again after the attempt, so even a safe prepared target requires a fresh
  startup entry.

  Canonical targets carrying POSIX access or default ACLs fail closed. A
  restrictive-mode residue remains opaque with respect to both payload and ACL
  state and is never promoted directly to move-ready evidence. Arbitrary user
  xattrs are not inspected and are explicitly outside the claimed security
  boundary.

  Commit `7bd1e640` separately consumes only the restrictive-residue Normalize
  lease. Binding-first non-namespace checks surround a final exact residue PRE,
  after which the checkpoint makes exactly one mode-normalization attempt on
  the retained target descriptor. It neither reopens a replacement inode nor
  trusts the raw chmod report. Fresh semantic evidence classifies a completely
  unchanged fingerprint as `NotApplied`, admits only the same-inode transition
  to an exact empty private target as canonical, and treats every other result
  as `Ambiguous`. Payload and ACL state remain opaque until that fresh
  inspection; arbitrary user xattrs remain uninspected and unclaimed.

  Commit `36fea65f` prevents even that fresh canonical evidence from escaping
  as completion. The checkpoint first syncs the exact retained target, then
  syncs the retained quarantine parent, with complete public-name and retained-
  identity revalidation around each barrier. It performs one final fresh
  canonical capture before returning `RestartRequired`. A barrier error,
  replacement, namespace race, or final mismatch yields `Ambiguous` and no
  partial durability authority.

  The externally observable normalization results are the fieldless
  `RestartRequired`, `NotApplied`, and `Ambiguous`. None retains evidence,
  descriptors, a retry, movement, or persistence capability. Even fully
  normalized and synchronized evidence therefore forces a new startup entry;
  this checkpoint cannot fall through into candidate movement.

  Commit `0d93f979` strengthens every freshly selected Move lease independently.
  Each lease repeats the retained candidate-tree barrier, then synchronizes the
  exact canonical target and retained quarantine parent in that order. Complete
  retained-descriptor, public-name, and full PRE revalidation surrounds the
  barriers, and one fresh final PRE capture must still match before namespace
  preparation can return.

  The enclosing authority then checks the open-journal binding first again and
  repeats the full installation, database, journal, and plan evidence. The
  resulting target-durable typestate performs one final exact pre-move
  revalidation before permitting at most one descriptor-relative
  `RENAME_NOREPLACE` attempt. The raw syscall helper is structurally private to
  that typestate; sibling paths cannot obtain it or bypass the durability
  constructor. This is pre-move durability only and adds no production
  dispatch, persistence, or post-move durability.

  The focused
  `make forge-startup-usr-rollback-candidate-preserve-effect-test` lane now
  passes 14/14 contracts. It covers effect selection, all raw-report/semantic-
  layout combinations from both rollback origins, ambiguity, final-prefix
  races, binding-first ordering, trailing database and journal checks, database
  and journal races during candidate sync, candidate namespace races, and the
  final target-mode race. Its four added contracts prove ordered target
  durability, exact fault prefixes, fail-closed namespace races, and repetition
  of the complete barriers by a fresh Move lease after failure.

  The focused
  `make forge-startup-usr-rollback-candidate-preserve-target-creation-test`
  lane passes 11/11 contracts. It covers misleading success and error reports,
  exact `EEXIST` post-states, every restrictive umask result, payload-bearing
  residue, unsafe modes and target types, ACLs, removal and replacement, parent
  rebinding, unrelated namespace changes, binding-first ordering, final-PRE
  races, and trailing database and journal races. Every case proves zero
  candidate-move attempts. At that checkpoint, the admission inventory was
  24/24, the target-prefix preparation lane was 3/3, the combined authority run
  was 38/38, creation was 11/11, and move reconciliation remained 10/10.

  The focused
  `make forge-startup-usr-rollback-candidate-preserve-target-normalization-test`
  lane passes 12/12 contracts. It covers the raw-report semantic matrix, every
  restrictive mode and rollback origin, concurrent canonicalization, retained-
  inode replacement defense, payload, ACL, and xattr boundaries, binding and
  final-PRE ordering, post-attempt ambiguity, exact target-then-parent
  durability order, durability faults, namespace races, and the final canonical
  capture. At that checkpoint the complete target-prefix aggregate was 26/26,
  the combined authority run was 50/50, preparation and creation remained 3/3
  and 11/11 respectively, and move reconciliation was 14/14. `make check`
  passed with only the four established warnings, while `make source-loc`
  reported all 1058 tracked text files at no more than 1000 lines.

  At the pre-move checkpoint, creation static gates permitted exactly one
  directory-creation attempt while
  forbidding retries, normalization, movement, synchronization, persistence,
  database or journal mutation, triggers, cleanup, descriptor escape, and
  production dispatch. Normalization static gates permitted one descriptor-bound
  mode attempt and exactly the ordered target-then-parent durability suffix,
  while forbidding creation, movement, retries, persistence, database or
  journal mutation, triggers, cleanup, and production dispatch. The move gates
  forbade target mutation, a second move, and post-move synchronization. All
  three checkpoints remained undispatched and claimed neither
  production recovery, a production target-preparation executor, post-create or
  post-move durability, nor completed candidate preservation.

  Commit `a84d0f47` implements the previously named indivisible post-move
  durability checkpoint behind a distinct test-only durability seal. Newly
  `Applied` movement and independently admitted exact NewState Finish evidence
  converge to the same consuming namespace suffix. Their provenance cannot be
  supplied by a caller: it is fixed internally to `Applied` for the first path
  and `AlreadySatisfied` for the second.

  The suffix order is exact: retained candidate tree, empty staging wrapper,
  journal target wrapper, retained quarantine parent, then one final fresh
  exact POST capture. Complete retained-descriptor, public-name, installation,
  and full POST identity checks surround every physical barrier. Both origins
  begin with the open-journal binding check and full pre-effect evidence, then
  perform a trailing binding-first full non-namespace gate regardless of the
  namespace result.

  No partial physical prefix yields authority. A later exact Finish admission
  must rerun the complete idempotent suffix after any failure rather than
  continuing from an intermediate barrier. Archived and ActiveReblit Finish
  evidence selects only fieldless `Unsupported` and performs no durability
  event.

  The dedicated
  `make forge-startup-usr-rollback-candidate-preserve-post-move-durability-test`
  lane passes 6/6 contracts. The combined authority run passes 56/56, and the
  existing move lane remains 14/14. `make check` passes with only the four
  established warnings; `make source-loc` reports all 1063 tracked text files
  at no more than 1000 lines; and independent review found no issue.

  At that checkpoint the seal had no production constructor, caller, or
  dispatcher. This
  checkpoint performs no persistence, database mutation, trigger, cleanup, or
  additional namespace mutation, and makes no power-loss claim.

  Commit `269aae2c` adds the next test-sealed persistence checkpoint. The
  sealed authority derives its fixed journal outcome from its internal
  candidate-preservation origin; callers cannot supply or alter that outcome.
  It performs complete authority revalidation twice, permits exactly one
  journal advance from `CandidatePreserveIntent` to `CandidatePreserved`, and
  then reopens the canonical journal. That reopen must classify the exact
  source or exact successor record and rejects every other result.

  Persistence-specific authority and projection are functionally split from
  the established post-move durability boundary. The older 6/6 durability gate
  therefore remains intact rather than being relaxed to admit persistence.
  The fresh database row and its provenance are not mutated. A restart from
  the source record reruns the complete idempotent durability suffix but never
  issues a second candidate move; a restart from the exact successor skips
  candidate preservation.

  The dedicated
  `make forge-startup-usr-rollback-candidate-preserve-persistence-test` lane
  passes 9/9 contracts. The established post-move durability lane remains 6/6,
  and the combined authority run remains 56/56. `make fmt` and `make check`
  pass with only the four established warnings; `make source-loc` reports all
  1072 tracked text files at no more than 1000 lines; and independent review
  found no issue.

  Commit `7bc33902` adds that separate route for exact NewState
  `CandidatePreserved` evidence. Admission requires the matching fresh
  transition row, present matching provenance, and the private preserved-
  candidate namespace. Both complete revalidation passes begin with the open-
  journal binding, then observe database, namespace, and database again in
  that exact order. The retained authority derives `rollback_successor(None)`
  exactly once, advances the journal exactly once to
  `FreshDbInvalidationIntent`, and reopens the canonical journal to accept only
  the exact source or exact successor record.

  Commit `0f041afe` keeps the route behind a separate test-only seal. A restart
  from the source retries only this route, while the exact successor skips it.
  No path in this checkpoint removes or changes the fresh row, its provenance,
  or the activation namespace. The dedicated route lane passes 11/11; the
  persistence, post-move durability, and combined authority lanes remain 9/9,
  6/6, and 56/56. `make fmt` and `make check` pass in the repository Nix shell
  with only the four established warnings, `make source-loc` reports all 1083
  tracked text files at no more than 1000 lines, and independent review found
  no issue. Commit `9adc2760` preserves the inventory-gate coverage while
  avoiding the host argument-size limit.

  Commit `20b36768` completes Phase 11A's exact fresh-transition removal
  substrate without widening that route. One exclusive database snapshot
  returns non-`Clone` `Present` or `JointlyAbsent` evidence bound to the source
  `Database` capability. That same snapshot covers the bounded global
  in-flight set and the complete `State`, selections, and metadata provenance;
  cleared, foreign, multiple, asymmetric, malformed, or otherwise
  unobservable evidence fails closed rather than becoming absence.

  Consuming `Present` permits one exact transaction attempt and no retry. It
  rechecks the complete preimage, then deletes provenance, selections, and the
  state row in that order with exact affected-row counts. Commit `7af46ce9`
  tightens the fresh exclusive reconciliation around invocation causality.
  Reported success or a deterministically known committed attempt plus joint
  absence is success. A proven non-start or rollback remains definitely not
  applied even when another writer removes the row before reconciliation.
  Generic uncertain reports, partial or changed restoration, unobservable
  state, and exactly restored ABA evidence are `Ambiguous`; absence alone never
  attributes the deletion to this invocation.

  The dedicated `make forge-exact-fresh-transition-removal-test` lane passes
  15/15 contracts. The adjacent route, candidate-preservation persistence, and
  post-move durability lanes remain 11/11, 9/9, and 6/6. `make fmt` and
  `make check` pass in the repository Nix shell; `make source-loc` reports all
  1091 tracked text files at no more than 1000 lines; and independent review
  returned CLEAN.

  Commit `ab1bfd5e` adds that separately sealed Phase 11B startup recovery
  effect while deliberately withholding every production constructor and
  dispatcher. Admission requires the exact NewState
  `FreshDbInvalidationIntent` rollback plan, a matching per-open journal
  binding, the active-state reservation, and the exact preserved-candidate
  namespace. A general database ownership/provenance observation is accepted
  only between equal source-bound exact observations. Complete admission and
  revalidation then use binding-first database -> namespace -> database
  sandwiches without switching the source database or selected typestate.

  Exact `Present` evidence yields a consuming Apply authority which calls the
  Phase 11A substrate exactly once. Jointly absent evidence yields a disjoint
  Finish authority which calls it zero times. Proved success retains an opaque,
  non-`Clone` authority with private `Applied` origin; Finish retains the same
  authority shape with private `AlreadySatisfied` origin. Definitely-not-
  applied and ambiguous outcomes are fieldless and cannot retry or reach later
  persistence. Every failed one-shot result additionally repeats the retained
  journal, namespace, plan, and installation checks, while the exact substrate
  remains the sole authority over post-attempt database classification.

  The dedicated
  `make forge-startup-usr-rollback-fresh-db-invalidation-effect-test` lane
  passes 12/12 contracts. The exact-removal, route, candidate-preservation
  persistence, post-move durability, and database-adapter lanes pass 15/15,
  11/11, 9/9, 6/6, and 29/29. `make fmt` and `make check` pass in the repository
  Nix shell with only the four established warnings; `make source-loc` reports
  all 1100 tracked text files at no more than 1000 lines; and independent
  review returned CLEAN. Unrelated ambient quarantine wrappers are allowed
  only while retained in the complete stable namespace fingerprint; unsafe or
  conflicting lookalikes fail closed.

  Commit `a15a7bc9` completes that separate Phase 11C persistence checkpoint
  without adding another seal or admission type. The existing non-`Clone`
  effect authority is the sole capability. Persistence-side revalidation starts
  with its per-open journal binding, requires the retained jointly absent
  database typestate, compares two fresh exact paired observations around the
  preserved-candidate namespace proof, and rechecks the exact plan and
  installation. It deliberately never substitutes the historical pre-effect
  database context for the retained post-effect absence.

  The executor performs two complete authority revalidations around one
  authority-owned `rollback_successor(Some(origin))` projection, followed by
  exactly one conditional journal advance. It then destroys the authority and old
  lock-bearing store before descriptor-rooted canonical reopen. Successful
  advance accepts only the exact `FreshDbInvalidated` successor. A reported
  advance failure accepts only the exact source intent or exact successor as
  its durable side; missing, different, or unreopenable records fail closed and
  return no store or reusable capability.

  If the source intent survives, fresh startup observes joint absence, enters
  Finish, makes zero removal calls, and persists `AlreadySatisfied` as this
  invocation's origin even when an earlier invocation applied the deletion. If
  the successor survives, Phase 11B is not applicable and cannot issue a second
  removal. The dedicated persistence lane passes 9/9 across current and
  historical matrices, both origins, all journal fault boundaries, final
  evidence races, and both restart sides. The effect, exact-removal, route,
  candidate-preservation persistence, post-move durability, and database lanes
  remain 12/12, 15/15, 11/11, 9/9, 6/6, and 29/29. `make fmt` and `make check`
  pass with only the four established warnings; `make source-loc` reports all
  1109 tracked text files at or below the 1000-line ceiling; independent review
  returned CLEAN.

  Commit `51a4a348` completes the separate Phase 11D journal-only route from
  `FreshDbInvalidated` to `RollbackComplete`. Its dedicated test-only seal and
  authority are intentionally disjoint from the Phase 11C persistence
  authority. Admission requires the exact NewState rollback plan with every
  ordinary action resolved, boot repair not required, and the preserved
  candidate topology. Generic missing-row and absent-provenance context is
  paired with a non-`Clone`, source-database-bound exact joint-absence proof.
  Each database inspection is itself exact-before -> generic -> exact-after;
  capture and revalidation retain binding-first database -> namespace ->
  database sandwiches.

  The executor performs two complete authority revalidations around the sole
  `rollback_successor(None)` projection, explicitly requires
  `RollbackComplete`, and attempts exactly one conditional journal advance.
  It drops the authority and old store before descriptor-rooted canonical
  reopen, which accepts only the complete source or successor record. A
  source-durable fault retries only Phase 11D on a later startup; a
  successor-durable fault makes both Phase 11B and Phase 11D inapplicable. The
  route never repeats fresh-row removal and performs no database, namespace,
  trigger, cleanup, finalization, delete, retry, or dispatch effect.

  The dedicated completion-route lane passes 11/11 contracts across current
  and historical evidence, both invalidation origins, both forward sources,
  both `/usr` outcomes, both candidate outcomes, all five journal durability
  faults, capture and final evidence races, cross-root stores, three namespace
  lookalikes, canonical reopen, and both restart sides. The Phase 11C, 11B,
  11A, earlier route, candidate-persistence, durability, database-adapter, and
  startup-gate lanes remain 9/9, 12/12, 15/15, 11/11, 9/9, 6/6, 29/29, and
  21/21. `make fmt`, `make check`, and the 1120-file source limit pass; the
  four established warnings remain, and independent review returned CLEAN.
  Commit `a5313099` connects these four exact NewState suffix checkpoints to
  the real startup gate after reverse recovery and before final diagnostics.
  One entry handles only its observed checkpoint and returns immediately,
  including preparation-only creation or normalization which safely retains
  `CandidatePreserveIntent`. Compiler-local seal definitions prevent sibling
  modules from minting effect authority. The 25 real-gate contracts cover both
  epochs and sources, both `/usr` and candidate outcomes, every target prefix,
  present and jointly absent fresh rows, all five journal faults at each of four
  persistence boundaries, effect/evidence/durability failures, fresh handles,
  non-NewState exclusion, and retained `RollbackComplete`. All adjacent lanes,
  the broader startup and reverse-dispatch gates, `make check`, the 1132-file
  limit, and independent review pass. No journal finalizer, later rollback
  effect, other-operation dispatcher, reboot proof, or power-loss proof exists,
  so Phase 11 remains open.

  Commit `6fc94f32` adds exact NewState terminal rollback finalization as a
  separate production startup checkpoint. Its non-`Clone`, phase-specific
  authority recaptures the exact `RollbackComplete` plan, source-database joint
  absence, preserved-candidate namespace, journal binding, and writer
  reservation. The consuming executor retains the same continuously locked
  store, verifies the public journal directory, lock, exact entry set,
  canonical inode, and canonical contents without provisioning or cleanup,
  then makes at most one conditional terminal deletion. Success requires exact
  public absence plus post-delete database -> namespace -> database evidence.
  False deletion reports, storage faults, substitutions, record recreation,
  and ambiguous evidence return typed errors without reusable authority.

  Production dispatch returns a record-free terminal result and transfers that
  same locked store directly into shared clean admission; it cannot reopen or
  redispatch the deleted record. Clean admission freshly rejects orphan rows,
  audits archived-prune residue, and finishes with a public-aware absence read
  through the retained store bracketed by mutable-namespace validation. A
  valid record recreated during this bounded handoff is preserved and rejected,
  not admitted as clean. The dedicated gates pass 5 authority, 13 executor,
  5 clean-handoff, and 33 complete NewState-startup contracts. `make check` and
  the 1153-file source limit pass with only the four established unrelated
  warnings, and independent adversarial review found no blocker. At that
  checkpoint, no terminal finalizer existed for ActivateArchived or
  ActiveReblit, and terminal process-death, reboot, and power-loss proof
  remained open.

  ActiveReblit terminal finalization is now a separate deterministic
  production checkpoint rather than an extension of the NewState finalizer.
  Admission requires an exact ActiveReblit `RollbackComplete` plan with
  `candidate == previous`, an `ExistingCandidate` database observation for
  that state under `Cleared` transition ownership, present matching immutable
  provenance, `previous: None`, and no global in-flight transition.
  Binding-first database -> namespace -> database capture and revalidation
  retain the source database, active-state reservation, journal binding, and
  exact preserved whole-wrapper topology. The replacement-wrapper index is
  part of that topology and must remain identical across the retained before
  and after snapshots and every fresh capture.

  The consuming finalizer keeps the original public journal directory and
  lock-bearing store continuously retained. It authenticates the exact public
  source, repeats final authority and public-name validation, and makes exactly
  one conditional terminal delete. It neither reopens nor retries. Deletion is
  reconciled only as the exact source record or authenticated public absence;
  success requires the latter to remain stable across the consuming
  post-delete database -> namespace -> database proof and a second public
  observation. The same still-locked store then enters the operation-neutral
  clean-startup handoff, where mutable namespace and database audits precede
  the shared orphan-transition, prune-residue, and final public-absence gate.
  No database row, provenance, wrapper, non-journal namespace, trigger,
  cleanup, or other finalization effect is reachable from this checkpoint.

  Deterministic terminal-delete faults and fresh-handle restart cover both
  observed source-or-absence sides without treating absence as deletion
  causality. A separate real-process checkpoint now covers an exact 2 x 2 x 3
  matrix: current and historical record epochs, both rollback sources, and
  final-PRE, post-unlink, and post-directory-sync kill boundaries. Those 12
  cases use genuine same-boot `SIGKILL` followed by fresh-process production
  startup. Historical epoch means only that the journal witness differs from
  the running epoch; it is not a reboot simulation, and no reboot or power-loss
  durability is claimed.

## Diagnostic reconciliation and namespace inventory

  When exact production finalization does not apply or defers, the assessment
  classifies every validated persisted phase as begin rollback, resume rollback,
  roll forward, finalize rollback, or manual
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

  Commits `932ab3bb` and `0e56aff3` extend that real-process method to NewState
  terminal deletion. Test-only one-shot seams fire immediately after successful
  canonical unlink and journal-directory fsync; the established final-PRE hook
  supplies the exact source-survives boundary. Current and historical epochs,
  both rollback sources, and all three boundaries form 12 genuine `SIGKILL`
  crash/recovery cases through `CleanSystemStartup`. The parent seeds an
  anchored persistent database with exact joint fresh-state absence, records
  public journal identities and bytes, and releases every fixture handle before
  either child opens fresh installation and database capabilities.

  Final-PRE death preserves the exact terminal inode and bytes. Post-unlink and
  post-fsync death expose lock-only public inventory, while `.cast`, journal,
  and lock identities remain stable. Fresh recovery repeats no update,
  candidate, or database effect; it admits clean startup, retains the journal
  lock until clean authority drops, and finally reopens exact absence. Database
  rows and semantic non-journal namespace remain unchanged throughout. Both
  focused gates, `make check`, and the 1155-file source limit pass. This is
  same-boot process-death evidence: `SIGKILL` does not prove which pre-fsync
  state survives reboot or power loss.

  The ActiveReblit terminal lane applies the same real-process method to an
  exact 12-case matrix. Its four epoch/source rows deliberately bind the other
  recovery dimensions rather than leaving them incidental:

  - current + `UsrExchangeIntent`: `/usr` `Applied`, candidate `Applied`,
    preserved-wrapper index 0;
  - current + `UsrExchanged`: `/usr` `Applied`, candidate
    `AlreadySatisfied`, preserved-wrapper index 13;
  - historical + `UsrExchangeIntent`: `/usr` `AlreadySatisfied`, candidate
    `Applied`, preserved-wrapper index 13; and
  - historical + `UsrExchanged`: `/usr` `AlreadySatisfied`, candidate
    `AlreadySatisfied`, preserved-wrapper index 0.

  Each row crosses three terminal-delete boundaries. Final-PRE death leaves
  the canonical `RollbackComplete` source byte- and identity-exact. Death at
  `CanonicalUnlinked` proves only that the running kernel observes the
  canonical name absent immediately after unlink, before directory-sync
  durability. Death at `DeleteDirectorySynced` observes absence after the
  journal-directory sync. At both absence boundaries, recovery does not infer
  that its own invocation caused deletion; it authenticates the public
  lock-only inventory and enters shared clean admission through the retained
  store. Fresh crash and recovery processes reopen the installation and source
  database, and production `CleanSystemStartup` preserves the exact
  `ExistingCandidate`/`Cleared` row, immutable provenance, non-journal
  namespace, whole-wrapper topology, and selected wrapper index.

  This remains same-boot process-death evidence. The historical row uses a
  deliberately out-of-current-epoch record under the same kernel boot; it is
  not a reboot simulation. Even the post-directory-sync observation is not
  presented as a power-loss oracle, so no reboot or power-loss durability
  claim follows from this matrix.

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

  The production ladder now covers the authenticated `/usr` rollback prefix,
  the exact NewState suffix from `CandidatePreserveIntent` through authenticated
  terminal journal absence, and the ActiveReblit suffix through authenticated
  terminal journal absence and shared clean admission. Separate NewState
  entries may create or normalize
  the quarantine target without advancing, move and durably preserve the
  candidate, invalidate the exact fresh transition or accept proved joint
  absence, route to completion, and delete the exact terminal record. Separate
  ActiveReblit entries preserve the whole replacement wrapper and then perform
  the direct journal route from `CandidatePreserved` to `RollbackComplete`;
  only a further entry may authenticate and delete that terminal record.
  Every entry handles at most its observed checkpoint and immediately returns;
  no resulting record is redispatched in the same entry.

  The focused ActiveReblit completion lane adds six real-startup contracts and
  one direct authority-binding proof: a 16-case
  epoch/source/`/usr`/candidate-outcome matrix, all five conditional
  journal-update faults with second-entry convergence, both fresh-handle
  durability sides, database/provenance/journal/namespace races, exact
  operation/phase/plan/topology exclusions, and rejection of separately
  reopened or cross-root journal bindings. The combined ActiveReblit startup
  lane passes 17/17 contracts. Completion repeats no wrapper exchange or
  candidate durability effect and changes neither the database nor the
  non-journal namespace.

  Commit `cbe3679a` production-wires exactly one ActivateArchived
  `CandidatePreserveIntent` checkpoint per startup entry. Exact staged evidence
  may move only `staging/usr` once into the authenticated archived wrapper;
  exact already-preserved evidence takes the idempotent Finish path with no
  second move. Ordered durability then permits one conditional advance to the
  sole `CandidatePreserved` successor, destroys the old authority and journal
  handle, and accepts only the exact source or successor after canonical
  reopen. A handled checkpoint immediately returns `RecoveryPending`, so the
  sealed completion foundation cannot run in the same entry.

  The production lane passes 11 persistence/shared-leaf tests and 10
  candidate-filter tests across current and historical epochs, both rollback
  sources, both recorded `/usr` outcomes, Apply and Finish, all five journal
  faults, six evidence races, and both fresh-handle restart sides. Updated
  sibling-dispatch and reverse-`SIGKILL` contracts prove one operation owns the
  checkpoint and no restart performs a second move or same-entry completion.
  ActiveReblit retains deterministic terminal finalization, authenticated clean
  handoff, and its exact 12-case same-boot terminal `SIGKILL` matrix.
  Commit `c8c5ea41` production-wires ActivateArchived completion as a separate
  bounded entry. Commit `32bf8589` adds its operation-specific terminal
  finalizer: exact cleared candidate and previous rows with candidate
  provenance are retained through binding-first database -> namespace ->
  database proof, two final authority and public-source checks, one same-store
  conditional delete, repeated authenticated absence, and same-lock clean
  admission. Its 16 startup contracts and four executor truth-table contracts
  pass; no real-process interruption, reboot, or power-loss claim is made by
  that checkpoint. The matching terminal `SIGKILL` matrix remains next, and
  the ladder also has no roll-forward executor, boot repair, or cleanup.
  The exact reverse prefix has deterministic contracts and genuine
  process-termination coverage. The NewState suffix adds deterministic
  real-startup matrices, all five journal durability faults across each of four
  persistence boundaries, deterministic terminal-delete faults, and 12 real
  terminal process-death cases, but not process death at every earlier suffix
  effect. ActiveReblit now adds terminal-delete fault injection, fresh-handle
  restart, and 12 genuine same-boot process-death cases. Neither suffix has
  reboot or power-loss proof: `SIGKILL` preserves the
  kernel-visible state at termination and cannot establish which pre-fsync
  rename survives a power cycle. The complete campaign required below
  therefore remains open, as do this item and all six broad Phase 11 work items.
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
  journal-update-boundary `SIGKILL` cases with fresh-process reopen. NewState
  and ActiveReblit terminal deletion each add an exact 12-case fresh-process
  matrix. This item remains unchecked because the other phases and true
  power-loss-equivalent durability outcomes are not yet covered.
