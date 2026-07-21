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
  Beyond this chmod, the bounded rollback ladder documented in the
  [startup-reconciliation plan](state-activation-startup-reconciliation.md)
  now covers the shared `/usr` reversal prefix and exact RootLinks
  `RollbackComplete` for NewState, ActivateArchived, and ActiveReblit. The
  genuine same-boot NewState death matrix remains confined to fresh-row
  invalidation rather than completion. The complete pre-existing NewState suffix reaches
  authenticated terminal journal absence, and the ActiveReblit no-boot-repair
  suffix through the same clean-startup handoff. An ActiveReblit
  rollback sourced from `BootSyncStarted` instead routes a preserved candidate
  to `BootRepairRequired`; a separately observed `BootRepairStarted` record is
  retained as terminal `BootRepairUnverified` without invoking boot again.
  The v2 journal domain can represent explicit `Applied` and
  `AlreadySatisfied` repair completion, and commit `ffc32ce1` production-routes
  an already durable `BootRepairComplete` record to `RollbackComplete`. No
  production path performs the repair or emits that successful record.
  The pre-existing ActivateArchived rollback source set reaches that same
  authenticated clean-startup handoff; its RootLinks source stops at exact
  `RollbackComplete`. Roll-forward execution, the actual boot
  repair effect and its durable publisher, and cleanup are not implemented.
  The public `ReadOnlyClient` path is now real: construction requires the explicit
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
  Commit `b7135946` makes stateful system-trigger execution consume retained
  isolation-root, local-`/etc`, and live-`/usr` capabilities for both live and
  alternate installation roots; the pathname-authorized live-root fallback no
  longer exists. The anchored scratch root is read-only, exposes only writable
  `/etc` and `/usr`, read-only `/proc`, bounded `/tmp`, and a minimal `/dev`, and
  does not expose `/sys`. Construction, activation, and the post-payload
  boundary revalidate all three public identities; a payload failure and a
  simultaneous revalidation failure are both retained in the returned error.
  The focused root Make lane covers twelve policy, substitution, activation,
  and post-payload contracts. Other path-based activation, archive, restore,
  quarantine, and cleanup paths remain, so this item is intentionally still
  open.

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

### ActiveReblit boot-repair input and topology foundations

The boot-repair effect remains unwired, but its fail-closed input and recovery
boundaries are no longer absent. Commit `92fa7aa0` production-routes only an
exact ActiveReblit `CandidatePreserved` rollback sourced from
`BootSyncStarted` to `BootRepairRequired`. Commit `b5928340` independently
admits an exact `BootRepairStarted` record on a later startup entry and advances
it to terminal `BootRepairUnverified` while invoking boot zero times. There is
still no production `BootRepairRequired` -> `BootRepairStarted` attempt,
durable publisher, successful completion dispatch, or terminal deletion for
that branch. Commit `406cabe5` nevertheless closes the journal vocabulary: new
records use payload v2, byte-canonical v1 remains readable and advances only
through its old domain, and typed constructors distinguish Required -> Started,
Started -> `BootRepairComplete { Applied | AlreadySatisfied }`, Started ->
Unverified, and Complete -> `RollbackComplete`. The generic rollback successor
cannot traverse those boot-specific edges, and `BootRepairComplete` remains
nondeletable until its explicit terminal advance.

The implemented preparation stack now carries one caller-owned absolute
deadline, without resetting it, through the exact state/layout database and
Stone projection -> bounded boot-asset plan -> sealed CAS snapshot -> Stone
binding chain. Exact live and archived state-root authority, state schemas, and
the retained local `/etc/kernel/cmdline.d` append/mask policy expose the same
deadline-preserving preparation and revalidation boundary, including terminal
checks after completed materialization. Commit `66d7f6d1` separately binds
package-owned command-line files to the exact non-`Clone` Stone owner and
exposes only normalized semantic text after bounded explicit-offset reads and
exact coordinate revalidation.

Machine-local Gluon intent declares explicit ESP and optional XBOOTLDR
PARTUUIDs and mount points, while `/etc/cast/root-filesystem.glu` authenticates
one closed root-filesystem intent and can produce exactly one injection-safe
`root=...` token after terminal revalidation. Bounded current-thread mountinfo,
current-task-root, attachment-chain, and sysfs observations produce retained
mounted-topology evidence without mounting, discovering an alternative, or
granting mutation authority. The pure publication plan uses that topology to
share one collision domain for aliased ESP/Boot or separate XBOOTLDR when it is
distinct, and performs a terminal deadline check after materialization.

