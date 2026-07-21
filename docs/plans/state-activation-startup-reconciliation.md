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

  As of 2026-07-21, startup's diagnostic checkpoint remains read-only and fail closed.
  Immediately before it, the mutable gate has one sealed, bounded ladder: ActiveReblit replacement-mode repair, forward exchange-parent durability, exact `UsrExchanged` root-ABI normalization, rollback-decision persistence and routing, and `/usr` reversal.
  Exact RootLinks-sourced `UsrRestored` routes through candidate preservation. NewState advances generation 15 through 16 `FreshDbInvalidationIntent` and 17 `FreshDbInvalidated` to 18 `RollbackComplete`; ActiveReblit advances generation 13 to 14 `RollbackComplete`; both then finalize cleanly.
  ActivateArchived likewise advances generation 11 to generation-12 `RollbackComplete`; exact RootLinks NewState generation 18, ActivateArchived generation 12, and ActiveReblit generation 14 each finalize through one record-bound deletion and the same locked-store clean handoff, while legacy `UsrExchangeIntent` and `UsrExchanged` admission remains intact.
  Accepted commit `39456719` supersedes the fresh-handle-only checkpoint with a separate 36-case RootLinks terminal campaign: 84 child executions yield 48 genuine same-boot `SIGKILL` deaths and 36 successful final recoveries, while the legacy Intent/Exchanged 12-case matrices remain unchanged; this is not reboot or power-loss proof.
  For the pre-existing later source set, NewState preserves its candidate, invalidates the exact fresh row, completes, and authenticates journal absence. ActivateArchived completes and finalizes in separate entries.
  ActiveReblit preserves its whole replacement wrapper; exact cleared ownership and provenance can route the no-boot case through `RollbackComplete` and later same-lock finalization.
  `BootSyncStarted` instead routes only to `BootRepairRequired`; observed
  `BootRepairStarted` becomes terminal `BootRepairUnverified` without invoking
  boot. Commit `ffc32ce1` routes durable `BootRepairComplete` to
  `RollbackComplete`, but no production entry performs or records that repair.
  Every entry recaptures fresh authority, crosses at most one effect, journal
  advance, or deletion boundary, and never redispatches its successor;
  unsupported or inexact evidence stays diagnostic.

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

  Commit `72511b3` added the consuming exchange-parent durability path. After
  checking its per-open binding, it syncs retained `.cast/root/staging` and the
  retained installation root, never reopening either parent or exposing a
  rename, exchange, reverse, database, trigger, cleanup, or root-link effect.
  Both barriers precede a full evidence recapture and private conversion to
  rollback-decision authority. A fault consumes the authority and leaves exact
  `UsrExchangeIntent` for a fresh idempotent durability entry, never a retry.

  Commit `035d0843` inserts a separate root-ABI boundary for exact forward
  `UsrExchanged` before rollback-decision capture. Across all three operations,
  every one of the 32 subsets of `bin`, `sbin`, `lib`, `lib32`, and `lib64`
  admits at most one retained publisher invocation in an entry. An incomplete
  set remains at the exact source and returns `RecoveryPending`; a publisher
  error is possibly applied, returns no retry authority, and requires fresh
  reconciliation. A set already complete at entry always syncs the retained
  installation root before decision evidence is captured again from scratch.
  The authority authenticates exact public `.cast`, journal-directory, lock, and record identities, retaining the admitted record inode through `Arc<File>`.
  Its bounded retained inventory of every noncanonical root entry detects regular-file, symlink, and installation-root replacement races.
  This startup normalizer still cannot emit `RootLinksComplete`; canonical links stay complete through rollback. Commit `04911701` proves one entry for an intent source versus two for an initially incomplete exchanged source; complete-at-entry exchanged evidence needs one.
  Independently reviewed commit `03c5fd13` lands the production in-process `UsrExchanged` -> `RootLinksComplete` transition, with 97/97 coordinator, 15/15 focused publication, and 19/19 normalizer tests passing.
  Commit `a4f16351` admits exact durable `RootLinksComplete` + `POST` during startup for all three operations only when all five canonical links are already exact; incomplete or `PRE` evidence remains non-mutating, and the entry invokes neither root-ABI publication nor complete-set synchronization.
  Before namespace and database evidence, the decision authority captures the exact canonical predecessor binding: per-open store identity, complete record value, and retained inode.
  Revalidation checks store identity first and consumes the non-Clone binding through one conditional `advance_record_binding`, deriving exactly one `RollbackDecided` with source `RootLinksComplete` and pending `/usr` without a namespace, database, trigger, rollback, or retry effect.
  After the advance it authenticates the exact successor binding, drops the old lock-bearing store, and independently reopens the canonical journal.
  Same-byte predecessor or successor inode replacement never becomes success; reopen classifies only the exact durable source or decision.
  The handled entry returns `RecoveryPending` and never redispatches its successor. Commit `2201a24b` admits only the resulting exact RootLinks decision through the journal-only resume route; commit `66e3cf6b` retains each non-Clone successor binding across old-store destruction and requires an independent canonical reopen to authenticate that exact inode and record inside an installation-revalidation sandwich.
  Commit `1b34d718` carries that exact non-Clone record binding through reverse admission, one reconciled effect, ordered parent durability, and bound persistence. Its durable authority privately seals `Applied` after one exchange or `AlreadySatisfied` from exact `PRE`, validates the successor in the same store and across canonical reopen, and never accepts an outcome from its caller.
  Fresh entries now move RootLinks exactly through `RollbackDecided` -> `ReverseExchangeIntent` -> `UsrRestored`; the reverse entry exchanges once and a later entry leaves the restored record byte-identical. Operation/epoch/outcome, five-fault, and same-byte replacement matrices converge without a second effect while all five canonical root links retain their exact targets and identities.
  Commit `7b3770b1` captures the exact non-Clone `TransitionJournalRecordBinding` before namespace/database evidence and moves it through NewState create/normalize/move, ActivateArchived, ActiveReblit, common effect/durability/persistence-facing authority, and dispatch; all six coarse semantic loads are eliminated, and `RestartRequired` now carries an opaque one-use unchanged-source authority. Its original identical-bytes/different-inode matrix was 44 pre-effect + 44 post-effect + 16 restart cases across current/historical epochs and both `/usr` outcomes, with `BootSyncStarted` only for ActiveReblit.
  Commits `fec890ad`, `c9140a88`, and `043a3c24` complete exact candidate persistence for NewState, ActivateArchived, and ActiveReblit respectively. Each consuming writer derives its sole `CandidatePreserved` successor from private operation evidence, validates the exact successor binding in the same store, destroys the old lock-bearing store, and requires an independent canonical reopen inside a final installation-revalidation sandwich. Covered publication faults, same-byte/different-inode replacement seams, and fresh restarts fail closed without extra database, non-journal namespace, or redispatch effects.
  Commit `67ad3de0` widens only the exact RootLinks source passage: current/historical epochs, all three operations, and both `/usr` outcomes now follow `UsrRestored` -> `CandidatePreserveIntent` -> `CandidatePreserved`, with one reverse exchange and all five root-link identities unchanged. Route mutation coverage is 360 cases across two seams; admission rejects another 360 races spanning all five links. Common binding totals become 64 pre-effect, 64 post-effect, and 24 restart cases; NewState and ActivateArchived writer totals are 24/120/96/48 for success/storage fault/binding substitution/restart, and ActiveReblit totals are 32/160/128/64. Accepted commit `e35a2183` then carries exact RootLinks NewState generation-15 `CandidatePreserved` through the bound non-Clone record-inode advance to generation-16 `FreshDbInvalidationIntent`, same-store validation, old-store destruction, and canonical reopen. Accepted commit `7457b259` continues only that exact source through capture, Apply-or-Finish effect reconciliation, the bound generation-17 `FreshDbInvalidated` advance, same-store validation, and independent reopen. Present evidence permits at most one exact removal; proved joint absence performs zero. Its success/storage/binding/fresh-handle matrices are 48/240/192/96, and its all-five-link races are 240 capture, 240 pre-effect, 120 Applied post-attempt, 240 initial-persistence, and 240 final-revalidation executions. Accepted commit `68759ba3` proves genuine same-boot `SIGKILL` recovery only for this RootLinks NewState generation-16 -> generation-17 boundary: exactly 20 cases = two epochs x (five SQLite application-transaction seams + five journal-update durability seams). The parent releases all installation, journal, and database handles; crash and recovery children re-execute production `CleanSystemStartup` under 15-second kill-and-reap deadlines, and recovery is the first SQLite opener. A nonempty selected row proves real cascade deletion. The first four database seams roll back, then recovery performs one exact `Applied` removal; post-commit and journal paths perform zero removals and converge through exact `AlreadySatisfied` or source-versus-successor evidence. Post-crash raw temporary inventory precedes any recovery store open, all five root-link identities remain exact, and unrelated effects stay zero. Accepted commit `f2b305d4` separately admits exact RootLinks NewState generation-17 `FreshDbInvalidated`, captures its non-Clone record-inode binding, consumes it through one bound advance to generation-18 `RollbackComplete`, validates the successor in the same store, drops the old lock-bearing store, and independently reopens the canonical journal to match that same successor inode and record. Its base-success/storage-fault/binding-substitution/fresh-handle matrices cover 48/240/192/96 executions; fresh-handle reopen is not process death. Its 480 all-five-root-ABI races split into 240 capture and 240 final-revalidation cases. This route is journal-only and invokes no database, non-journal namespace, reverse-exchange, candidate, boot, cleanup, terminal-delete, or finalization effect. Accepted commit `a3fb25d3` independently admits exact RootLinks ActivateArchived generation-11 `CandidatePreserved` and carries its exact record-inode binding from capture through one bound advance to generation-12 `RollbackComplete`, same-store validation, and independent reopen. Accepted commit `a05997d8`, with acceptance-gate follow-up `cfb5a70d`, does the same for RootLinks ActiveReblit generation 13 to generation-14 `RollbackComplete`. The full endpoint performs exactly one reverse `/usr` exchange and preserves all five root-link identities; ActiveReblit also performs its one wrapper exchange. `BootSyncStarted` remains disjoint and routes to `BootRepairRequired`. At that completion-route checkpoint, NewState was byte-stable at generation 18, ActivateArchived at generation 12, and ActiveReblit at generation 14 because RootLinks terminal finalization remained closed. The 20-case `SIGKILL` claim remains only generation 16 -> 17; if its invalidation successor was already canonical, the recovery entry may naturally take generation 17 -> 18 without proving a completion-route death boundary. It is not reboot or power-loss proof. The next blocker was an exact record-bound terminal-deletion primitive before operation finalization.
  Accepted commit `8f391985` closes the bound-delete store blocker: its exact same-store non-`Clone` binding primitive detaches with `RENAME_NOREPLACE`, authenticates the retained inode/frame, unlinks once, syncs once, and preserves foreign winners. Accepted commit `0a91c2ed` adds writer-reopen recovery after exact public journal-directory and lock authentication: exactly one canonical-form owner-private, single-link, mode-`0600`, valid-terminal `.state-transition.delete-*` is retained by complete bounded frame and inode, the exact inventory is double-observed, and it is restored once to canonical with `RENAME_NOREPLACE` plus one directory sync. Ambiguous restore/sync reports reconcile without retry; foreign, malformed, unsafe, corrupt, nonterminal, multiple, or canonical-coexisting evidence fails closed; read-only still rejects residue; and the cooperative same-credential limitation remains. Its gates pass 13/13 focused recovery, 10/10 bound-delete, and 110/110 direct-journal tests. Accepted commits `a0966008`, `b0af65d6`, and `806003ac` consume bound deletion only for exact RootLinks ActivateArchived generation 12, NewState generation 18, and ActiveReblit generation 14 while preserving legacy Intent/Exchanged admission. The ActiveReblit path captures mandatory non-`Clone` binding before database/namespace evidence, performs exactly one bound delete, and keeps the same locked store through exact `ExistingCandidate`/`Cleared` provenance, whole-wrapper/index, all-five-link, two-public-absence, and clean-handoff proof. Every delete error, including authenticated `Absent`, fails. Its proof covers 24 focused tests, a 24-case success matrix, 15 link races, fresh handles, and clean-then-clean endpoint. Accepted commit `39456719` adds the separate RootLinks-only terminal process campaign: 3 operations x 2 current/historical epochs x 6 scenarios = 36 cases and 84 child executions, comprising 48 genuine same-boot `SIGKILL` deaths and 36 successful final recoveries. The six seams are final PRE, private detach before private unlink, post-private-unlink, post-delete-directory-sync, recovery after canonical restore, and recovery after restore-directory-sync. Before any child opens a writer, journal store, or database, raw inventory authenticates exact Cast/journal/lock anchors and exact absence or, when present, the canonical/private record name, inode, complete frame, mode, and link count. Death callbacks assert zero operation-specific effect attempts; every child uses only production `CleanSystemStartup`, with internal 15-second deadlines restricted to those children. Final recovery proves all five links, exact operation-specific database/topology, public absence, clean, then clean again. The unchanged legacy 12-case matrices remain Intent/Exchanged-only. This proves same-boot process death, not reboot or power loss.
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

  Commit `c7c97d4c` originally reused that same sealed authority for one additional exact
  source: `UsrRestored` whose recorded forward rollback source was
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
  non-Clone record binding, cooperating-writer reservation, complete source record,
  stable database ownership and provenance, and descriptor-rooted namespace
  proof. Apply makes exactly one retained descriptor-relative exchange attempt
  and recaptures the layout rather than trusting the raw syscall report. An
  applied layout continues even if the syscall reported an error; a semantic
  non-application or ambiguous layout terminates that startup entry and returns
  no reusable effect or journal authority. Finish makes no exchange attempt.

  Both successful paths complete staging-parent and installation-root
  durability in that order and revalidate all evidence before the resulting
  private authority seals the sole legal outcome. An exchange applied by this
  entry seals `Applied`; exact PRE seals `AlreadySatisfied`, with no caller
  override. Persistence consumes the non-Clone source binding in one bound
  advance, validates the successor binding in the same store, retains it after
  destroying the old store, and authenticates the exact reopened inode and
  record inside an installation sandwich. A storage error remains an error and
  never authorizes an in-process retry or later rollback action.

  The one-recovery-journal-mutation-per-entry rule therefore remains intact.
  One entry may persist `RollbackDecided`, a later one may persist
  `ReverseExchangeIntent`, and a later one may perform the admitted reverse and
  persist `UsrRestored`. Because the journal-only route ran earlier in that
  startup entry, the reverse entry stops there and returns `RecoveryPending`.
  For every admitted source, including RootLinks, one fresh later entry may route exact `UsrRestored` to
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

  The current admission matrix covers NewState, ActivateArchived, and
  ActiveReblit; rollback sources `UsrExchangeIntent`, `UsrExchanged`, and `RootLinksComplete`;
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
  namespace, or capture result as `Ambiguous`. At that historical checkpoint,
  `RestartRequired` described the safe observed prefix rather than proving
  which actor created it, and all three results were fieldless. None retained a
  descriptor, retry, normalization, or move capability. Database, journal,
  installation, and plan evidence was checked again after the attempt, so even
  a safe prepared target required a fresh startup entry.

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

  At that historical checkpoint the externally observable normalization
  results were the fieldless `RestartRequired`, `NotApplied`, and `Ambiguous`.
  None retained evidence, descriptors, a retry, movement, or persistence
  capability. Even fully normalized and synchronized evidence therefore forced
  a new startup entry; this checkpoint could not fall through into movement.

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

  At that historical checkpoint, commit `0f041afe` kept the route test-only. Accepted commit `e35a2183` carries exact RootLinks NewState generation-15
  `CandidatePreserved` through the bound record-inode advance to generation-16 `FreshDbInvalidationIntent`, same-store validation, and canonical reopen.
  Accepted commit `7457b259` then carries that exact binding through capture, Apply-or-Finish reconciliation, a bound advance to generation-17
  `FreshDbInvalidated`, same-store successor validation, and independent reopen. Present evidence permits at most one exact fresh-row removal; proved joint
  absence performs zero. Success/storage-fault/binding-substitution/fresh-handle matrices cover 48/240/192/96 executions. Accepted commit `68759ba3` adds
  exact RootLinks-only genuine same-boot process-death proof: 20 cases = two epochs x (five SQLite transaction seams + five journal-update seams).
  After all parent handles are released, crash and recovery children re-execute production `CleanSystemStartup` under 15-second deadlines; recovery is the first database opener. A nonempty selection proves real deletion. The first four SQLite deaths roll back and require one `Applied` recovery removal;
  post-commit and journal deaths remove zero, preserve exact `AlreadySatisfied` or source-versus-successor handling, inspect raw temporaries before any recovery store open, retain all five root-link identities, and perform no unrelated effects. Accepted commit `f2b305d4` separately captures exact generation-17 `FreshDbInvalidated` with its non-Clone record-inode binding, consumes one bound advance, validates generation-18 `RollbackComplete` in the same store, drops the old store, and independently reopens the same successor inode and record. Its base-success/storage/binding/fresh-handle totals are 48/240/192/96, and 480 root-ABI races split across capture/final revalidation; the journal-only route leaves database, non-journal namespace, reverse, candidate, boot, cleanup, deletion, and finalization effects at zero. At that completion-only checkpoint, generation 18 remained stable because RootLinks finalization stayed closed.
  Accepted commit `8f391985` supplies exact record-bound store deletion; `a0966008`, `b0af65d6`, and `806003ac` consume it only for exact RootLinks ActivateArchived generation 12, NewState generation 18, and ActiveReblit generation 14 while preserving Intent/Exchanged. ActiveReblit retains mandatory binding-first authority, one bound delete, and the same locked store through exact post-delete database/topology/index/all-five-link/two-public-absence proof and clean handoff. Accepted commit `0a91c2ed` restores one exactly authenticated terminal private-detach residue on writer reopen, and accepted commit `39456719` now proves all three RootLinks finalizers and that restoration path across the exact separate same-boot process campaign above without widening reboot or power-loss claims.
  Commit `9adc2760` preserves the inventory-gate coverage while avoiding the host argument-size limit.

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

  Accepted commit `806003ac` makes ActiveReblit terminal finalization a separate
  deterministic production checkpoint rather than an extension of another
  operation's finalizer. RootLinks admission is restricted to exact
  `RollbackComplete` generation 14 while the legacy `UsrExchangeIntent` and
  `UsrExchanged` sources remain intact. Mandatory binding-first authority
  captures the exact non-`Clone` source record before database or namespace
  evidence. Admission requires `candidate == previous`, exact
  `ExistingCandidate` database evidence under `Cleared` transition ownership,
  matching immutable provenance, `previous: None`, no global in-flight
  transition, the preserved whole-wrapper topology and replacement-wrapper
  index, and unchanged identities for all five root links.

  The consuming finalizer retains the original public journal directory and
  lock-bearing store continuously. It revalidates and consumes that exact
  binding through exactly one `delete_record_binding`; every delete error,
  including authenticated `Absent`, fails rather than becoming success. The
  same locked store crosses post-delete database -> namespace -> database
  proof, two exact public-absence observations, and the shared clean-startup
  handoff. Mutable namespace and database audits precede the shared orphan-row,
  prune-residue, and final absence gate. No database row, provenance, wrapper,
  non-journal namespace, trigger, cleanup, or other effect is changed.

  Coverage includes 24 focused contracts, the full 24-case success matrix, 15 all-five-link races, deterministic delete/storage failures, same-bytes/different-inode and public-binding rejection, fresh handles, and clean then clean again.
  The unchanged 12-case process matrices span current/historical epochs, both legacy sources, and final-PRE, post-unlink, and post-directory-sync deaths; RootLinks remains excluded from those legacy matrices.
  Accepted commit `39456719` supplies its separate RootLinks campaign across all three exact terminal generations: 3 x 2 x 6 = 36 cases, 84 child executions, 48 genuine same-boot `SIGKILL` deaths, and 36 final recoveries over final PRE, pre-private-unlink detached residue, post-private-unlink, post-delete-directory-sync, post-canonical-restore recovery, and post-restore-directory-sync recovery.
  Before any child opens a writer or database, raw inventory proves exact Cast/journal/lock anchors and exact absence or, when present, the canonical/private name, inode, complete frame, mode, and link count; every death callback first proves zero operation-specific effect attempts, and the children invoke only production `CleanSystemStartup` under internal child-only 15-second deadlines.
  Each final recovery retains all five root links and exact operation-specific database/topology, reaches public absence and clean admission, then remains clean. These are same-boot kills, not reboot or power-loss durability. `BootSyncStarted` remains disjoint and routes unchanged to `BootRepairRequired`.

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
  both legacy rollback sources, and all three boundaries form 12 genuine `SIGKILL`
  crash/recovery cases through `CleanSystemStartup`. The parent seeds an
  anchored persistent database with exact joint fresh-state absence, records
  public journal identities and bytes, and releases every fixture handle before
  either child opens fresh installation and database capabilities.
  RootLinks is excluded from this unchanged matrix.

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

  Commit `f44c2be9` adds the first genuine ActivateArchived process-death
  proof inside candidate preservation. Its exact 2 x 2 matrix crosses current
  and historical record epochs with both rollback sources at one production
  boundary: the real no-replace `staging/usr` move has returned, but fresh
  semantic recapture, every POST durability barrier, and journal advancement
  have not begun. Each crash child reaches that seam through
  `CleanSystemStartup`, proves exactly one real move attempt, and dies by
  `SIGKILL`. The parent then finds the canonical `CandidatePreserveIntent`
  source journal byte- and identity-exact while the candidate tree is already
  preserved in its archived wrapper.

  A fresh recovery process opens new installation and database handles, enters
  production startup, selects Finish with zero second move, completes the POST
  durability suffix, and persists `CandidatePreserved(AlreadySatisfied)`. That
  entry returns only `RecoveryPending`; it cannot run completion or
  finalization. This is same-boot evidence only. The historical row is not a
  reboot, and death before POST durability cannot prove which move survives
  power loss.

  Commit `bc6d6792` expands that first proof into an exact 2 x 2 x 7 matrix.
  Additional crash children die before candidate-tree sync, each retained parent sync, final POST capture, and pre-persistence durable POST revalidation.
  Every seam first observes one move attempt; the parent proves the preserved tree and exact source record, database, bytes, and inode identities.
  A fresh recovery child selects Finish, makes zero second moves, repeats the idempotent durability suffix, and persists exact `CandidatePreserved(AlreadySatisfied)`.
  The 15-second deadlines kill and reap a hung child. This remains same-boot proof, not reboot or power-loss evidence.

  Commit `19f60c51` applies the same exact 2 x 2 x 7 process-death matrix to NewState candidate preservation.
  Current and historical epochs cross both rollback sources and seven seams from post-move recapture through final durable revalidation.
  Each production-startup child performs exactly one real no-replace move, retains the source record, and dies by genuine `SIGKILL`.
  The parent proves exact database, non-journal namespace, tree bytes, topology, and inode identities.
  Fresh recovery reopens installation and database capabilities, selects Finish, makes zero second moves, repeats durability, and persists
  exact `CandidatePreserved(AlreadySatisfied)`. Bounded deadlines kill and reap every child. Historical epochs remain same-boot records;
  they are not reboot simulations or power-loss durability evidence.

