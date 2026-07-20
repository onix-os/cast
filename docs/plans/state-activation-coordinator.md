# Durable State-Activation Coordinator

[Back to the Phase 11 recovery hub](state-activation-recovery.md)

[Back to the canonical package-function plan](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)

This continuation owns the durable forward-transition coordinator contract,
its operation-specific authority, and the evidence which remains before it can
replace the live activation paths. Phase order, completion, and repository
closure remain authoritative in `PLAN.md`.

## Coordinator objective and durable ordering

- [ ] Drive new-state creation, archived-state activation, and active-state
  reblits through the same journal coordinator. Persist each intent before DB
  allocation, candidate decoration, trigger execution, `/usr` exchange,
  previous-state archive, boot synchronization, commit cleanup, or rollback;
  persist completion only after the effect and its durability and identity
  proofs succeed. Production-safe record constructors now derive the sole legal
  forward successor, insert a fresh state ID only at allocation completion,
  derive rollback requirements from exact observations, and advance every
  recovery action only with an explicit outcome. Their focused contract covers
  the complete fixed rollback order and the deliberately terminal unverified
  boot result.

## Operation-specific prefix and state machine

  As of 2026-07-20, one intentionally unwired coordinator contract owns the
  durable prefix through `RootLinksComplete` for all three operations. While
  that phase remains canonical, ActiveReblit must consume a sealed,
  non-trigger-ready typestate to reserve the exact replacement wrapper and
  park the authenticated previous-marker link. Both NewState and the reserved
  ActiveReblit path must then publish and retain the exact transaction-isolation
  ABI before acquiring trigger authority. The coordinator owns the internal
  transaction-trigger sequence through `TransactionTriggersComplete`, the
  common intent-only boundary through `UsrExchangeIntent`, the one-shot
  exchange effect through durable `UsrExchanged`, and retained root-ABI
  publication through its exact bound `RootLinksComplete` successor. A typed request makes the
  legal state relationships explicit: a new state has no
  candidate ID and classifies its previous tree as an active state,
  synthesized empty tree, or unmanaged tree; archived activation has distinct
  candidate and previous active-state IDs; and an active reblit binds the same
  state ID to the active-reblit candidate and corrupt previous-tree roles.
  Identity preparation retains an exact previous classification under the
  installation authority, and transition creation compares the request with
  that retained fact before runtime capture or journal creation. An
  `Unmanaged` request remains representable for a future authenticated path but
  is currently rejected: preparation can authenticate only the exact active
  state or a genuinely empty synthesized live `/usr`, never bless an arbitrary
  nonempty unowned tree.
  Transition creation generates one kernel-random 32-character lowercase
  hexadecimal transition ID and derives the exact state-ID-independent
  `failed-transition-<transition-id>` quarantine component from it. It captures
  the boot/mount-namespace epoch plus exact candidate and previous
  runtime tree witnesses and durable tree tokens, revalidates the retained
  identities, and persists `Preparing` at generation 1 before returning.
  `archive_previous` is derived only from the authenticated previous origin;
  system-trigger and boot-sync selections remain explicit request data.

  The new-state prefix is exactly `Preparing(1)` ->
  `FreshStateAllocating(2)` -> `FreshStateAllocated(3)` ->
  `CandidatePrepareStarted(4)` -> `CandidatePrepared(5)`. Archived activation
  and active reblit skip the inapplicable allocation states and follow exactly
  `Preparing(1)` -> `CandidatePrepareStarted(2)` -> `CandidatePrepared(3)`.
  New states then reach `TransactionTriggersStarted(6)` and
  `TransactionTriggersComplete(7)`; active reblits reach the same phases at
  generations 4 and 5. They then reach `UsrExchangeIntent` at generations 8
  and 6 respectively. Archived activation has no transaction-trigger phase
  and remains at `CandidatePrepared(3)` when that internal runner is offered;
  its separate proof-bearing typestate advances directly to
  `UsrExchangeIntent(4)`.
  Every transition uses the journal's conditional create/advance operations.
  A wrong operation or phase fails before storage, and an exact-record compare
  prevents a stale generation from overwriting newer evidence. A persistence
  error is deliberately fail-stop rather than assumed not applied: depending
  on the durable storage boundary, reopening may find the exact predecessor or
  its sole legal successor. Fresh allocation uses the transition ID as the
  sole database correlation token. Identity preparation also retains the exact
  in-process state-database capability; completion rejects a different handle
  before any ownership query and consults only the retained database.
  Completion accepts only the exact newly allocated state row with `Matching`
  ownership, and
  rejects missing, cleared, foreign-token, or wrong-state evidence without
  advancing. If the database commit succeeds but the following journal advance
  fails before publication, the matching row and older durable journal phase
  are deliberately preserved for later reconciliation rather than compensated
  or hidden; a post-publication error may instead leave the exact successor
  durable. Candidate identity is a private three-way retained authority rather
  than `Option<state::Id>`: `NewState` begins as unknown-ID/absent and becomes
  known-ID/absent only after correlated allocation is durably recorded;
  `ActiveReblit` begins as known-ID/absent because its newly materialized tree
  reuses the active database row; and `ActivateArchived` begins with an exact
  retained existing `.stateID`. Operation-specific constructors bind that
  distinction before a request can create the journal, so an absent candidate
  cannot be reinterpreted between NewState and ActiveReblit and neither can be
  passed off as an archived tree. For both newly decorated operations, the
  payload and exact marker are retained while `.stateID` and the fixed
  `.cast-state-id.tmp` are descriptor-proved absent through
  `CandidatePrepareStarted`. Only that durable phase authorizes publication.
  The coordinator creates one exclusive owner-private temporary, writes and
  syncs the canonical decimal state ID, normalizes and syncs mode `0644`, then
  makes one descriptor-relative `RENAME_NOREPLACE` attempt. It reconciles both
  names after every result, never retries an ambiguous rename, syncs the exact
  candidate directory after an applied move, and retains the published inode
  before recording `CandidatePrepared`. Certain pre-rename failures remove
  only the exact temporary and sync that cleanup; published or ambiguous
  failures remain at the operation's `CandidatePrepareStarted` generation as
  recovery evidence without overwriting or adopting a foreign final or
  temporary. NewState and ActiveReblit both enter this publication path;
  archived activation may only revalidate the state-ID inode retained during
  preparation and cannot enter it. Candidate preparation therefore preserves its exact
  durable predecessor-or-successor evidence when publication, identity proof,
  or final journal persistence fails. Every state-changing
  coordinator method consumes its
  coordinator; an error returns no reusable coordinator or stale in-memory
  record, so an uncertain persistence result fails stop instead of permitting
  an in-process continuation.