Commit `d3151b53` closes the lifetime-bound semantic render-input aggregate.
It retains the exact Stone and state-root owners, internally constructs schema
and package-command-line children, joins only eligible nonempty kernel inputs,
and rebinds the exact systemd-boot, kernel, and ordered initrd coordinates.
Every package and local append is grammar-audited before scope or masking;
`root` and `cast.fstx` are reserved globally; canonical per-kernel command
lines are admitted against byte, token, and aggregate bounds before output
allocation. The final database revalidation closes the sandwich after semantic
materialization, and the returned view retains the caller's absolute deadline.
Commit `3f752e32` makes the revalidated mounted-topology view retain that same
deadline for the renderer to compare rather than silently minting a new budget.

Commit `aa341706` adds the pure deterministic BLS renderer. Its non-`Clone`
result retains the exact revalidated semantic inputs, topology, absolute
deadline, topology-scoped publication plan, and sealed asset views without
exposing detachable source or mutation authority. It pins loader and Type 1
entry bytes, canonical payload order, case-insensitive collision handling,
FAT-safe relative paths, pre-materialization generated-byte limits, finite
request/path/work limits, and terminal deadline checks. Synthetic topology
fixtures exercise the production planning path without touching host storage.

Commit `dfa247d5` adds a distinct exact-byte SHA-256 identity. Sealed assets
compute it during the existing bounded copy pass, generated publications derive
it from their owned bytes, and renderer deduplication, publication planning,
and final source binding all retain and recheck it. XXH3 remains the path and
namespace-protocol checksum; this does not authenticate a publisher, establish
ownership provenance, or add cryptographic destination verification.

Commit `738ebd06` projects that bound plan into pure owned canonical desired
state. Its domain-separated SHA-256 binds the destination layout and every
output's root, phase, role, path, mode, XXH3, length, and exact content SHA while
excluding Stone binding indices, source kind and bytes, descriptors, runtime
mount identity, and the fingerprint itself. The inventory is descriptive only:
it supplies no persistence, ownership, deletion, or mutation authority.

Commit `9ac34286` adds a pure bounded destination-namespace assessment for the
rendered requests. It preserves request order, admits only stable `Absent`,
`Exact`, or `Different` states, and rejects raw/kernel-name disagreement, FAT
aliases, cross mounts, wrong node kinds, inventory and lookup races, content
drift, deadline expiry, and resource overruns. Commit `2eeaa22c` adds a
syscall-free bounded parser for complete raw `getdents64` chunks, with strict
record/name validation and a separately charged terminal EOF probe. It
produces only a closed raw-name inventory. Commit `f8a5da34` adds the one-shot
`getdents64` source, and commit `71ee5e95` reserves descriptor capacity before
ownership transfers and unwinds retained nodes in bounded LIFO order. Commit
`365e0ae5` completes the bounded production observer: it acquires fresh
directory descriptions and positional content readers below one private
retained destination and returns scalar states only. Commit `8620986a` retains
the exact observed-root device, inode, and mount ID. Commit `3f8309b1` then
sandwiches assessment through that same private destination `File` between
opening and closing boot-filesystem authentication and requires the root triple
to match. The next client blocker is a bounded expected-source bridge for
generated slices and sealed asset descriptors; it must stream positionally
rather than materialize the roughly 10-GiB publication ceiling.

