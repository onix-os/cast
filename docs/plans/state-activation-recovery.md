# Crash-Recoverable State Activation

[Back to the canonical package-function plan](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)

This subplan owns the detailed Phase 11 recovery contract and its accumulated
implementation evidence. Phase order, global constraints, validation gates,
completion, and repository closure remain authoritative in `PLAN.md`.

## Phase 11: Make state activation crash-recoverable

An atomic `/usr` exchange prevents a partially visible switch, but it does not
by itself explain an interrupted transaction after reboot. Stateful activation
must therefore persist intent before every irreversible effect, authenticate
the exact filesystem trees involved, and recover from durable evidence rather
than from in-memory flags or mutable pathnames. This work preserves the
existing Stone state model, merged-/usr layout, container-trigger boundary,
and instant rollback mechanism; it hardens their failure semantics.

- [x] Give each fresh-state database row a unique, canonical transition ID and
  provide exact `(state ID, transition ID)` lookup, clear, and removal
  operations. Allocation and its package selections commit in one SQLite
  transaction.
- [x] Publish the five merged-/usr root links without replacing foreign names.
  Anchor inspection to the opened root, accept only the exact raw-byte symlink
  targets, retain inode witnesses through the directory fsync, and reject
  final-name, staging-name, and root-replacement races.
- [x] Land a versioned, bounded, checksummed transition-journal codec and an
  owner-private descriptor-relative store. Canonical creation, advancement,
  and deletion must be conditional, process- and thread-serialized, atomic,
  fsync-ordered, crash-reopenable, and locked by an exact full-frame v1 golden
  fixture. The payload binds permanent per-tree tokens to a creation boot and
  mount-namespace epoch plus boot-scoped device, inode, and mount witnesses;
  those runtime witnesses are historical evidence after an epoch change, not
  durable identity. These journal and state operations retain Linux 5.6 as
  their descriptor-safe baseline; full frozen execution separately requires
  Linux x86_64 5.14 or newer. Restrictive-umask repair may use only an
  authenticated procfs alias to the retained descriptor.
- [ ] Open mutable system clients in recovery order: installation lock,
  databases, journal lock, journal reconciliation, orphan-token audit, strict
  live-state discovery, then repositories and the active registry. Frozen
  clients skip system recovery. Read-only clients must take a shared,
  non-mutating snapshot lock and fail closed on any unresolved journal.
  As of 2026-07-15, mutable construction takes the cooperating-writer guard
  before the journal to preserve the transition lock order, but defers strict
  live-state discovery until after database opening, retained fail-closed
  journal inspection, and the bounded orphan-token audit. The journal guard
  remains held through repository and active-registry construction. The strict
  result is checked against the preliminary Installation observation, which
  remains only a stale-clone witness; a mismatch is rejected rather than
  refreshed. Focused tests prove unresolved journal and orphan evidence precede
  malformed live state. Startup recovery execution is not implemented. The public
  `ReadOnlyClient` path is now real: construction requires the explicit
  snapshot authority, proves a clean journal before imaging the state database,
  rejects orphan transitions and prune residue before strict live selection,
  then images metadata and layouts. It exposes only bounded state-ID, exact
  state, active-state, exact metadata, and selected-layout queries; repository,
  registry, search, network, configuration, cache, and mutation surfaces are
  deliberately absent. Every query revalidates the retained installation,
  journal, database namespace anchors, and live-state proof before and after
  reading its immutable private database image. The installation-level snapshot
  authority opens only an existing root,
  `.cast`, default or custom cache, and their lockfiles; retains and revalidates
  their exact directory and lock identities; holds shared global and custom-cache
  locks through every clone; and bounds contended shared-lock acquisition to 30
  seconds without entering a blocking kernel lock call. Mutable `ClientBuilder`
  construction rejects explicit snapshots and naturally read-only installations
  before the coordinator or any SQLite handle is opened. Default authored intent
  is no longer evaluated by
  `Installation::open`: the mutable client retains the exact `etc/cast`
  directory, loads `system.glu` and its imports through the descriptor-rooted
  Gluon source authority only after the clean startup gate and strict active
  state discovery, and revalidates that active proof around evaluation. The
  journal and active-state guards remain live through repository and registry
  construction. Explicit imports retain their user-selected path semantics but
  also run after the gate, and repository CLI construction can no longer bypass
  this ordering. Focused tests cover journal/orphan precedence, frozen-client
  separation, CLI notice timing, unsafe source metadata, and root, directory,
  and source substitution. As of 2026-07-16, every mutable Installation clone
  also retains the exact `.cast`, database directory, and global-lock
  authority. Install, state, and layout SQLite handles open only through
  authenticated `/proc/thread-self/fd` locations whose exact directory aliases
  are proved before and after SQLite opens them and whose anchors outlive the
  connections; the complete root/cast/lock/database chain is revalidated around
  each open, including SQLite error paths. Startup opens the journal beneath
  that retained `.cast` descriptor rather than resolving the public name again.
  Five focused tests replace each authority boundary, prove detached anchored
  operation, reject the substitution, and verify that every foreign replacement
  remains untouched. This proof currently gates construction; post-build
  lifecycle operations still require the coordinator-wide capability preflight
  below. Startup reconciliation and the remaining mutable recovery coordinator
  therefore keep this item open.