## Trigger, metadata, and provenance authority

  The internal transaction-trigger runner derives its started and completed
  records through the journal's sole forward-successor constructor. It proves
  both retained runtime identities, both exact public tree names and markers,
  the candidate's retained `.stateID`, operation-specific database ownership,
  mandatory operation readiness, and the exact descriptor-pinned isolation
  ABI before intent, immediately before the callback, after the callback, and
  at later readiness boundaries. New states require the exact
  `Matching` transition token; active reblits and every existing-state journal
  creation require an existing candidate row with `Cleared` ownership. Every
  distinct recorded previous state also requires a `Cleared` row, while a
  synthesized previous tree has no row and an active reblit reuses its already
  checked candidate row. The global database audit must contain exactly the
  new-state candidate and journal transition ID for `NewState`, and no
  transition-bearing row for `ActiveReblit`; invalid, multiple, or unrelated
  transition evidence cannot reach completion. A bounded
  existing-marker inventory seals the candidate before intent and establishes,
  syncs, and exactly re-inventories the callback's accepted result before
  completion. Safe root-owned one-link payload changes are therefore accepted,
  while candidate-name, state-ID, database, unsafe-inode, or unstable-inventory
  substitutions leave the durable phase at `TransactionTriggersStarted`.
  Intent persistence failure invokes no callback and may leave only
  `CandidatePrepared` or `TransactionTriggersStarted`; completion persistence
  failure invokes the callback once and may leave only
  `TransactionTriggersStarted` or `TransactionTriggersComplete`. Every error
  drops the coordinator-owned journal, identity, and database capabilities.

  The callback remains an intentionally unwired sequencing contract, but its
  authorization is now proof-bearing rather than a raw phase check.
  `CandidatePrepareStarted` is the only coordinator state which can construct
  the neutral metadata publication capability. The client-policy callback sees
  only the bounded optional `os-info.json` bytes read through that exact
  capability and returns both labeled, size-bounded semantic outputs together;
  it never receives the publication object, archived canonical bytes, or a
  proof token. The coordinator alone consumes those buffers and the publication
  capability. For `NewState`, it hashes both independently derived buffers and
  immutably inserts that pair under exact `Matching` transition ownership before
  either canonical output is published. An interrupted provenance commit may
  therefore leave no row or the exact row, while first/second publication and
  final journal faults may leave provenance plus absent/partial/complete outputs
  under `CandidatePrepareStarted`. `ActiveReblit` instead requires the existing
  immutable pair to match the newly derived buffers before publication. The
  resulting exact `os-release` and `system-model.glu` descriptor proof and the
  independently loaded provenance pair travel together through private
  operation typestates. Their database and descriptor evidence is sandwiched
  before `.stateID`, trigger, exchange-intent, and `CandidatePrepared`
  boundaries.

  `CandidatePrepared` returns one of three unforgeable operation-specific
  variants. `NewState` receives isolation-preparation authority;
  `ActiveReblit` receives a distinct non-trigger-ready reservation authority
  and reaches isolation preparation only after exact replacement and
  previous-marker parking durability; `ActivateArchived` receives a distinct
  wrapper with no transaction-trigger method. The two trigger-capable paths
  obtain the common trigger runner only after isolation preparation publishes
  and retains the exact ABI. That runner accepts no caller-supplied proof and
  carries the metadata, provenance, operation-readiness, and isolation
  authorities together. It repeats the candidate, evidence, metadata, and
  readiness sandwiches immediately before durable trigger intent and again
  after the effect before completion. Thus replacing either
  canonical metadata inode with an
  identical-byte inode before intent invokes no effect and leaves
  `CandidatePrepared`; doing so inside the effect invokes it once and leaves
  `TransactionTriggersStarted`. Every returned failure owns neither the
  coordinator nor proof, so journal, installation, and database authorities
  are released while the error remains alive. The post-effect inventory still
  cannot substitute for the semantic proof because it intentionally baselines
  permitted payload changes. No live client path is changed or silently
  bypassed by this still-unwired slice.