Commit `b8acd3d4` adds bounded scalar-only destination-descriptor evidence for
stable directory identity and the Linux MSDOS magic family; commit `029f0590`
keeps its final descriptor inside retained attachment authority, and commit
`a93efe70` composes it with exact mountinfo `vfat` evidence in every topology
pass. Commits `9c688dc6` through `28c4735b` retain bounded kernel device names
and fixed-512-sector partition geometry; commit `24d82abf` seals the parent-disk
expectation to one freshly revalidated sysfs view. Commit `78b87df9` admits only
an exact `/dev` root attachment reported by mountinfo as `devtmpfs`, without
claiming that policy is descriptor authority. Commit `5ed70923` authenticates
strict caller-owned GPT images and exact ESP/XBOOTLDR roles without opening a
device. Commit `c2539d7f` strengthens that parser to require two complete,
exactly matching table passes under one cumulative ledger and deadline and
returns a role-independent table fingerprint without raw bytes or reusable read
authority. Commit `215b9032` retains the partition number, logical-block size,
and complete image length and introduces one private same-deadline inter-pass
hook before any second-pass source observation. Commit `2eeaa22c` then provides
bounded pure reconciliation of exact opening and closing injected block-node
observations with those GPT scalars and the sealed sysfs expectation. That
result is deliberately non-authoritative: it owns no descriptor and cannot
prove GPT read provenance. Commit `f8a5da34` adds read-only retained block
observations and bounded positional reads. Commit `1f9d578a` composes them into
an opening sysfs-parent preflight, two GPT passes separated by a caller-owned
name rebind and exact descriptor re-observation, a closing observation, and
reconciliation, returning distinct closed live read-provenance evidence.
Commit `dceab6cc` independently binds stable directory identity, authenticated
mount ID, and shared `TMPFS_MAGIC` to the exact devtmpfs mountinfo policy; it
proves same-mount descriptor evidence, not exact `/dev` authority by itself.
Commit `bfa3a0c2` now composes the exact retained `/dev` attachment with that
devtmpfs evidence, opens the sealed parent `DEVNAME` beneath the same private
destination, and owns the opening-preflight, GPT-pass-one, private name-rebind,
inter-pass observation, GPT-pass-two, closing-observation, and reconciliation
schedule. The closed result does not prove whole-root non-bind provenance or
ongoing currentness; same-thread `setns` still requires outer aggregate
revalidation. Linux MSDOS magic likewise remains distinct from exact `vfat`.
Disk admissibility beyond the admitted evidence, write authority, durable
descriptor-rooted publication, device-flush ordering, restart reconciliation,
and disposable-VM evidence remain open.
Default and focused tests do not inspect or mutate host ESP/BOOT storage; real
publication, reboot, and power-loss evidence requires the user-supplied
disposable VM.

### Durable transition coordination

The open coordinator work item, its implemented durable prefix, and its exact
validation evidence continue in the
[durable state-activation coordinator plan](state-activation-coordinator.md).
That document owns the operation state machines, metadata and provenance
authority, trigger sequencing, and one-shot forward exchange contract.

Commit `035d0843` closes the startup normalization prefix after an exact
forward `UsrExchanged` record without claiming the next durable phase. Across
NewState, ActiveReblit, and ActivateArchived, every one of the 32 subsets of
the five canonical merged-/usr links converges through at most one retained
publisher invocation per startup entry. An incomplete set remains at the exact
source record and returns `RecoveryPending`; publisher errors are possibly
applied and must be classified by a fresh entry. A set complete at entry always
syncs the retained installation root before rollback-decision authority is
captured again from fresh evidence. The authority authenticates the exact
public `.cast`, journal directory, lock, and record identities throughout and
retains the admitted record inode as an `Arc<File>`. Its bounded inventory of
all noncanonical installation-root entries detects file, symlink, and root
replacement races which would be invisible if only the five link names were
tracked.

The normalizer cannot advance the journal to `RootLinksComplete`, and the
canonical links deliberately stay complete through the existing rollback
suffix. Commit `04911701` proves the integration model: an intent source needs
one startup entry to reach the pending reverse decision, while an initially
incomplete exchanged source needs one normalization entry and a second decision
entry; complete-at-entry exchanged evidence reaches the decision in one.
Commit `03c5fd13` adds the independently reviewed production in-process
`UsrExchanged` -> `RootLinksComplete` transition. It binds the exact predecessor
after full preflight, publishes and synchronizes the retained no-replace root
ABI once, repeats operation-specific evidence, conditionally advances only to
the exact successor, and retains that successor inode with every earlier
authority. Commit `a4f16351` then admits exact durable `RootLinksComplete` +
`POST` during startup for all three operations only when the complete root ABI
is already present. It consumes the exact non-Clone predecessor-record binding
to persist one `RollbackDecided` with source `RootLinksComplete` and pending
`/usr`, invokes neither root-ABI publication nor complete-set synchronization,
verifies the exact successor binding, drops the old store, and independently
reopens the canonical journal. Same-byte predecessor or successor replacement
and uncertain storage outcomes never authorize success or retry. Commit
`2201a24b` admits only that exact decision through the journal-only resume
route. It captures exact record identity before namespace or database evidence,
consumes the non-Clone binding through one advance to `ReverseExchangeIntent`,
authenticates the returned successor binding, and canonically reopens after
every mutation uncertainty. Its 20 focused tests cover all three operations,
current and historical epochs, exact and conflicting plans, same-byte
predecessor and successor replacement, and all five journal fault points. The
route changes no namespace or database state and invokes no reverse exchange or
root-ABI effect. Commit `66e3cf6b` closes the residual reopen identity window
in both the decision and route boundaries. After same-store successor
validation, each keeps the non-Clone binding alive across destruction of the
old lock-bearing store; the independently reopened canonical store must then
match that exact successor inode and record inside an installation-
revalidation sandwich. Same-byte replacement between binding validation and
reopen cannot become success.