- [ ] Replace path-based activation, archive, restore, quarantine, and cleanup
  with one retained capability namespace. Resolve beneath authenticated
  directory descriptors without symlink, magic-link, or mount traversal. Give
  every `/usr` tree one reserved, permanent random token which follows that
  logical tree through staging, exchange, archive, and quarantine; treat
  device, inode, and mount ID only as boot- and mount-namespace-scoped runtime
  witnesses. Require candidate and previous to have distinct tokens and
  filesystem objects on the same exchange-capable mount, keep all descriptors
  close-on-exec, and fsync every changed parent before recording completion.
  As of 2026-07-15, fresh-state creation, active-state reblit, and inactive
  archived repair share one descriptor-bound candidate-metadata primitive.
  It retains the authenticated candidate `/usr` and `lib`, bounds and pins
  `os-info.json`, prepares both generated files as sealed `O_TMPFILE` inodes,
  publishes their names no-replace, and retains exact file/content witnesses
  through transaction triggers, the final exchange validator, system triggers,
  previous-tree archive, and boot synchronization. Strict activation proofs
  include the exact `.stateID`; recovery remains marker-only. Package layouts
  now reserve `lib/os-release` and `lib/system-model.glu` while keeping
  `lib/os-info.json` package-owned. Applied-but-reported-error `lib` publication
  completes the same `/usr` fsync suffix as ordinary success. Focused tests cover
  symlink escapes, existing regular and hardlinked outputs, final-name races,
  candidate substitution, trigger-time deletion/rewrite/replacement/hardlink,
  sealed success, compensating recovery, and atomic rollback. External
  ephemeral application now retains the exact target, `/usr`, and separately
  published `/etc`; publishes the root ABI through the retained target
  descriptor; and keeps the metadata, root-ABI, isolation-root, and active-state
  proofs live across both trigger phases. Transaction and system trigger
  discovery is rooted beneath the retained `/usr`, while execution pins exact
  `/usr` and `/etc` descriptors into an anchored container. The system phase is
  structurally unable to enter the live-root direct-execution branch. Focused
  production-path tests cover metadata mutation, target and `/usr` substitution,
  `/etc` publication and replacement races, pinned bind substitution, invalid
  and destructive replacement triggers, and retained root-ABI publication.
  Other path-based lifecycle and cleanup paths remain, so this item is
  intentionally still open.
