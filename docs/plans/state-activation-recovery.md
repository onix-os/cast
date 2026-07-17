# Crash-Recoverable State Activation

[Back to the canonical package-function plan](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)

This hub and its linked continuations own the detailed Phase 11 recovery
contract and its accumulated implementation evidence. Phase order, global
constraints, validation gates, completion, and repository closure remain
authoritative in `PLAN.md`.

Detailed evidence is divided by authority boundary:

- this hub retains the journal, client-opening, namespace, tree-identity, and
  pre-journal foundations;
- the [durable coordinator plan](state-activation-coordinator.md) owns forward
  transition sequencing and one-shot effects; and
- the [startup-reconciliation plan](state-activation-startup-reconciliation.md)
  owns recovery admission, rollback execution, and interruption evidence.

## Phase 11: Make state activation crash-recoverable

An atomic `/usr` exchange prevents a partially visible switch, but it does not
by itself explain an interrupted transaction after reboot. Stateful activation
must therefore persist intent before every irreversible effect, authenticate
the exact filesystem trees involved, and recover from durable evidence rather
than from in-memory flags or mutable pathnames. This work preserves the
existing Stone state model, merged-/usr layout, container-trigger boundary,
and instant rollback mechanism; it hardens their failure semantics.

### Journal and client-opening foundations

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
  refreshed. Mutable startup now also has one narrow phase-authorized recovery
  effect before diagnostic inspection: direct ActiveReblit
  `CandidatePrepared`, or rollback whose recorded source is
  `CandidatePrepared`, may normalize one exact same-owner restrictive
  replacement wrapper to mode `0700`. The sealed authority retains the exact
  installation, journal, database, active-state reservation, record, and
  initial in-flight evidence; it requires stable database ownership, immutable
  metadata provenance, and live active selection before and around the
  descriptor-bound chmod. Absent, canonical, inapplicable, ambiguous, foreign,
  or changing evidence is not mutated, and this effect never advances the
  journal. The focused `make forge-client-startup-gate-test` lane lists 21
  contracts, including 5 which prove compatible repair and zero chmod for
  incompatible database, active-selection, record, or installation authority.
  Apart from this chmod and the narrow rollback ladder through exact `/usr`
  reversal, `UsrRestored` persistence, and the later journal-only route to
  `CandidatePreserveIntent` documented in the
  [startup-reconciliation plan](state-activation-startup-reconciliation.md),
  general phase recovery execution is not implemented. The public
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
  lifecycle operations still require the coordinator-wide capability
  preflight in the [coordinator plan](state-activation-coordinator.md), while
  the [startup-reconciliation plan](state-activation-startup-reconciliation.md)
  owns recovery execution. Both keep this item open.

### Retained activation namespace

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

### Durable tree identity

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
  remains dependent on the durable baseline later in this hub and the
  [startup-reconciliation work](state-activation-startup-reconciliation.md).
  Once the replacement reservation is retained, a pre-commit failure,
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
  activation namespace, or finish the pre-journal baseline below and the
  [coordinator work](state-activation-coordinator.md). Repaired-archive
  publication is descriptor-relative, and
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

### Pre-journal baseline

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

### Durable transition coordination

The open coordinator work item, its implemented durable prefix, and its exact
validation evidence continue in the
[durable state-activation coordinator plan](state-activation-coordinator.md).
That document owns the operation state machines, metadata and provenance
authority, trigger sequencing, and one-shot forward exchange contract.

### Startup reconciliation and interruption campaign

The open startup-reconciliation and interruption work items, including the
completed database-ownership probes, Phase 11A's exact source-database-bound
fresh-transition removal substrate, Phase 11B's sealed exact invalidation
effect authority, Phase 11C's exact journal persistence checkpoint, and the
bounded authenticated `/usr` rollback prefix continue in the
[startup-reconciliation plan](state-activation-startup-reconciliation.md).
The substrate provides non-`Clone` complete-state/provenance evidence, one
exact no-retry transaction, and invocation-causal post-attempt reconciliation.
The test-sealed effect checkpoint pairs that evidence with exact journal,
installation, reservation, and preserved-candidate namespace authority; only
proved applied or already-satisfied absence can retain capability. Persistence
consumes that capability through one authority-owned journal advance and exact
canonical reopen. It still has no production dispatcher or second rollback
effect. That document owns the next route from durable `FreshDbInvalidated`,
later rollback actions, restart matrices, genuine process-death evidence, and
the remaining power-loss campaign.

The [canonical Phase 11 exit gate](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)
remains authoritative in `PLAN.md`.