Commit `1b34d718` extends that exact binding discipline through reverse
admission, the physical effect, parent durability, and journal persistence,
and admits the RootLinks source across all three operations and both current
and historical record epochs. `Applied` is sealed inside the durable authority
only after one reconciled reverse exchange; exact `PRE` evidence instead seals
`AlreadySatisfied`, and the persistence caller cannot supply either outcome.
The bound `UsrRestored` publication checks its successor binding in the same
store, retains it across destruction of that store, and checks the exact inode
and record again after canonical reopen. Focused matrices cover both outcomes,
all five bound-update faults, predecessor and successor replacement seams, and
fresh restart convergence without a second effect.

Fresh RootLinks entries now progress exactly from `RootLinksComplete` to
`RollbackDecided`, then `ReverseExchangeIntent`, then `UsrRestored`; the reverse
entry performs exactly one exchange and the five canonical root links retain
their targets and identities. A following entry leaves that exact
`UsrRestored` record byte-identical.

Commit `7b3770b1` hardens the common candidate-preservation passage. At that
checkpoint it did not widen the RootLinks gate. It captures one exact non-Clone
`TransitionJournalRecordBinding` inside an installation-revalidation sandwich
before namespace or database inspection, then moves it through NewState target
creation, target normalization, and candidate movement; ActivateArchived and
ActiveReblit effects; their durability and persistence-facing authorities; and
production dispatch. The slice eliminates all six coarse semantic journal
loads from that chain. A safe creation or normalization crash prefix now
returns an opaque, one-use `RestartRequired` source authority rather than
discarding the binding and loading the source record again.

At that checkpoint, the identical-bytes/different-inode proof had 44 pre-effect
cases, 44 post-effect cases, and 16 preparation-restart cases across current
and historical epochs, both recorded `/usr` outcomes, and both common candidate
sources, with `BootSyncStarted` admitted only for ActiveReblit. The pre-effect
matrix authorizes no operation; the post-effect matrix never converts an inode
substitution into success; and the restart matrix rejects the same bytes at a
successor inode.

Commits `fec890ad`, `c9140a88`, and `043a3c24` complete exact candidate-writer
persistence for NewState, ActivateArchived, and ActiveReblit respectively.
Each operation consumes its exact predecessor binding, derives the sole
`CandidatePreserved` successor from its private effect origin, validates that
successor in the same store, destroys the old lock-bearing store, and requires
an independently reopened canonical store to match the exact successor inode
and record inside an installation-revalidation sandwich. The operation-specific
success, storage-fault, same-byte/different-inode, and fresh-restart matrices
remain fail closed without changing established database or non-journal
namespace effects and without redispatching a new checkpoint.

Commit `67ad3de0` widens only the exact passage through `CandidatePreserved`.
For current and historical record epochs, all three operations, and both
recorded `/usr` outcomes, RootLinks-sourced `UsrRestored` now routes to
`CandidatePreserveIntent`, then the matching operation writer persists its sole
exact `CandidatePreserved` successor. The production endpoint performs exactly
one reverse `/usr` exchange across the full route, preserves all five canonical
root-link targets and identities.