- [x] Land the descriptor-relative `/usr/.cast-tree-id` primitive independently
  of coordinator integration. Its fixed v1 frame is bounded, checksummed, and
  locked by an exact golden; pre-journal publication uses an anonymous
  same-filesystem `O_TMPFILE`, full file syncs, identity-bound no-replace
  linking through authenticated procfs, directory sync, and retained inode
  revalidation. Canonical markers are exact owner-owned 0444 files and use one
  link by default; the narrowly authorized state-slot transition may retain
  the sole second link described below. Package ownership of both durable and
  temporary names is forbidden, and filesystems without linkable `O_TMPFILE`
  support fail closed without a named pathname fallback. The recovery API is
  structurally read-only: a missing, malformed, mismatched, replaced, or
  temporary marker fails without minting or repair.
- [x] Consume that primitive at the real in-process activation boundary without
  claiming crash-reopen coordination. After candidate materialization (and,
  for the legacy fresh-state path, database allocation), the stateful client
  takes the canonical journal lock, rejects any journal or transition-bearing
  database row, then creates or adopts distinct markers for candidate and
  previous before transaction/system triggers or `/usr` exchange. When live
  `/usr` is genuinely absent, it is created as an exact empty same-mount child
  beneath the retained installation-root descriptor, checked for ACLs and
  racing occupants, fully synced with its parent, name-revalidated, and marked
  before exchange. The guard retains both inode proofs and the journal lock
  across exchange, archive, quarantine, and compensating recovery; every
  forward and compensating live/staging `/usr` exchange now resolves both
  parents beneath the authenticated installation-root descriptor, binds both
  children to those retained proofs, performs exactly one descriptor-relative
  `RENAME_EXCHANGE`, and reconciles both names after every syscall result. An
  error reported after the move is adopted rather than blindly exchanged
  back; both changed parents are synced and revalidated before success, while
  a forward post-move durability fault is routed through the swapped recovery
  path. If the compensating reverse exchange has already moved both trees,
  recovery retries only its idempotent sync-and-revalidation suffix before
  preserving the staged candidate; it never exchanges the trees a second
  time.
  Every other post-preparation pathname check uses the exact-token recovery
  reader and binds both the currently named directory and marker inode to the
  retained proofs, so a copied token cannot authenticate a substituted tree.
  Failed candidates enter a deterministic token-named quarantine through
  retained parent descriptors and a no-replace move. Only an empty slot
  created and inode-retained by the live guard is eligible for one bounded production
  retry after an in-process fault; pre-existing empty or populated collisions
  fail closed. A `syncfs` barrier flushes dirty candidate data and metadata on
  its root filesystem before the changed parents are synced, and the complete
  retained name/inode proof is repeated before a fresh database row may be
  invalidated. Nested-mount rejection and any other-filesystem descendants
  remain part of the pending descriptor-recursive coordinator. The primary
  previous-tree archive and compensating restore now retain the roots,
  staging, and state-slot parents beneath the authenticated installation root.
  A missing slot is first created as an exact owner-private, ACL-free directory
  at one of 256 bounded non-state parking names, then published to the canonical
  positive-decimal state name with one descriptor-relative no-replace rename;
  partial preparation can therefore leave only inert hidden residue, and
  ambient empty state slots are never adopted. Each archive/restore direction
  pre-syncs and revalidates the exact previous tree, makes one descriptor-relative
  `RENAME_NOREPLACE` attempt, reconciles both names by permanent token and
  directory inode after every syscall result, fsyncs every changed parent, and
  performs a final namespace proof. Exact pre-syscall `after` layouts are
  adopted as applied; an unprovable layout is ambiguous rather than mislabeled
  not-applied. After an aborted archive or compensating restore, the exact empty
  wrapper is non-destructively renamed back to its private parking name and the
  canonical absence is synced. It is never unlinked by a mutable final name, so
  a racing replacement is preserved; post-retirement durability faults resume
  only the idempotent sync/revalidation suffix, with one bounded production
  retry before recovery reverses `/usr`. Proven post-move durability failures
  likewise resume only their idempotent suffix and never rename the tree a
  second time. The bounded scan deliberately fails closed after all 256 names
  are occupied, preserving both canonical and staged namespaces. A later
  previous-tree archive may reuse one uniquely authenticated marker-only
  wrapper left by archived-candidate activation instead of consuming another
  bounded name; every foreign file type or wrapper layout is preserved and
  skipped, multiple structurally valid reusable wrappers fail closed, and
  reclaiming any other inert parked wrapper across process restarts belongs to
  the later durable coordinator.
  Initial staging and compensating rearchive of an archived candidate now
  retain the roots, canonical state wrapper, and fixed staging wrapper, make
  exactly one descriptor-relative `RENAME_EXCHANGE`, and reconcile both exact
  wrapper inodes after every syscall result. Once the exchange has applied,
  retries finish only the sync-and-revalidation suffix and never exchange the
  wrappers a second time. The displaced staging wrapper is tracked by the sole
  authorized extra hardlink to the archived candidate tree's permanent
  `/usr/.cast-tree-id` inode, not by a separately forgeable token file. The
  in-process path publishes that link no-replace from the retained canonical
  marker after strict revalidation. On restart, an already two-link marker is
  authorized only after a bounded scan proves exactly one state-slot link with
  the same inode: parked wrappers must be exactly marker-only, while the
  canonical wrapper may be marker-only or contain that marker plus the exact
  retained `usr` tree. That retained link moves no-replace between the
  exchanged wrappers, and the displaced wrapper is restored, parked, or
  retired without unlinking a mutable final name. Applied restore or
  marker-transfer preparation receives one bounded client retry; foreign
  regular files, symlinks, FIFOs, and unsafe directories are preserved, while
  unexpected I/O and duplicate authenticated layouts fail closed. Repeated
  archived activations therefore reuse the same exact wrapper past the
  256-name scan bound during normal operation.
  Active-state verification reblits now reserve an exact empty 0700 replacement
  wrapper before the live `/usr` exchange. The bounded 256-name scan skips every
  occupied final-component type without adoption, and exhaustion is discovered
  while the old live tree is untouched. Once that replacement is retained and
  durable, but still before triggers or the live exchange, an authorized
  marker-only wrapper at the old active state's canonical decimal name is
  retained and moved with one descriptor-relative no-replace rename into the
  bounded `archived-candidate-slot` namespace already recognized by slot-link
  recovery. Exact canonical/parking reconciliation makes the outcome sticky;
  an applied move resumes only marker, wrapper, and roots sync plus final proof.
  Foreign occupants and exhaustion are preserved unchanged. Every later active
  snapshot requires the exact old wrapper to remain marker-only and parked with
  the canonical name absent, so a trigger cannot silently re-poison the next
  ordinary archive. The old quarantined tree and parked wrapper retain their
  exact two-link marker pair, while the repaired live tree's new token can be
  archived into a fresh canonical wrapper on the following transition. The
  candidate's `.stateID` is retained
  as one exact owner-owned 0644, single-link inode beside its retained tree
  marker; bounded full reads, metadata witnesses, final-name reopen, and marker
  sandwiches reject same-inode rewrites and same-content inode replacement after
  transaction triggers, inside the exchange preflight, after system triggers,
  before and after boot synchronization, and at final proof. Recovery movement
  remains marker-only so a trigger-corrupted candidate can still be reversed and
  preserved. On success, one whole-wrapper `RENAME_EXCHANGE` moves the displaced
  old wrapper intact to the private `active-reblit-wrapper` slot and leaves the
  exact empty replacement at fixed staging. Entry into active-state verification
  requires a strict retained proof of the live `.stateID`; a missing, malformed,
  unsafe, or conflicting live selection fails closed before candidate staging is
  mutated. After exchange, the displaced old payload remains opaque and is never
  repaired in place. Restart-safe recovery from damaged live selection metadata
  remains dependent on the durable baseline and startup reconciliation work
  below. Once the replacement reservation is retained, a pre-commit failure,
  or a failure after a compensating `/usr` reverse, uses that same pre-reserved
  exchange to preserve the entire failed candidate wrapper and consumes no
  second quarantine name. If bounded-name exhaustion or a create/reopen failure
  occurs before any reservation is retained, recovery instead uses the existing
  marker-authenticated `/usr` quarantine while leaving the live tree and every
  foreign wrapper-name occupant unchanged. Applied suffix failures resume
  without a second wrapper exchange, NotApplied final cleanup returns through
  swapped recovery, and ambiguous substitution is never retried or guessed
  through.
  `make forge-active-reblit-wrapper-test` covers every preparation/rotation
  fault, queued NotApplied and Applied faults, strict state-ID races, whole-wrapper
  sentinels, foreign-name collisions and exhaustion fallback, repeated same-client
  reblits, authorized two-link slot parking faults/races/exhaustion, and a
  subsequent ordinary archive after repair.
  Failed preparation never promotes the candidate, keeps its database row, and
  leaves or preserves the exact candidate in staging or its retained quarantine
  slot. Any preservation durability fault retains that database correlation and
  exact candidate. This remains an in-process, cooperating-lock boundary: it
  cannot make a filesystem rename and SQLite deletion atomic against an
  uncooperative same-UID writer. It does not create a journal record, reconcile
  a reboot, durably fence an ambiguous post-exchange namespace, perform the
  bounded descriptor-recursive stable-inventory proof, authenticate the entire
  activation namespace, or finish the pre-journal baseline and coordinator
  items below. Repaired-archive publication is descriptor-relative, and
  production stateful materialization already uses the retained fixed-staging
  capability; `blit_root_with_materialization` is test-only. Archived-state
  pruning now authenticates an exact journal-locked batch of at most 64 retained
  wrappers, detaches each into a fresh private no-replace quarantine, applies
  boot exclusions before rollback selection, removes the exact database rows in
  one reconciled SQLite transaction, and deletes only beneath the private
  retained descriptors with aggregate entry, depth, byte, operation, and time
  bounds. Startup scans twice for every raw-byte `state-prune-*` residue before
  live-state or authored-intent discovery, so interrupted pruning blocks normal
  reopening without deleting evidence. `make forge-state-prune-test` covers 34
  exact production, compensation, restart-residue, race, bound, deletion, and
  database cases; the startup-gate lane covers the residue precedence. This is
  still not durable automatic recovery: there is no prune intent to adopt after
  process death, the pinned boot manager does not propagate every stale-entry
  deletion failure, syscall deadlines are cooperative, and package/CAS orphan
  cleanup remains path-based. Other legacy garbage-collection paths also remain,
  so this item does not claim complete lifecycle safety.