## Exchange intent and one-shot execution

  The common `/usr` exchange-intent boundary is deliberately effect-free.
  `NewState` and `ActiveReblit` can reach it only from their unforgeable
  `TransactionTriggersComplete` wrapper; archived activation reaches it only
  from its distinct `CandidatePrepared` wrapper. Both paths reseal the exact
  marked candidate, then repeat canonical journal, runtime epoch and tree,
  exact public-name, candidate state-ID, operation-specific database, global
  audit, and metadata-proof evidence immediately before a conditional journal
  advance. The exact sequences are therefore
  `TransactionTriggersComplete(7)` -> `UsrExchangeIntent(8)` for new states,
  `TransactionTriggersComplete(5)` -> `UsrExchangeIntent(6)` for active
  reblits, and `CandidatePrepared(3)` -> `UsrExchangeIntent(4)` for archived
  activation. A preflight failure leaves the exact predecessor canonical. A
  persistence failure may leave only that predecessor or `UsrExchangeIntent`,
  returns no coordinator, proof, descriptor, database, or journal authority,
  and requires reopening the canonical record before any continuation.
  Successful intent publication retains the exact tree identity and metadata
  proof but performs no rename, exchange, root-link publication, or client
  callback; the candidate remains staged and the previous tree remains live.

  The intent typestate remains proof-only, but a separate private and still
  unwired effect now owns exchange-syscall authority. Client preflight takes
  the active-state writer lease before inspecting the journal, retains the
  installation namespace, merged-/usr root-ABI preflight, and exact
  ActiveReblit snapshot when applicable, then consumes that authority during
  tree-identity preparation. The coordinator-only preparation seal selects a
  nonblocking journal acquisition. If a contender wins the small handoff gap,
  preparation fails immediately and releases the writer lease instead of
  waiting behind a journal owner which may itself need that lease. Legacy
  identity preparation keeps its existing blocking order, and every legacy
  exchange path still requires journal absence.

  The effect consumes both `UsrExchangeIntent` and the client authority. It
  repeats the complete journal, runtime epoch, public-name, marker, state-ID,
  database, provenance, metadata, active-state, root-ABI, and ActiveReblit
  evidence immediately before the syscall; makes exactly one
  descriptor-relative `RENAME_EXCHANGE` attempt; and never retries, reverses,
  cleans up, or publishes root links. Every raw syscall result is reconciled
  as `NotApplied`, `Applied`, or `Ambiguous`. Only the exact applied layout is
  synced through the staging parent and installation root, re-proved through
  the retained post-exchange authorities, and conditionally advanced to
  `UsrExchanged`: generation 9 for NewState, 7 for ActiveReblit, and 5 for
  archived activation. Any uncertain persistence result returns no reusable
  coordinator or authority and leaves only `UsrExchangeIntent` or its legal
  `UsrExchanged` successor durable.

  Commit `035d0843` adds the startup-side boundary which makes that durable
  `UsrExchanged` successor safe to inspect when merged-/usr publication was
  interrupted. For NewState, ActiveReblit, and ActivateArchived, each incomplete
  subset of the five canonical root links admits at most one retained publisher
  invocation per startup entry. An incomplete result leaves the exact `UsrExchanged` record
  canonical and returns `RecoveryPending`; a publisher error is treated as
  possibly applied and requires fresh reconciliation rather than an in-process
  retry. A set which is already complete at entry always synchronizes the
  retained installation root before rollback-decision evidence is captured
  again from scratch; complete-at-entry invokes the publisher zero times and
  synchronizes that root once. Exact public `.cast`, journal-directory, lock, and record
  identities remain authenticated around the effect, with the admitted record
  inode held open by `Arc<File>`. A bounded retained inventory of every
  noncanonical installation-root entry detects regular-file, symlink, and root
  replacement races which canonical-link inspection alone would miss.

  This startup normalizer deliberately has no journal-advance capability and
  never emits `RootLinksComplete`; the links remain complete while the existing
  rollback ladder reverses `/usr`. Commit `04911701` aligns the coordinator
  recovery proof with that ordering: an `UsrExchangeIntent` source reaches its
  pending rollback decision in one startup entry, while an initially incomplete
  `UsrExchanged` source uses one normalization entry and a second decision entry;
  complete-at-entry reaches the decision in one. Commit `03c5fd13` adds the
  independently reviewed production in-process `UsrExchanged` ->
  `RootLinksComplete` transition. It captures the exact bound predecessor after
  full preflight, publishes and synchronizes the retained no-replace root ABI
  once, repeats all operation-specific evidence, conditionally advances only to
  NewState generation 10, ActiveReblit generation 8, or ActivateArchived
  generation 6, and retains the exact successor inode with every earlier
  authority. The complete coordinator lane passes 97/97, its focused publication
  lane passes 15/15, and the startup normalizer remains at 19/19. Startup now
  consumes that durable successor through commit `a4f16351`. Exact
  durable `RootLinksComplete` + `POST` consumes a non-Clone predecessor-record
  binding and persists one `RollbackDecided` without replaying root-ABI
  publication or synchronization. Exact successor binding and an independent
  canonical reopen make predecessor or successor inode replacement fail closed.
  The entry stops at `RecoveryPending`. Commit `2201a24b` consumes the next
  fresh exact decision through a separately record-bound journal-only route to
  `ReverseExchangeIntent`; all five journal fault points, both epochs, and all
  three operations reopen to only the exact source or successor without a
  reverse, root-ABI, namespace, or database effect. Commit `66e3cf6b` closes
  the remaining decision/route cross-reopen identity window: after same-store
  successor validation, the non-Clone binding survives destruction of the old
  store and an independent canonical reopen must authenticate the exact
  successor inode and record inside an installation-revalidation sandwich.

  Commit `1b34d718` then admits this RootLinks source through the complete exact
  reverse-effect chain. The same non-Clone record binding crosses admission,
  one reconciled physical effect, ordered parent durability, and bound journal
  persistence. The durable authority itself seals `Applied` after exactly one
  reverse exchange or `AlreadySatisfied` from exact `PRE` evidence; callers
  cannot choose the successor outcome. Publication validates its successor
  binding in the same store and again by exact inode and record after canonical
  reopen. The focused operation/epoch/outcome matrices cover all three
  operations, current and historical records, all five bound-update faults,
  same-byte replacement seams, and restart convergence without a second
  exchange. Fresh entries now take RootLinks exactly through
  `RollbackDecided` -> `ReverseExchangeIntent` -> `UsrRestored` while the five
  canonical root links remain unchanged, and a later entry leaves the restored
  record byte-identical.

  RootLinks is still intentionally excluded from candidate admission and from
  the `UsrRestored` candidate route. Exact candidate preservation is the next
  safe hardening boundary; these commits do not claim candidate completion,
  boot repair, cleanup, reboot, or power-loss durability.

  ActiveReblit no longer enters the legacy unjournaled wrapper-rotation path.
  While `CandidatePrepared` is canonical, a sealed coordinator-only effect
  reserves the exact empty replacement wrapper and parks an authenticated
  second previous-marker link through the journal-authorized namespace API.
  The resulting retained reservation is mandatory before trigger authority and
  is revalidated through triggers, exchange intent, and the post-exchange
  direction flip. Positive first-installation coverage proves a synthesized
  empty previous `/usr` exchanges once and remains staged without a `.stateID`.
  The coordinator still has no live client callsite. Publishing its intent
  remains forbidden because the startup
  [rollback ladder](state-activation-startup-reconciliation.md#admitted-startup-recovery-ladder)
  covers the full no-boot rollback suffixes through authenticated terminal
  absence and the journal-only ActiveReblit boot-required/unverified routes,
  but not every corresponding durable forward, other boot-bearing rollback,
  roll-forward, actual boot effect, or cleanup phase.

## Archived-state verification

  Archived activation dispatches to a separate read-only verifier because its
  candidate already contains canonical metadata. The coordinator first loads
  the immutable digest pair from the exact state database, then derives both
  expected buffers from retained policy input without opening either canonical
  output. Only after their labeled hashes match that independent database row
  does the verifier descriptor-read and retain both exact output inodes. It
  repeats the database provenance read after proof construction and around every
  later proof boundary. Verification performs no chmod, synchronization,
  replacement, or other mutation; a same-byte output replacement or provenance
  deletion inside either sandwich is rejected with candidate files preserved.
  Legacy archived states without provenance fail closed rather than hashing
  their archived bytes into a new expectation.

## Validation evidence and remaining work

  The focused `make forge-transition-journal-coordinator-test` lane now runs 82
  exact tests and freezes
  those three phase/generation sequences, request-derived origins and options,
  runtime evidence, fixed quarantine naming, non-reinterpretable three-way
  candidate state authority, ActiveReblit state-ID publication failures, exact database correlation,
  transaction-trigger ordering, predecessor-or-successor persistence faults,
  substitution rejection, proof-bearing operation dispatch, exact
  `os-info.json` policy input, pre-intent and post-effect metadata replacement,
  fail-stop lock release, exact `/usr` exchange-intent and `UsrExchanged`
  generations for all three operations, prepared-candidate resealing, complete
  pre-intent and immediate pre-syscall evidence, predecessor-or-intent and
  intent-or-completion persistence faults, provenance commit outcomes,
  first/second output interruption, existing-state legacy/mismatch rejection,
  archived provenance sandwiches, proof/provenance typestate retention,
  one-shot raw-result reconciliation, applied durability faults, post-syscall
  metadata and namespace substitution, a bounded writer/journal handoff,
  synthesized-empty first installation, sealed ActiveReblit replacement and
  previous-slot reservation faults, typed readiness retention, mandatory
  isolation-ABI publication, tamper rejection, and reopenable crash prefixes,
  plus the absence of root-link, reverse, retry, or cleanup effects. Its static
  gates prove that metadata authority is mandatory rather than optional, the
  runner accepts no proof parameter, archived activation cannot acquire trigger
  authority, ActiveReblit cannot reach triggers without the reservation, and
  neither trigger operation can reach them without retained isolation
  readiness. No coordinator method has a callsite outside the contract module,
  and the callback authority and failure type remain private. The
  transition-identity gate additionally
  rejects mutation primitives in the existing-metadata verifier and any client
  bypass around coordinator-owned verification. No live
  activation path creates or advances this coordinator. The legacy
  ActiveReblit wrapper rotation still requires journal absence, while the
  coordinator uses a separate sealed journal-authorized reservation boundary.
  Startup classifies ActiveReblit `Preparing` as strictly state-ID-absent and
  treats `CandidatePrepareStarted` as the only state-ID
  publication-ambiguity boundary. There is still no general phase-advancing
  recovery executor. The bounded production startup ladder can now normalize
  forward exchange-parent durability, persist `RollbackDecided`, route a later
  entry to its first unresolved rollback intent, restore `/usr`, and persist
  `UsrRestored`. Separate operation-specific entries then carry NewState through
  candidate preservation, fresh-row invalidation, `RollbackComplete`, and
  authenticated terminal journal absence. ActiveReblit whose rollback plan
  requires no boot repair carries its whole-wrapper preservation through a
  later journal-only `CandidatePreserved` to `RollbackComplete` route. A
  separate later entry admits terminal deletion only for exact
  `ExistingCandidate`/`Cleared` database evidence with present provenance,
  `previous: None`, `candidate == previous`, and the unchanged preserved-wrapper
  topology and index. It retains the same continuously locked journal store for
  one conditional delete, authenticates public absence, and transfers that
  store to shared clean admission without a database, non-journal namespace,
  trigger, wrapper, or cleanup effect. An exact `BootSyncStarted` rollback
  instead routes `CandidatePreserved` to `BootRepairRequired`; a later startup
  observing `BootRepairStarted` records terminal `BootRepairUnverified` without
  invoking boot. The v2 journal model has typed Applied/AlreadySatisfied
  completion edges, but the actual repair attempt and successful production
  dispatch remain unwired. ActivateArchived
  preservation, completion, and terminal finalization now run as three separate
  bounded production entries with no same-entry successor redispatch.
  Commits
  `62b15f29`, `e69ad276`, and `50cb98f8` respectively sealed the exact restored
  outcome, connected the one-phase reverse dispatcher to real mutable startup,
  and proved its initial parent- and journal-restart convergence. Commit
  `86c6c900` extended that interruption matrix through fresh-handle restart,
  evidence races, and both coordinator-origin failure classes. Commits
  `ecd58020` and `e8c952f9` add genuine `SIGKILL` coverage around reverse
  execution and its journal update, respectively. Commit `c7c97d4c` adds the
  final journal-only route in that prefix without executing candidate
  preservation. The restrictive
  replacement-mode normalizer still never changes the record, and the
  diagnostic startup assessment remains non-mutating. Commit `20b36768` adds a
  source-database-bound, non-`Clone`, exact fresh-transition removal substrate:
  one snapshot binds complete state, selections, provenance, and the global
  in-flight invariant; one no-retry transaction removes provenance, selections,
  then state; and commit `7af46ce9` ensures that fresh reconciliation never
  mistakes net absence for proof that this invocation performed the deletion.
  Commit `ab1bfd5e` consumes that substrate only behind a separate test seal:
  exact NewState `FreshDbInvalidationIntent` evidence selects disjoint one-call
  Apply and zero-call Finish typestates, while only proved applied or already-
  satisfied absence retains capability. Commit `a15a7bc9` then consumes that
  capability through two revalidations, one authority-owned
  `FreshDbInvalidated` successor, one conditional journal advance, and exact
  canonical reopen. Source-side restart uses zero-removal Finish; successor-
  side restart skips invalidation. Production dispatch, namespace mutation,
  later rollback actions, roll-forward, triggers, and cleanup are not executed,
  so this item remains open. ActiveReblit's terminal finalizer now has an exact
  12-case real-process matrix across current/historical record epochs, both
  rollback sources, and final-PRE source retention, kernel-observed post-unlink
  absence, and post-directory-sync absence. Each crash is a genuine same-boot
  `SIGKILL`, followed by fresh-process production startup; it is not evidence
  that a pre-sync state survives reboot. Historical epoch is only a mismatched
  runtime witness, not a reboot simulation, and no power-loss claim is made.
  Phase 11 and its broad interruption campaign therefore remain open.
  Commit `c8c5ea41` production-wires ActivateArchived's bounded completion
  suffix. Commit `32bf8589` adds its separate deterministic terminal deletion
  and same-lock clean handoff. Commit `c6362aae` adds its exact 12-case
  real-process terminal `SIGKILL` matrix across both epochs, both rollback
  sources, and the three deletion boundaries. The newer bounded boot
  projection, sealed Stone inputs, state roots and schemas, local and package
  command-line semantics, Gluon topology intent, and retained mounted-topology
  evidence and the pure BLS renderer remain outside this coordinator. They
  grant no forward phase, destination authority, or publisher callsite.
  Roll-forward, durable boot publication, the actual boot-repair effect, cleanup, earlier
  interruption boundaries, and power-loss-equivalent work remain.