The route proof rejects 360 root-link mutation races, 180 at each of its two
revalidation seams. Candidate admission separately rejects 360 mutations
spanning every one of the five canonical links. The common same-byte binding
proof is now 64 pre-effect, 64 post-effect, and 24 preparation-restart cases.
NewState and ActivateArchived each cover 24 success, 120 storage-fault, 96
predecessor/successor binding-substitution, and 48 fresh-restart writer
executions. ActiveReblit covers 32, 160, 128, and 64 respectively.

Accepted commit `e35a2183` admits only exact RootLinks-sourced NewState
`CandidatePreserved` generation 15 into the journal-only route and carries the
non-Clone record-inode binding through a bound advance to
`FreshDbInvalidationIntent` generation 16, same-store validation, old-store
destruction, and independent canonical reopen.

Accepted commit `7457b259` admits that exact generation-16 source into the
production invalidation boundary. One record-inode binding crosses capture,
Apply-or-Finish effect reconciliation, the bound advance to
`FreshDbInvalidated` generation 17, same-store successor validation, old-store
destruction, and independent canonical reopen. A present exact fresh transition
is removed at most once; proved joint absence performs zero removals. The
success, storage-fault, predecessor-or-successor binding-substitution, and
fresh-handle matrices cover 48, 240, 192, and 96 executions respectively.
Fresh-handle reopen is explicitly not process-death evidence.

Accepted commit `68759ba3` adds genuine same-boot `SIGKILL` proof only for this
RootLinks NewState generation-16 -> generation-17 boundary. Its exact 20 cases
are two record epochs x (five SQLite application-transaction seams + five
journal-update durability seams). The parent releases every installation,
journal, and database handle before separate crash and recovery children
re-execute production `CleanSystemStartup`; a 15-second deadline kills and
reaps a hung child, and the recovery child is the first SQLite opener.

The selected fresh row has a nonempty selection. SQLite rolls back deaths at
the first four database seams, so recovery observes the complete preimage and
performs one exact `Applied` removal. The post-commit database seam and all
journal seams perform zero removals: they reconcile joint absence as exact
`AlreadySatisfied`, or consume the exact already-published successor according
to raw source-versus-successor evidence. Post-crash raw temporary-file inventory
is taken before any recovery journal-store or SQLite open. All five root-link
targets and identities remain exact and namespace, exchange, and boot effects stay
zero.

Five all-link mutation seams fail closed: 240 capture, 240 pre-effect, 120
Applied post-attempt, 240 initial-persistence, and 240 final-revalidation
executions. The endpoint performs exactly one reverse `/usr` exchange and
  leaves all five canonical root-link targets and identities unchanged. At
  that checkpoint, a later NewState entry left generation 17 byte-identical
  because its RootLinks completion and terminal-finalization gates remained
  closed.

Accepted commit `a3fb25d3` independently admits exact RootLinks-sourced
ActivateArchived `CandidatePreserved` generation 11 and carries the exact
non-Clone record-inode binding from capture through one bound advance to
`RollbackComplete` generation 12, same-store successor validation, and an
independent canonical reopen. Its proof covers 24 successes, 120 storage
faults, 96 binding substitutions, 48 fresh-handle reopens, and 360 all-five-
root-ABI mutation cases across capture, fresh-namespace, and final-revalidation
seams. Database state, archived-wrapper and state-slot identities, and all five
canonical root-link identities remain unchanged. The journal-only entry invokes
no database or non-journal effect, cleanup, finalization, or boot action. A
later entry leaves generation 12 byte-identical because the RootLinks terminal-
finalization source axis remains closed.