- [ ] Establish a durable pre-journal baseline. With no journal and no orphan
  transition row, clean only bounded authenticated scratch, materialize and
  recursively sync the candidate, create or adopt its strictly validated tree
  marker and fsync both marker and `/usr` before journal creation, synthesize
  and sync an empty live `/usr` only when genuinely absent, classify managed,
  corrupt, empty, and unmanaged previous trees from strict evidence, reject
  missing, malformed, or duplicate tokens where recovery requires identity,
  reserve the marker path from package and trigger output, and preflight every
  root ABI name.
  As of 2026-07-15, the root-ABI portion uses a retained two-stage capability:
  all five final names and all five legacy `.next` names are inspected without
  mutation before candidate identity preparation, so a static foreign occupant
  leaves live `/usr`, the already-materialized fixed-staging candidate, tree
  markers, triggers, and the already-allocated database row unchanged. The same
  root descriptor and per-link absence/inode witnesses are
  revalidated before transaction triggers, at ordinary exchange preflight, and
  inside the final exchange validator. Only after the forward `/usr` exchange,
  while the already-clean journal guard remains retained, are missing canonical
  links published no-replace and fsynced through that descriptor; the completed
  capability is revalidated before and after system triggers. Focused coverage
  exercises every final and `.next` conflict, archived-candidate ordering,
  retained absence and inode replacement at the exchange boundary, and
  compensating recovery when a foreign name wins after `/usr` exchange but
  before root-link publication. As of 2026-07-16, the candidate portion also
  retains the exact candidate `/usr` and performs an iterative, raw-byte,
  descriptor-rooted inventory without following symlinks or crossing mounts.
  Entry, depth, raw-name, regular-byte, operation, and cooperative deadline
  bounds fail closed, while attacker-scaled collections, paths, names, and
  buffers use fallible reservations. Regular contents are SHA-256 hashed inside
  full metadata sandwiches; symlink targets remain opaque bytes; special
  inodes, duplicate or non-marker hardlinks, foreign owners, POSIX ACLs,
  extended attributes on regular files and directories, external-write mode
  bits, and the special mode bits already forbidden by Stone materialization
  are rejected. Canonical symlinks cannot carry user or file-capability xattrs;
  security-label symlink attributes are outside the supported package model
  rather than inspected through an unsafe pathname fallback. This canonical
  boundary therefore deliberately excludes SELinux-, IMA-, or EVM-labeled
  candidate filesystems until those attributes gain a typed declarative model.
  Every exact file and directory is synced
  bottom-up, symlink namespace durability receives a filesystem barrier, and a
  complete second inventory must match. Marker publication then admits only a
  sole new canonical one-link marker or exact adoption of a previously strict
  marker, followed by marker, root, inventory, `.stateID`, candidate-name,
  live-`/usr`-name, and mutable-namespace revalidation. Reopened authorized
  two-link marker recovery repeats marker and containing-wrapper durability
  before accepting that link.

  This is deliberately a cooperating-writer proof for a private staging
  wrapper, not a kernel freeze against same-UID inode-reuse or create/delete
  ABA. An archive produced by the former CAS-hardlink materializer whose
  ordinary payload still has `nlink != 1` fails closed; aliases already removed
  down to one link remain admissible. Current `state verify` does not discover a
  byte-correct surviving alias, so backward activation requires an explicit
  independent-copy repair or migration surface rather than silent adoption.
  The authenticated canonical marker's sole authorized second link remains the
  only exception. This boundary still creates no live journal and cannot
  reconcile a reboot. Authenticated scratch cleanup, complete
  previous-tree classification, trigger-proof marker reservation, and
  coordinator integration remain open, so this item is not complete.
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

  As of 2026-07-16, one intentionally unwired coordinator contract owns the
  durable prefix through `CandidatePrepared` for all three operations, the
  internal transaction-trigger sequence through `TransactionTriggersComplete`
  for new states and active reblits, and the common intent-only boundary
  through `UsrExchangeIntent`. A typed request makes the legal state
  relationships explicit: a new state has no
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

  The internal transaction-trigger runner derives its started and completed
  records through the journal's sole forward-successor constructor. It proves
  both retained runtime identities, both exact public tree names and markers,
  the candidate's retained `.stateID`, and operation-specific database
  ownership before intent and after the callback. New states require the exact
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

  `CandidatePrepared` returns one of two unforgeable operation-specific
  wrappers. `NewState` and `ActiveReblit` receive a wrapper which owns both the
  coordinator and metadata proof; `ActivateArchived` receives a distinct
  wrapper with no transaction-trigger method. The trigger runner exists only
  on the former wrapper and accepts no caller-supplied proof. It seals the
  candidate and performs evidence -> metadata -> evidence -> metadata checks
  immediately before durable trigger intent and again after the effect before
  completion. Thus replacing either canonical metadata inode with an
  identical-byte inode before intent invokes no effect and leaves
  `CandidatePrepared`; doing so inside the effect invokes it once and leaves
  `TransactionTriggersStarted`. Every returned failure owns neither the
  coordinator nor proof, so journal, installation, and database authorities
  are released while the error remains alive. The post-effect inventory still
  cannot substitute for the semantic proof because it intentionally baselines
  permitted payload changes. No live client path is changed or silently
  bypassed by this still-unwired slice.

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

  ActiveReblit deliberately does not call the legacy unjournaled wrapper
  rotation or slot-parking helpers. It retains and proves the exact
  canonical-or-parked two-link marker arrangement unchanged; a later sealed
  coordinator phase must own that monotonic namespace effect. Positive first
  installation coverage proves a synthesized empty previous `/usr` exchanges
  once and remains staged without a `.stateID`. The effect still has no live
  client callsite. Publishing its intent remains forbidden until the startup
  executor can resume or reverse every corresponding durable phase.

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

  The focused `make forge-transition-journal-coordinator-test` lane now runs 61
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
  synthesized-empty first installation, and the absence of root-link,
  reverse, retry, or cleanup effects. Its static gates prove that metadata authority is
  mandatory rather than optional, the runner accepts no proof parameter,
  archived activation cannot acquire trigger authority, no coordinator method
  has a callsite outside the contract module, and the callback authority and
  failure type remain private. The transition-identity gate additionally
  rejects mutation primitives in the existing-metadata verifier and any client
  bypass around coordinator-owned verification. No live
  activation path creates or advances this coordinator. In particular, the
  legacy ActiveReblit wrapper rotation still requires journal absence and an
  already published `.stateID`; it must be replaced by a later
  phase-authorized coordinator effect rather than reused or weakened. Startup must also
  classify ActiveReblit `Preparing` as strictly state-ID-absent and treat
  `CandidatePrepareStarted` as the only publication-ambiguity boundary. There
  is still no phase-specific recovery executor; the read-only startup
  assessment below cannot advance the record. This item remains open.