## Remaining recovery campaign

  The production ladder covers the authenticated `/usr` rollback prefix and exact RootLinks rollback for all three operations through candidate preservation and their admitted later boundaries. NewState reaches generation-18 `RollbackComplete`, ActivateArchived reaches generation 12, and ActiveReblit reaches generation 14; each finalizes in its own later bounded entry.
  Accepted commits `7457b259`, `68759ba3`, and `f2b305d4` carry RootLinks NewState from generation 16 through exact invalidation to generation 18; only the generation-16 -> 17 boundary has its 20-case same-boot `SIGKILL` proof. Commits `a3fb25d3`, `a05997d8`, and `cfb5a70d` carry ActivateArchived to generation 12 and ActiveReblit to generation 14. The endpoint performs one reverse `/usr` exchange and preserves every root-link identity; ActiveReblit also exchanges its wrapper once.
  Accepted commit `8f391985` supplies exact record-bound deletion. Accepted commits `a0966008`, `b0af65d6`, and `806003ac` consume it only for exact RootLinks ActivateArchived generation 12, NewState generation 18, and ActiveReblit generation 14 respectively, preserving legacy Intent/Exchanged admission and the same locked-store post-delete database/topology/five-link/two-public-absence proof and clean handoff. Accepted commit `0a91c2ed` adds exact terminal private-detach restoration on writer reopen; accepted commit `39456719` proves all three finalizers and that restoration path through the separate 36-case same-boot RootLinks process campaign while leaving legacy matrices unchanged and making no reboot or power-loss claim. The earlier source set retains the exact operation-specific suffixes through authenticated terminal absence.
  Separate NewState entries preserve the candidate, invalidate the exact fresh transition or accept proved joint absence, route to completion, and delete the terminal record.
  Separate no-boot ActiveReblit entries preserve the whole replacement wrapper and route `CandidatePreserved` to `RollbackComplete`;
  only a further entry may authenticate and delete that terminal record. Every entry handles only its observed checkpoint,
  returns immediately, and never redispatches the resulting record.

  The focused no-boot ActiveReblit completion lane adds six real-startup contracts and
  one direct authority-binding proof: a 16-case
  epoch/source/`/usr`/candidate-outcome matrix, all five conditional
  journal-update faults with second-entry convergence, both fresh-handle
  durability sides, database/provenance/journal/namespace races, exact
  operation/phase/plan/topology exclusions, and rejection of separately
  reopened or cross-root journal bindings. The combined ActiveReblit startup
  lane passes 17/17 contracts. Completion repeats no wrapper exchange or
  candidate durability effect and changes neither the database nor the
  non-journal namespace.

  Commit `92fa7aa0` routes exact ActiveReblit `CandidatePreserved` evidence sourced from `BootSyncStarted` once to
  `BootRepairRequired`, reopens only the exact source or successor, invokes boot zero times, and returns. Journal v3 carries
  the compact immutable receipt pair. A complete bounded canonical body separately binds transition/predecessor hashes,
  desired inventory, exact destinations, and every ordered output with a keyed inert claim. One exclusive SQLite transaction
  inserts that immutable body and stages its pending singleton head with strict body/head validation. Startup still correlates
  only the compact journal/head pair; production forward staging and full-body startup consumption remain unwired. Existing
  v1/v2 records at `BootSyncStarted` retain their conservative journal-only route. Commit `b5928340` separately advances exact
  `BootRepairStarted` evidence to terminal `BootRepairUnverified` with zero boot calls. Commit `406cabe5`'s explicit Required ->
  Started, Started -> Complete/Unverified, and Complete -> `RollbackComplete` edges remain. No production entry performs the
  repair or emits success; authenticated claim derivation, exact durable predecessor binding, promotion, publisher, boot
  mutation/deletion authority, and disposable-VM evidence remain open.

  Commit `cbe3679a` production-wires exactly one ActivateArchived
  `CandidatePreserveIntent` checkpoint per startup entry. Exact staged evidence
  may move only `staging/usr` once into the authenticated archived wrapper;
  exact already-preserved evidence takes the idempotent Finish path with no
  second move. Ordered durability then permits one conditional advance to the
  sole `CandidatePreserved` successor, destroys the old authority and journal
  handle, and accepts only the exact source or successor after canonical
  reopen. A handled checkpoint immediately returns `RecoveryPending`, so the
  sealed completion foundation cannot run in the same entry.

  The production lane passes 11 persistence/shared-leaf tests and 11
  candidate-filter tests across current and historical epochs, both rollback
  sources, both recorded `/usr` outcomes, Apply and Finish, all five journal
  faults, six evidence races, and both fresh-handle restart sides. Updated
  sibling-dispatch and reverse-`SIGKILL` contracts prove one operation owns the
  checkpoint and no restart performs a second move or same-entry completion.
  ActiveReblit retains deterministic RootLinks terminal finalization and clean
  handoff; its exact 12-case same-boot terminal `SIGKILL` matrix remains legacy-only.
  Commit `c8c5ea41` production-wires ActivateArchived completion as a separate
  bounded entry. Accepted commit `a3fb25d3` extends only that entry to exact RootLinks generation-11 `CandidatePreserved`, carrying its bound record to generation-12 `RollbackComplete`. Commit `32bf8589` adds its operation-specific finalizer for legacy Intent and Exchanged sources: exact cleared candidate and previous rows with candidate provenance survive binding-first database -> namespace -> database proof, one same-store deletion, repeated absence, and same-lock clean admission.
  Commit `c6362aae` adds the matching 12-case same-boot terminal `SIGKILL` matrix
  across both epochs, both legacy sources, and three deletion boundaries.
  Accepted commit `a0966008` widens only the exact RootLinks generation-12 finalizer. It captures and revalidates the non-`Clone` record binding before
  database/namespace evidence, consumes it once through record-bound deletion,
  and retains the same locked store through post-delete database -> namespace -> database proof, including archived topology, all five links, repeated public
  absence, and clean handoff.
  Accepted commit `b0af65d6` applies that exact architecture to RootLinks NewState generation 18. Accepted commit `806003ac` applies it only to RootLinks ActiveReblit generation 14 with the deterministic coverage and delete-error policy recorded above. Commit `0a91c2ed` adds exact writer-reopen residue restoration; accepted commit `39456719` exercises it and all three RootLinks finalizers through genuine same-boot `SIGKILL`, without claiming reboot or power-loss behavior. The ladder still has no roll-forward executor, boot publisher, actual
  boot-repair attempt, cleanup, or power-loss-equivalent proof.
  The exact reverse prefix has deterministic contracts and genuine
  process-termination coverage. The NewState suffix adds deterministic
  real-startup matrices, all five journal durability faults across each of four
  persistence boundaries, deterministic terminal-delete faults, 12 real
  terminal process-death cases, and the 28 candidate-move cases above, but not
  process death at every earlier database and routing effect. ActiveReblit now adds its separate 12-case RootLinks terminal process matrix while its unchanged legacy 12-case matrix remains legacy-only. ActivateArchived likewise adds 12 RootLinks terminal cases alongside
  its legacy 12 terminal cases and the 28 candidate-preservation cases above, but
  its other earlier interruption boundaries remain open. None of these lanes
  has reboot or power-loss proof: `SIGKILL` preserves the
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
  journal-update-boundary `SIGKILL` cases with fresh-process reopen. NewState,
  ActiveReblit, and ActivateArchived terminal deletion each retain an exact
  legacy Intent/Exchanged-only 12-case matrix. Accepted commit `39456719` adds their distinct RootLinks-only 3 x 2 x 6 terminal matrix with 36 cases, 84 child executions, 48 genuine same-boot deaths, and 36 final recoveries. NewState and ActivateArchived candidate
  preservation each cover 2 x 2 x 7 process-death cases; RootLinks NewState
  invalidation adds 2 x (5 SQLite + 5 journal) = 20 same-boot cases. This stays
  unchecked because the other phases and true reboot or power-loss durability
  outcomes are not yet covered.