Accepted commit `a05997d8`, with acceptance-gate follow-up `cfb5a70d`, admits
only exact RootLinks-sourced ActiveReblit `CandidatePreserved` generation 13.
It carries the exact record-inode binding from capture through one bound advance
to `RollbackComplete` generation 14, validates the successor in the same store,
destroys the old lock-bearing store, and requires an independent canonical
reopen. Its proof covers 24 successes, 120 storage faults, 96 predecessor or
successor binding substitutions, and 48 fresh-handle reopens. Another 240 cases
mutate all five root-ABI links: exactly 120 at `CaptureSandwich` and 120 at
`FinalRevalidation`; the legacy fresh-namespace-capture race remains a separate
focused contract. The full RootLinks endpoint performs exactly one reverse
`/usr` exchange and one ActiveReblit wrapper exchange. This entry performs no
boot, database, non-journal namespace, finalization, or cleanup effect. Exact
`BootSyncStarted` remains disjoint and routes to `BootRepairRequired`.
Accepted commit `f2b305d4` now admits only exact RootLinks NewState generation-17
`FreshDbInvalidated`. It captures the non-Clone predecessor record-inode
binding, consumes it through one bound advance to generation-18
`RollbackComplete`, validates the successor in the same store, drops the old
lock-bearing store, and independently reopens the canonical journal to match
the same successor inode and record. Its base-success, storage-fault,
binding-substitution, and fresh-handle matrices cover 48, 240, 192, and 96
executions. The fresh-handle lane is not process-death evidence. Another 480
cases retain all five root-ABI identities across 240 capture and 240 final-
revalidation races.

That completion entry is journal-only: database, non-journal namespace,
reverse-exchange, candidate, boot, cleanup, terminal-deletion, and finalization
effects all remain zero. NewState now stays byte-stable at generation 18,
ActivateArchived at generation 12, and ActiveReblit at generation 14 because
all RootLinks terminal-finalization gates remain closed. Commit `68759ba3` still
proves only the 20-case NewState generation-16 -> generation-17 invalidation
boundary. When its crash already made the generation-17 successor canonical,
the recovery entry may naturally take the ordinary generation-17 -> 18 route;
that does not create a completion-boundary `SIGKILL` claim. Other RootLinks
process-death boundaries, reboot, and power-loss durability remain unclaimed.
The next blocker is an exact record-bound terminal-deletion primitive before
any operation's RootLinks finalization is widened.

### Startup reconciliation and interruption campaign

The open startup-reconciliation and interruption work items, including the
completed database-ownership probes, every operation's no-boot rollback
suffix, and the ActiveReblit boot-required journal routes, continue in the
[startup-reconciliation plan](state-activation-startup-reconciliation.md).
Production startup handles exactly one entry checkpoint at a time. NewState
preparation-only target creation or normalization retains its phase; movement,
routing, exact fresh-row invalidation, completion persistence, and terminal
deletion each stop after their own boundary. ActiveReblit preserves its whole
replacement wrapper. When boot repair is not required, a later entry advances
only the journal from `CandidatePreserved` to `RollbackComplete`; that
successor is never finalized in the same entry. A further entry admits the
terminal record only with the exact `ExistingCandidate` row under `Cleared`
ownership, present immutable provenance, `previous: None`, `candidate ==
previous`, and the unchanged preserved-wrapper topology and wrapper index. Its
operation-specific finalizer retains the same continuously locked journal
store, performs one conditional deletion, authenticates public absence, and
transfers that store directly into the shared clean-startup audit. It performs
no database, non-journal namespace, trigger, cleanup, or wrapper effect. An
exact `BootSyncStarted` source instead routes the preserved candidate to
`BootRepairRequired`; independently observed `BootRepairStarted` evidence
becomes terminal `BootRepairUnverified` without another boot invocation.
Compiler-local seals prevent sibling authority construction,
and operation-specific real-gate contracts cover the complete deterministic
matrices, journal-update and terminal-delete faults, evidence races, and
fresh-handle restart.

The separate ActiveReblit terminal process lane now runs an exact 2 x 2 x 3
matrix: current and historical record epochs, both rollback sources, and three
genuine same-boot `SIGKILL` boundaries. Final-PRE death retains the exact
canonical source; death immediately after unlink exposes kernel-observed
absence before the journal-directory barrier; and death after that directory
sync exposes post-barrier absence. Fresh recovery processes enter production
startup and preserve the exact cleared existing-candidate row, provenance,
wrapper topology, and wrapper index while converging through the appropriate
source-or-absence path to clean admission.

Commit `8c22ec67` establishes two independently sealed ActivateArchived
foundations without making either production-reachable. Candidate preservation
authenticates the staged archived tree and its exact canonical state-slot hard
link, orders candidate, source-parent, destination-parent, and roots-parent
durability, and permits at most one descriptor-relative no-replace move of
only `staging/usr` into that canonical wrapper. Fresh namespace evidence,
rather than the raw syscall report, classifies the result. A closing retained-
snapshot revalidation rejects both PRE-to-POST and POST-to-PRE races after
classification without leaking retry or effect authority.