- [ ] Reconcile startup using exact phase-specific namespace and database
  evidence. Every pre-commit phase rolls back except a durably completed boot
  synchronization; `CommitDecided` and later roll forward. Resume rollback in
  its persisted order, never delete a fresh DB row before preserving its
  candidate, never guess through a foreign occupant, and retain an
  undeletable `BootRepairUnverified` record when boot side effects cannot be
  proved repaired.
  As of 2026-07-16, startup has a deliberately read-only, fail-closed
  assessment checkpoint. It classifies every validated persisted phase as
  begin rollback, resume rollback, roll forward, finalize rollback, or manual
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

  This snapshot is not recovery authority. It always records that a complete
  descriptor-rooted activation-namespace inventory is still required and
  exposes no mutation API. Inspection retains the installation, journal, and
  exact database capabilities through its final revalidation, then releases
  the mutable installation/global locks and exclusive journal before returning
  `RecoveryPending`; keeping that journal after the startup coordinator was
  released would permit a coordinator/journal ABBA deadlock. A retry must
  independently acquire locks in canonical order and reload the journal. The
  focused `make forge-startup-reconciliation-test` lane proves nine exact
  contracts, including the complete database phase matrix, pre-allocation
  rollback, the independent 19-phase and rollback-source provenance matrix,
  stable legacy absence, a database mutation between sandwich reads, current
  versus historical epochs, unauthenticated `nlink=2`, final directory and marker
  substitution, exact database retention, and a bounded second startup while
  the first diagnostic remains alive. Provenance is diagnostic evidence, not
  recovery authority: phase-specific namespace inventory and all
  rollback/roll-forward effects remain unimplemented, so this item stays open.
- [x] Add database ownership probes that distinguish matching, cleared,
  missing, and foreign transition rows, plus a bounded global orphan-token
  audit. Journal absence with any non-null transition token is corruption, not
  permission to start another transaction.
- [ ] Add deterministic process-kill and fault-injection coverage at every
  journal fsync, database mutation, rename/exchange, trigger boundary, archive,
  quarantine, and boot boundary. Reopening after each injected interruption
  must converge to exactly one authenticated live tree and one terminal
  outcome without deleting or overwriting a foreign entry.

**Exit gate:** after a kill or power-loss-equivalent interruption at every
persisted boundary, reopening Cast either completes the committed transition,
restores the exact previous `/usr` and preserves the candidate, or stops on a
structured manual-recovery record. It never starts a second transition while
the first is unresolved, never infers success from a pathname or an
out-of-epoch runtime witness alone, and never weakens atomic updates, state
separation, merged-/usr compliance, container trigger isolation, or fast
rollback.