The second seal admits only exact ActivateArchived `CandidatePreserved`
evidence with distinct cleared candidate and previous-state rows plus immutable
candidate provenance. It derives the sole `RollbackComplete` successor,
performs one conditional journal advance, drops the old authority and store,
and then classifies only the exact source or successor after canonical reopen.
The child-move and completion-route lanes each pass 12 focused tests across
current and historical epochs, rollback sources, recorded outcomes, durability
faults, evidence races, and restart sides. Independent review is clean and all
1258 tracked text files remain within the 1000-line limit.

Commit `cbe3679a` makes only the candidate-preservation half production
reachable. One exact ActivateArchived `CandidatePreserveIntent` startup entry
selects Apply or Finish, crosses the operation-specific durability boundary,
and performs one conditional advance to `CandidatePreserved`. Canonical reopen
accepts only the exact source or successor after the old authority and journal
handle are destroyed. Source-durable restart observes the already-preserved
namespace and finishes without another move; successor-durable restart skips
preservation entirely. A handled entry immediately returns
`RecoveryPending`, so the separately sealed completion authority cannot run in
that same entry.

The production gate passes 11 persistence/shared-leaf tests and 11
candidate-filter tests. It covers both epochs, rollback sources, recorded
`/usr` outcomes, Apply and Finish, all five journal faults, all six final
evidence races, exact Pending inspection, cross-operation authority rejection,
and fresh-handle source/successor restart. Adjacent NewState, ActiveReblit,
reverse, target-preparation, shared-effect, completion-foundation, workspace
check, and 1321-file line-limit gates pass; independent production review is
clean.

Commit `f44c2be9` adds an exact 2 x 2 real-process matrix at the first
ActivateArchived candidate-preservation interruption boundary: current and
historical record epochs, both rollback sources, and death after the real
no-replace child move returns but before semantic recapture, POST durability,
or journal advancement. Each crash child reaches that seam through production
`CleanSystemStartup`, performs exactly one real move, and leaves the exact
`CandidatePreserveIntent` source journal canonical while the candidate is
already preserved. A fresh recovery process opens new handles, selects Finish,
makes zero second moves, completes POST durability, and persists
`CandidatePreserved(AlreadySatisfied)`. The entry returns only
`RecoveryPending`; completion and finalization remain later checkpoints.

Commit `bc6d6792` expands that production path to an exact 2 x 2 x 7 matrix.
In addition to the original post-move/pre-recapture seam, a real crash process
now dies before candidate-tree sync, each of the three retained parent
barriers, final POST capture, and the pre-persistence durable POST
revalidation. Every callback first requires exactly one move attempt. The parent
then proves the candidate is preserved with exact source journal, database,
bytes, and inode identities; a fresh recovery process takes Finish, performs zero second moves,
replays the idempotent durability suffix, and persists the exact
`CandidatePreserved(AlreadySatisfied)` successor.

The historical epoch dimension is an out-of-current-epoch journal witness in
the same boot, not a reboot simulation. Neither the terminal post-sync kills
nor any candidate-preservation kill is a power-loss oracle, so reboot and
power-loss durability remain unproved. Phase 11 and the broad interruption
campaign stay open. ActivateArchived completion
dispatch is production-wired by `c8c5ea41` as its own bounded entry. Accepted
commit `a3fb25d3` widens only that exact completion entry to RootLinks, reaches
generation-12 `RollbackComplete`, and deliberately leaves the RootLinks
terminal finalizer closed.
Commit `32bf8589` adds a separately authorized terminal checkpoint with one
same-store conditional journal delete, repeated exact-source-or-absence
classification, and same-lock clean handoff. Commit `c6362aae` adds the exact
12-case real-process terminal matrix across current and historical epochs,
both rollback sources, and final-PRE, post-unlink, and post-directory-sync
same-boot `SIGKILL` boundaries. It does not simulate reboot or power loss;
later rollback, roll-forward, durable boot publication, production boot-repair
wiring, cleanup, other earlier interruption boundaries, reboot, and
power-loss-equivalent durability work remain.

The [canonical Phase 11 exit gate](../../PLAN.md#phase-11-make-state-activation-crash-recoverable)
remains authoritative in `PLAN.md`.
