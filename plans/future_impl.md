# Implementation Plan: FUTURE_PLAN.md closure

Concrete, sequenced plan to implement every item in
[`../FUTURE_PLAN.md`](../FUTURE_PLAN.md). Grounded in four codebase-research
passes (boot-recovery, package/build/archive, evaluator/config/installation,
maintenance/test-ops). Sequencing is **durability-gap-first** by explicit
decision; the two most dangerous items (real boot repair, reboot/power-loss VM
campaigns) are **in scope with full care** — gated behind prerequisites, proven
only in the disposable VM, and each dangerous step confirmed before execution.

The forbidden list from FUTURE_PLAN.md stays forbidden throughout: no host-disk
mutation, no ambient host/`/nix/store` fixture mounts, no mutable untracked
recipe mounts, no fake tool shims, and no same-boot test described as reboot or
power-loss proof.

Legend — **E** effort (S/M/L/XL), **R** risk (low/med/high/critical),
**D** = open decision I need from you before that item starts.

### Resolved decisions
- **D2.3 → prerequisites first.** Real boot repair lands the receipt-bound
  partial-residue authentication + exact-pending-receipt-head DB op *before* any
  boot-mutation effect; boot mutation is confirmed with the user before wiring.
- **D3.1 → strict.** Toolchain-free means *zero* compiler tools in the locked
  closure/identity (analyzer objcopy/strip stay, selected separately); a `Shell`
  step invoking `cc` in a toolchain-free package is a validation error.
- **D4.3 → accept global bumps.** Keep the single global
  `EVALUATOR_POLICY_VERSION`; a policy tightening re-fingerprints all Gluon
  evaluations. No per-adapter policy id.
- **D6.x → design sketch + trigger.** Phase 6 items stay concrete design specs
  with explicit reconsideration triggers; no speculative implementation.

---

## Phase 0 — Safe prerequisites · **CLOSED 2026-07-27**

Verified item by item rather than assumed; every one is already satisfied, and
the section was actively misleading about the state of the tree.

- **0.1** — `startup_reconciliation_database_phase_matrix_is_exact` passes.
- **0.2** — the `forge-focused-tests.mk` line naming the removed test is gone.
- **0.3** — `flake.nix:22` pins `rust-bin.stable."1.94.1"` exactly and owns the
  rustfmt policy in the flake, as the item asked.
- **0.4** — asks to resolve 18 forge warnings; the production build emits
  **zero**.
- **0.5** — `misc/scripts/lib/host-scratch-root.sh` exists and every host script
  now uses it. The last `${TMPDIR:-/tmp}` fallback
  (`test-support/write-fixtures-ci-proof-v2.sh`) was routed through it on
  2026-07-27 — worth having done, since `/tmp` saturation genuinely halted work
  during this epic.
- **0.6** — already dropped.

Original text retained below for provenance.

## Phase 1 — Durability closure (the biggest real gap; leads by your decision)

Today **only ActiveReblit is journal-durable.** `NewState` and `ActivateArchived`
run with **no journal at all** — a crash mid-transition survives no reboot,
recoverable only by same-boot in-process logic. The coordinator's own module doc
names durable startup reconciliation as the precondition for live wiring. This
phase closes that.

### Phase 1 — shipped (detail removed 2026-07-29)

Everything below was implemented and merged; the planning detail is gone, the
findings that outlived it are kept. `close_out.md` was deleted on 2026-08-10;
current state and open work live in the task list.

- **1.1 / 1.1a-1.1e — NewState durable coordinator.** Shipped, including first
  install. 1.1e needed two fixes, not one: the namespace policy (D1.5) and the
  in-flight marker, which only predecessor archiving cleared — so a first install
  left a marker making every install look interrupted.
- **1.2 / 1.2a / 1.2b — ActivateArchived.** Phase model, archived-staging pair
  and coordinator shipped. **The route had zero callers until 2026-07-29** —
  `cast state activate` used the legacy non-journalled path, so this operation's
  durability was theoretical. Now wired, and guest-level proof landed 2026-08-10:
  four of five crash cells recover; a cut at `ArchivedCandidateStagingIntent`
  still stalls on a namespace-policy conflict.
- **1.3 — Archived repair.** Interruption marker shipped. D1.3 resolved: a
  journal record was not viable because the forward chain crosses `/usr`
  unconditionally (`validation.rs:535`), so a lighter marker was correct.
- **1.4 — Forward cleanup crash-safety.** Answered by measurement, and it found
  a real defect: crashes during transaction triggers were unrecoverable. Fixed
  across five layers of hard-coded post-exchange assumptions.

**Standing hazards this phase produced — these are why the detail is worth
remembering even though the sections are gone:**

- Phase ordering was duplicated across three tables, two invisible to a
  `.ordinal()` grep. Now one source of truth. Keep it that way.
- Generation expectations were hard-coded in production
  (`root_abi_publication`, the rollback finalization authority). Now derived.
- Rollback admission repeated the same post-exchange assumption across 20 call
  sites. Now two operation-aware predicates. When extracting a predicate, grep
  for every raw copy of the condition — one left behind survived two rounds of
  narrowing and silently accepted an invalid plan.
- A dead-code warning from the production build proves nothing: confirm with
  `cargo build -p forge --tests` before deleting anything.
- Measure, do not derive from assumed arithmetic. Instrumenting found two root
  causes after repeated code-reading had failed.

## Phase 2 — Live boot proof, real boot repair, and destructive VM campaigns

The heaviest, most dangerous phase. **Both dangerous items are in scope, with
full care.** Nothing here runs on the host; all destructive evidence is
disposable-VM-only; each dangerous step is confirmed with you before running.

### 2.1 Reboot-capable VM campaign harness  · E:XL R:high  (prerequisite for 2.2–2.4)
The current harness (`misc/vm/disposable-uefi-boot-*`) **explicitly never
reboots** and depends on a persistent SSH session. Real durability proof needs a
harness that survives SSH loss.
**Approach:** add a reboot-capable orchestration layer that (i) persists a
self-describing on-disk campaign state + a **first-boot resume service** that
re-attaches and reports (serial-console evidence as the SSH-independent channel),
(ii) injects interruption at each `BootSyncStarted`/receipt-promotion/
`BootSyncComplete` boundary, (iii) verifies the post-reboot startup gate
reconciles the journal to a bootable state. Keep the existing static guards
(no `ssh|virsh|reboot` inside the assert scripts; disposable-disk-only).
**D2.1 (need your input on VM model):** (a) power-loss model admissible as
"equivalent" — qemu hard `quit`, `fsync`-fault (`dm-flakey`), or nested-VM
snapshot revert? (b) is nested KVM available in the approved VM, or must
interruption be driven from the host hypervisor (current rules forbid `virsh`
inside scripts)? (c) how to authenticate campaign identity/results across a
reboot that severs SSH — first-boot resume service + serial evidence?

**D2.1 — measured in the VM, 2026-07-26.** Two of the three sub-questions now
have factual answers; only (a) is still a judgement call.

- **(b) nested KVM is available.** `/sys/module/kvm_*/parameters/nested` reads
  `Y` and `/dev/kvm` is present; the VM has 12 cores and 7 GB RAM. So
  interruption can be driven from *inside* the approved VM against a nested
  guest, and does not need host-hypervisor access — which keeps the "no `virsh`
  on the host from scripts" rule intact.
  **Caveat:** the tooling is not installed. `virsh` and `qemu-system-x86_64` are
  both absent and `libvirtd` is inactive. Provisioning qemu/libvirt inside the
  VM is a prerequisite for the harness.
- **(c) campaign identity across a reboot is already modelled.** The kernel
  exposes `/proc/sys/kernel/random/boot_id` (observed
  `7b1f4f54-2fd4-4596-8205-2241f0a62251`), which is exactly what
  `RuntimeEpoch { boot_id, mount_namespace }` records and validates against. A
  reboot changes it, and the journal already treats a changed epoch as the
  signal that a record predates this boot. So the campaign does not need a new
  identity mechanism — it needs to *read* the same value the journal does, and
  correlate results by it.
- **(a) still open, but narrowed.** With a nested guest, `virsh destroy` (or
  qemu `quit`) is a genuine instantaneous power cut: everything not fsynced is
  lost, which is the semantics the durability work actually claims. That is a
  stronger and simpler model than `dm-flakey` fsync-fault injection, which
  simulates a *failing* device rather than a *vanishing* one. Snapshot revert is
  weaker still — it restores a consistent point rather than an interrupted one.
  Recommendation: nested-guest hard destroy, with `dm-flakey` kept in reserve
  for targeted single-fsync-failure cases the coordinator claims to survive.

**Harness slice landed 2026-07-26:** `misc/scripts/crash-matrix-nested.sh`.

Done and verified running inside the VM:
- qemu 10.2.1 + qemu-utils provisioned; the invoking user added to `kvm`.
- `--self-test` proves both primitives the matrix rests on: nested KVM really
  accelerates a guest, and a SIGKILL power cut lands mid-run. Exits 0 and
  cleans up its scratch root.
- The cut is SIGKILL, never a monitor `quit` — `quit` would let qemu flush,
  defeating the point. This is why `kill -9` on the forge process is *not* a
  substitute: the page cache outlives the process, so the filesystem still sees
  every write. Only destroying the machine loses them.

Still to build for a real campaign:
- A guest image with forge installed, and a way to drive one operation to a
  chosen journal phase before the cut.
- Per-phase cut scheduling across the operation matrix (NewState,
  ActivateArchived, ActiveReblit, archived repair).
- Reboot-surviving result collection, keyed by the guest's
  `/proc/sys/kernel/random/boot_id`, plus the startup-gate verdict per run.

The archived-repair marker (§1.3) is a natural first target: it is the newest
durability claim and has no crash coverage yet.

### 2.1a `receipt_promotion::completion` concurrency bug  · E:M R:med

Long treated as full-suite flake; it is a real bug. 2-7 tests under
`receipt_promotion::completion::*` fail in parallel runs with
`boot-topology typed value or evaluation fingerprint changed`
(`require_exact_evaluation`, `active_reblit_boot_topology_intent.rs`), on a path
inside the test's own tempdir. Membership shifts run to run within that module.

**Diagnosed 2026-07-26 as shared mutable state between concurrent tests:**

- `cargo test -p forge receipt_promotion::completion` reproduces in ~100s
  (3 of 27 fail). Use this, not the ~29-minute full suite. The interference is
  therefore *inside* the `completion` submodule — only 27 tests to bisect.
- The same run with `-- --test-threads=1` passes 61/61. Serialising the module
  fixes it, which is the discriminator.

Ruled out: the 30s `BINDING_TIMEOUT` deadline; `remaining_at_admission` (reaches
only error messages, never the resource policy); every `EvaluationIdentity`
field (all content-derived); the `arm_*` hooks and the fixture assessment queue
(all `thread_local!`); global statics in the boot/evaluation stack (none exist). Also ruled out: a shared Lua/Gluon
evaluator VM (neither evaluator holds process-global state), and process-global
`umask` mutation (`tree_marker.rs:931` is correctly isolated in a re-exec'd
child process; no other call site exists). No `set_current_dir` anywhere.

**Partly fixed 2026-07-26.** Two distinct causes found; a third remains.

**Residual resolved 2026-08-10.** The `active_reblit_boot_inputs_tests.rs` file-descriptor
reuse bug described below is fixed, and the same defect in `asset_snapshots_tests.rs` was
fixed in `f60c0d8b`. The class is now swept crate-wide. `make test` pins 16 threads; a bare
`cargo test` defaults to 24 on a 24-core box and invents failures.

1. **Fixed — wall-clock was hashed into evaluation identity.** All four intent
   evaluators set `limits.timeout = remaining.min(MAX_EVALUATION_TIME)`, and
   that timeout is hashed into `resource_policy_sha256`, hence into
   `EvaluationIdentity`. Whenever `remaining` exceeded the 2s constant the `min`
   clamped and hid it; once evaluation ran long enough for `remaining` to drop
   below 2s, preparation and revalidation hashed differently and revalidation
   failed with "typed value or evaluation fingerprint changed" on source that
   never changed. This was a **production** bug, not a test artefact. Now a
   fixed constant; the absolute deadline is still enforced by the budget.
2. **Fixed — production budgets were inherited by a 24-way parallel suite.**
   Added `client/boot/timeout_policy.rs`: production values stay as written and
   only the test build scales them (×20). Production runs one boot publication
   at a time; the suite runs ~24 concurrently on a shared machine, where
   contention alone exhausted 30s budgets.
3. **Open.** At `--test-threads=24`, 2 of 27 still fail with `DeadlineExceeded`
   and a **consistently ~4ms** `remaining_at_admission` on the boot-topology
   intent. That value is suspiciously constant rather than load-dependent, so
   the governing deadline is armed shortly before admission from a source not
   yet identified — it is not `BINDING_TIMEOUT`, `BOOT_TOPOLOGY_TIMEOUT`,
   `BOOT_PUBLICATION_TIMEOUT` (all scaled), not the per-budget test `clock`
   (a struct field, not thread-local, so it cannot leak), and not any fixture
   constant in the receipt-promotion test support.

State after the two fixes, measured on the full workspace suite:

- **forge at 16 threads: 2757 passed, 1 failed** (was 8-10).

  **The residual is diagnosed, and it is a test bug — not a budget.**
  `active_reblit_boot_inputs_tests.rs:498`
  (`failed_final_revalidation_drops_every_prepared_snapshot_descriptor`)
  captures a raw file-descriptor *number* and then asserts the descriptor was
  closed via:

      assert_eq!(fcntl(descriptor, FcntlArg::F_GETFD), Err(Errno::EBADF));

  File-descriptor numbers are reused process-wide. Under concurrency another
  test opens a file, is handed the same number, and `F_GETFD` succeeds — so the
  assertion fails even though the descriptor under test was closed correctly.
  That is precisely why it only fails in whole-suite runs: more concurrent
  tests, higher chance of reuse. No amount of budget scaling can fix it.

  **Fixed 2026-07-27.** The test now records what the descriptor *points at*
  (`/proc/self/fd/N` dev+ino) at capture time, and accepts either `EBADF` or a
  different identity — both prove the plan dropped its descriptor, while "still
  open on the original inode" remains a failure. Coverage is unchanged and the
  race is gone.

  **Whole-suite state after that fix: still 2757/1, but a different test.** The
  descriptor test no longer appears; the remaining failure is
  `receipt_promotion::completion::drift::final_return_revalidation_catches_late_drift_after_durable_completion`
  — a straggler from the original completion cluster, which the two budget fixes
  reduced from 8-10 to this one. Same reproduction as §2.1a: it needs the
  whole-suite thread count, passes in isolation and at its own module.

  **Better reproduction, and the obvious explanation ruled out (2026-07-27).**
  `cargo test -p forge receipt_promotion::completion -- --test-threads=16`
  **alone** fails 4-5 of 27 — more than the whole-suite run — because in
  isolation those 27 run far more concurrently than when spread across 2758. At
  `--test-threads=24` it is 7-8.

  Every failure is `DeadlineExceeded` with `remaining_at_admission` in the
  *milliseconds*. The boot budgets are provably scaled x20 in test builds
  (`timeout_policy::tests::the_test_build_actually_scales_budgets` now asserts
  this, so it cannot silently regress), putting them at 600s+. A 2ms remainder
  is impossible from any scaled budget.

  So the governing deadline is **not** one of the 13 scaled boot budgets — it is
  armed elsewhere, near the point of use, and that source is still unfound.
  Next step: instrument `BootTopologyIntentBudget::new_until` to print its
  incoming `deadline` and caller. Three rounds of auditing constants have failed
  to find it; measure instead.
- `receipt_promotion::completion` is 27/27 at 16 threads and below; 24 fails 2.
- `make test` now defaults to `TEST_THREADS ?= 16`, overridable
  (`make test TEST_THREADS=1`) for bisects.

**Separately — `mason` has 3 pre-existing failures unrelated to any of this.**
`planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete`
fails with `Git(Error(RepositoryDepth { limit: 0 }))`, plus two
`upstream::git::fixture_import_tests` cases. They fail **identically at 1 thread
and at 16**, which is what `make test` already did before the thread-cap change,
so the cap did not cause them. They do stop `make test --workspace` before it
reaches forge — worth fixing or marking, or full-workspace runs never exercise
the forge suite at all.

### 2.2 Live `Ready`-branch boot regression  · E:L R:med
No single regression drives `Client::verify → complete_active_reblit_boot →
finalize` end-to-end (CI resolves boot inputs to `NotApplicable`; the VM test
drives the low-level publisher directly and is forbidden from finalization).
**Approach:** an admissible boot-topology fixture (real UEFI/GPT in the VM, or a
hermetic loopback-ESP) so `prepare_until` returns `Ready` under real
`Client::verify`, asserting the journal walks `BootSyncStarted → … → Complete →
finalized`. **D2.2:** VM (real UEFI) vs host-side loopback-ESP hermetic fixture —
which counts as "live-client" without being misread as reboot proof?

### 2.3 Real startup boot repair  · E:XL R:critical  ⚠ most dangerous item
Production currently fails closed: `BootRepairRequired → BootRepairStarted →
BootRepairUnverified → ManualBootRepair`, emitting **no** `BootRepairComplete`
(the success edge `boot_repair_complete_successor(Applied)` exists but has no
producer). Implementing real repair means minting a **new authenticated
boot-mutation authority** on an already-failed rollback path — re-publishing the
restored state's boot entries to ESP/XBOOTLDR, verifying, then advancing to
`BootRepairComplete`.
**Approach (staged, gated):**
1. First land the **receipt-bound partial-residue prerequisites**
   (FUTURE_PLAN.md:203–214): bind every private `.stage`/`.replace` residue name
   to its exact pending receipt; add one conditional DB op clearing only the
   exact pending receipt head while retaining its committed predecessor.
2. Then a narrow "re-publish the restored (predecessor) state" repair effect,
   bound to the exact journal record + ESP identity, behind a new authenticated
   authority that is structurally forbidden from touching anything but the exact
   target — never the failed candidate's residue.
3. Only after 2.1 exists, prove it across real reboot + power-loss in the VM.
**D2.3:** (a) is the automatic pending-receipt rollback protocol a **hard
prerequisite**, or can a narrower repair land first? (b) what authenticates that
the restored state's boot entries are the correct repair target vs. failed-candidate
residue? I will **confirm with you before wiring any boot-mutation effect** and
keep the safe fail-closed-to-manual default until it is proven.

### 2.4 Destructive VM evidence campaigns  · E:XL R:high
Using 2.1's harness: selected-payload bootability, interruption at every persisted
boot-publication boundary, reboot recovery (startup gate resuming a durable
journal), power-loss-equivalent durability. Then rerun the Phase-11 exit gate and
record exact accepted commits, machine identity, target-disk identity, logs,
hashes, and explicit non-claims (FUTURE_PLAN.md:57–59). No same-boot result
described as reboot/power-loss proof.

---

## Phase 3 — Package/build model

### 3.1 Toolchain-free package mode  · E:M R:med  D
Prebuilt-artifact packages still freeze the full LLVM/GNU toolchain into their
closure + identity (`build/root.rs:297`, `planner.rs:261`, `lock_resolution.rs`).
**Approach:** typed `ToolchainSpec::None` (or `toolchain_free` option) →
`inputs_for` skips toolchain/compiler blocks, `freeze_toolchain_commands` returns
typed-absent, identity binds a distinct `"none"` toolchain value; a
`plan_checks` rule that a toolchain-free plan carries no compiler roles. Bump
`DERIVATION_PLAN_SCHEMA_VERSION`/`BUILD_LOCK_SCHEMA_VERSION` + update 64-example
goldens. **D3.1:** (a) strict "no compiler in closure" vs "no structured compile
phase" (may a toolchain-free package still `Shell` `cc`?); (b) keep analyzer
tools (objcopy/strip for debug-split) — yes, selected separately; (c) new
`ToolchainSpec` variant vs option flag (v3 ABI surface).

### 3.2 Contentful prebuilt-ELF execution fixture  · E:M R:low
Pairs with 3.1 as its execution proof. Commit a deterministic ELF archive
fixture under `tests/fixtures/gluon/execution/archives`; add
`execution_prebuilt_elf.rs` (model: `execution_go_module.rs`) exercising the
existing ELF analysis (`package/analysis/handler/elf.rs`: `DT_NEEDED`, interp,
`split_debug`) + byte-identical rebuilt Stones. **D3.2:** produce the ELF via a
bootstrap build step vs commit a pinned prebuilt binary (no compiler in hermetic
env); which interpreter/libc soname to assert.

### 3.3 Content-addressed local directory/file source ABI  · E:L R:med  D
Add `UpstreamSpec::LocalTree { path, digest, destination }` (+ `LocalFile`) with
matching `SourceResolution::Local`/`LockedSource::Local`, reusing the existing
Git tree normalization+hashing (`upstream/git/materialization/normalization_hashing.rs`,
binds type/mode/symlink) as the digest, and the fixture-import copy-from-cache
mechanics promoted to production. **D3.3:** content must be pre-seeded into the
content-addressed store by digest (recipe references a digest, never a mutable
recipe-dir path — the forbidden mount); symlink absolute/out-of-tree admit/reject
rule.

### 3.4 Wrapper helper  · E:S R:low  (demand-gated: 16/64 recipes repeat the shim)
Gluon-only helper in `cast.package.v3` building the exported-env exec-shim +
`install -Dm755` from typed `{ target_exe, env, installed_path }`, lowering to the
existing `StepSpec::Shell`. No Rust ABI change. Do not copy Nix helper names.

### 3.5 Structured Python/Go builders  · E:M-per-builder R:low  (demand-gated)
Add structured `StepSpec` variants + `BuilderEnvironmentSpec` entries for the
highest-repeated shell-based systems (Python PEP517, Go module), mirroring
CMake/Meson. Incremental/opt-in; `Shell` stays valid. **D3.5:** Python vs Go
first; how much layout variability before it loses to `Shell`.

---

## Phase 4 — Evaluator / config (implementable-now wins + the versioning decision)

### 4.1 Boot-topology v1→v2 migration helper  · E:M R:low  D
v2 is the stable authoritative ABI; v1 `aliases_esp` took a bare partuuid string,
v2 requires a full `PartitionSelector{partuuid, mount_point}`. **Approach:** an
offline converter that evaluates the v1 form, synthesizes the missing
`mount_point`(s), and emits **reviewed canonical Gluon** (reuse the repository
canonical-encoder pattern) — human-review output only, never a runtime fallback
or a second accepted ABI. **D4.1:** safe default mount points to synthesize
(refuse-and-prompt on ambiguity vs guess `/efi`/`/boot`); lives as a `cast`
subcommand vs one-shot tool.

### 4.2 Canonical `repo list` output  · E:S R:low  D
The field-complete canonical encoder already exists (`repository/gluon.rs:321`
`encode_specs`); the CLI `list` (`cli/repo.rs:192`, explicit TODO) prints ad-hoc
strings. **Approach:** wire `list()` to `RepositoryCodec::encode` behind a
`--canonical`/`--gluon` flag, preserving `GLUON_GENERATED_MARKER` + generated-slot
ownership so it round-trips; keep the human summary as default. Verify
`enabled`↔`active` mapping. **D4.2:** replace vs supplement the human summary;
emit the generated marker header or a bare fragment.

### 4.3 Policy-layer pure-array capability tightening  · E:S code / M evidence  D
One-line removal of `enable_array_primitives()` (`build_policy/layers/gluon.rs:43`)
— the layers ABI never imports `std.array.prim`. Blocker is identity versioning.
**D4.3 (load-bearing for the whole evaluator section):** `EVALUATOR_POLICY_VERSION`
is a single global constant; bumping it re-fingerprints *every* Gluon evaluation.
Introduce a **per-adapter/per-ABI evaluator-policy id** (so only the layers ABI's
identity changes) — recommended — vs accept a global bump. Ride the neutral-identity-v2
cutover or land as an independent policy-version step? Before/after evidence:
a layer source with `import! std.array.prim` currently admitted → denied, while
canonical `build_policy_layers.glu` and existing manifests keep byte-identical
identities.

### 4.4 Separately-mounted `/etc` / `/etc/cast`  · E:M (after design) R:med  D
Rooted loading rejects mount crossings via `RESOLVE_NO_XDEV`
(`config/declaration/root_slot.rs:18`). **Approach:** accept an explicitly
supplied, descriptor-authenticated config-root descriptor (operator hands in a
pre-opened trusted fd), authenticate *that* instead of blindly crossing; keep the
default `NO_XDEV` rejection. **D4.4:** how the config-root descriptor is
declared/authenticated (fstab-like vs CLI fd); cross-mount trust model
(device allow-list / fs-magic / ownership); reconcile the fixed-root loader's
*missing* `NO_XDEV` vs the rooted loaders that have it (intentional?).

### 4.5 Read-only repository-manager backend  · E:M R:med  D
Generic read-only DB primitives already exist (`db/read_only.rs`,
deserialize-into-memory + `SQLITE_READONLY` proof) but the *repository* manager
lacks a strict `mode=ro` reader + trusted-owner model. **Approach:** a `mode=ro`
(or deserialize-image) repo-manager reader anchored on an explicit trusted-owner
policy (reuse cache/installation ownership checks), taking only shared locks —
never relaxing the writable manager's lock/mode/owner checks. **D4.5:** `mode=ro`
file open vs the existing deserialize model; who is a trusted owner (root-owned
index readable by unprivileged users?).

---

## Phase 5 — Security-sensitive design + implementation

Each needs a design decision before code; I'll write a concrete design spec, then
implement on your approval.

### 5.1 Authenticated adoption flow for nonempty unmanaged `/usr`  · design → E:L R:high  D
`TransitionOrigin::Unmanaged` currently maps to `Quarantine`;
`require_empty_or_marker_only` (`active_state_snapshot.rs:650`) refuses to bless
unowned content. **Spec:** bounded-inventory adoption with explicit operator
intent + durable per-file/inventory ownership provenance + a recovery path that
never converts ambiguity into authority, assuming a hostile same-UID pre-creator
(`transition_identity.rs:222`). Must not weaken `require_empty_or_marker_only`.

### 5.2 Typed SELinux/IMA/EVM/security-xattr policy  · design → E:L R:high  D
**No** xattr/label handling exists today (blit applies only mode; security xattrs
silently stripped). **Spec:** labels as typed package/system data, authenticated
through descriptor-rooted inventories, applied via `lsetxattr` in the blit
metadata phase (order-sensitive vs `fchmod` for IMA/EVM), no pathname fallback.
Touches `.stone` layout payload — coordinate with 6.3 (`.stone` V2). Precedes any
labeled-candidate-filesystem support.

### 5.3 Privilege separation / hostile same-UID isolation  · design → R:high  D
Current contract defends *cooperating* same-credential writers, not a hostile
same-UID adversary (conceded at `transition_identity.rs:222`). **Spec:** an
explicit hostile-same-UID threat model + evaluation of privilege separation
(helper uid), user namespaces, seccomp, or `FS_IMMUTABLE`/verity to close the
same-UID pre-creation window. Do not claim the cooperative contract is a kernel
freeze.

---

## Phase 6 — Deferred design/research (concrete specs + reconsideration triggers)

For each: a concrete design sketch + the exact trigger that would move it into
active work. These stay design-only until their trigger fires (per FUTURE_PLAN.md
"requires a separate decision").

- **6.1 Fixed-output network ABI** — plumbing exists (`NetworkMode` enum inert;
  planner mapping present; validation rejects). Spec: output-digest contract +
  bounded transfer + isolation + reproducible cache reusing the Phase-3.3
  content store. **Trigger:** a real recipe that cannot be expressed as a locked
  source (none demonstrated; go/cargo/node vendored examples disprove the need).
- **6.2 Provider-resolution alternative** — current resolver is deterministic
  first-match single-identity (`registry/transaction.rs`). Spec: any replacement
  must *become* the resolver, preserve first-match/selection semantics +
  build-lock reproducibility + single identity. **Trigger:** a demonstrated
  version-conflict/optional-dep case first-match can't express.
- **6.3 `.stone` V2 format migration** — single-version today
  (`stone/src/header:V1`). Spec: header `V2` dispatch across `stone`+`libstone`+
  `stone.h`, readers, upgrade policy, reproducibility + rollback evidence
  (older Cast already rejects via `UnknownVersion`). Coordinate with 5.2 (labels
  may be the V2 payload). **Trigger:** a concrete new payload need (labels /
  compression / larger fields) + current recovery contracts closed.
- **6.4 Recursive policy-overlay fixed points** — one-way add/replace/modify only.
  Spec: finite, explainable, Stone-native (no Nix fixpoints). **Trigger:** a real
  package family the one-way model can't express without duplication.
- **6.5 `PackageInputs -> PackageSpec` function ABI + callPackage reflection** —
  inputs are identity-only today (hashed, never VM-injected). Spec: first make
  inputs a typed VM-injected argument (still folded into identity), *then*
  reflection preserving typed missing/extra-input errors + deterministic
  provenance. **Trigger:** decision to make inputs a real argument at all.
- **6.6 Eval-time fetching / import-from-derivation** — hermeticity is structural
  (empty VM, forbidden fs/io/net modules, frozen derivation). Spec: separately
  versioned capability with complete hermeticity/locking/recursion/failure model,
  outside the pure initial evaluator. **Trigger:** likely indefinite; a concrete
  need that a locked source cannot serve.
- **6.7 Global/user policy discovery** — single explicit rooted `data_dir/policy`.
  Spec: only if every discovered layer seals into `EvaluationIdentity` reproducibly
  without ambient `$HOME`. **Trigger:** a real multi-layer requirement.
- **6.8 Direct Gluon provenance for structured build steps** — diagnostics already
  keep real script spans (guardrail satisfied); steps have argv identity but no
  per-step evaluated-source identity. Spec: add provenance once a stable per-step
  source identity is defined. **Trigger:** that identity being defined. Do not
  regress into recipe-text scanning.
- **6.9 Nix interoperability / lazy package-set architecture** — comparative
  research only. Spec: none; keep [`bsd.md`](bsd.md)-style as archived study.
  **Trigger:** a concrete Stone-native limitation a smaller interop layer can't
  solve — via a separate architecture decision.
- **6.10 Declarative services/users/kernel/per-machine composition** — beyond
  package sets today. Spec: a future typed module system preserving the `/usr`
  vs `/etc` boundary + atomic rollback (no imperative activation scripts).
  **Trigger:** after package + crash-recoverable activation contracts (Phases 1–2)
  close; a concrete workflow requiring it. Multi-version coexistence / profile
  generations as a separate store/profile design that must not weaken the
  one-live-tree model.
- **6.11 More Stone-native recipe patterns** — demand-gated; the natural new gaps
  are exactly Phase 3's toolchain-free + local-source + prebuilt-ELF patterns.

---

## Phase 7 — Test-ops tooling + remaining maintenance

### 7.1 `timeout` wrapper sweep  · E:M R:low  D
119 files. Remove `timeout <N>s` prefixes on deterministic helpers
(`grep/test/rg/sed/awk/mkdir/rm/...`) and on compilation/`--list` invocations;
**keep** bounds on `$(CARGO) test` that runs fixtures, `systemd*`/`udevadm`, and
fixture-runner scripts (things that can genuinely hang). Per-file classification,
not a blind sed. **D7.1:** a `CARGO_TEST_TIMEOUT` macro to centralize the kept
bounds so the remove-sweep is unambiguous?

### 7.2 VM hygiene audit  · E:M R:low  (build only if VM campaigns become routine — Phase 2 makes them so)
Read-only reporter: leftover `cast-fixtures-ci-*`/`cast-delegated-fixture-*`
`--user` units + procs, VM test-disk mounts (mountinfo), `loginctl` linger, the
Ubuntu AppArmor userns setting. Reuse the UID-scoping from
`stop-owned-fixture-unit.sh`. Report-only.

### 7.3 VM capacity preflight  · E:S R:low
`df --output=avail,iavail` / `stat -f` preflight with thresholds, wired as a make
prerequisite that refuses the outer unit before compilation. **D7.3:** thresholds;
which filesystem (`target/` build root vs `/var/tmp` VM build root).

---

### 7.4 Source-comment backlog (`TODO`/`FIXME`)  · E:S-per-item R:low

Filed from `plans/cleanup_legacy.md` §6 so no source comment survives without a
plan reference. Each is small, independent, and blocks nothing. Grouped by
nature; the file:line is the anchor, not a promise about scope.

**Correctness / behaviour (decide before changing):**
- `vfs/src/tree/mod.rs:147` — duplicate-path detection is downgraded from an
  error to an `eprintln!` (`return Err(e)` commented out). Re-enabling makes
  currently-succeeding installs fail. **D-CL6** in `cleanup_legacy.md`.
- `dag/src/subgraph.rs:123` — cycle breaking is unimplemented.
- `forge/src/registry/plugin/active.rs:39` and
  `forge/src/registry/plugin/active.rs:81` — two unhandled error paths.
- `forge/src/registry/plugin/cobble.rs:99` — unverified flag choice.
- `libstone/src/lib.rs:156` — error handling unimplemented.
- `container/src/lib.rs:548` — replace the catch-all error with finer variants.

**Parsing gaps (known-wrong inputs):**
- `mason/src/draft/metadata/github.rs:106` — string version prefixes unhandled.
- `mason/src/draft/metadata/gitlab.rs:106` — project name embedded in the
  version is unhandled.

**API / ergonomics:**
- `forge/src/cli/repo.rs:60` — the `repo` CLI API wants a full overhaul; the
  current shape is explicitly temporary. (Its canonical-output TODO is already
  closed by `1218c00a`.)
- `forge/src/client/cache.rs:244` — return an `Unpacked` value owning `blit`.
- `forge/src/registry/plugin/repository.rs:31` — replace mutation with a
  type-safe construction.
- `forge/src/cli/search.rs:398` — search binary names by default.
- `forge/src/client/prune.rs:116` — report "no states to be removed".
- `forge/src/client/sync.rs:163` — surface the "why" of system-intent packages.
- `forge/src/client/postblit.rs:235` — cache under `/var/`.
- `stone/src/write.rs:21` — allow plain encoding.
- `container/src/mounts/syscalls.rs:40` — prefer a real API over the current
  approach.

**Blocked on upstream Rust:**
- `forge/src/util.rs:266` — adopt `try {}` once stable.

**Cosmetic:**
- `mason/src/build/job/phase.rs:70` — output formatting.

---

## Cross-cutting sequencing summary

```
Phase 0  (safe prereqs, incl. flake.nix pin + stale-fixture fix)   ── do first
Phase 1  (durability: NewState/ActivateArchived/repair → journal)  ── your priority
Phase 2  (reboot harness → live Ready test → real boot repair → VM campaigns)
Phase 3  (toolchain-free + prebuilt-ELF, local-source ABI, wrappers, builders)
Phase 4  (boot-topology v1→v2, canonical repo-list, policy-version, /etc, ro-repo)
Phase 5  (security design+impl: /usr adoption, SELinux/IMA, privilege sep)
Phase 6  (deferred design specs + triggers)
Phase 7  (timeout sweep, VM hygiene/capacity tooling)
```

Phase 2's real-boot-repair (2.3) and VM campaigns (2.4) are the highest-risk work
and are explicitly in scope **with per-step confirmation**. Phases 3–4 can run in
parallel with Phase 2's long VM campaigns since they touch disjoint code.

## Open decisions I still need from you

Blocking or shaping, by phase: **D0.2** (make-gate test rename/removed),
**D0.4** (dead-code keep vs delete), **D1.1** (NewState receipt/rollback
semantics — RESOLVED: one operation-neutral boot route), **D1.4** (NewState
pre-allocation boot applicability — RESOLVED: assess from selections), **D1.3** (archived-repair record weight), **D2.1** (VM power-loss
model + nested-KVM + cross-reboot identity), **D2.2** (VM vs loopback-ESP for the
Ready test), **D2.3** (pending-receipt protocol as hard prerequisite? — I confirm
before any boot-mutation), **D3.1** (toolchain-free strictness + encoding),
**D3.3** (local-source digest-seeding + symlink policy), **D4.1** (boot-topology
default mount points), **D4.3** (per-adapter vs global evaluator-policy version —
load-bearing), **D4.4/4.5** (config-root + repo trusted-owner models),
**D5.1/5.2/5.3** (security design confirmations), **D7.1/7.3** (timeout macro,
capacity thresholds).

### Session plan (decided 2026-07-26)

Order: item 1 first (it gates verification of everything else), then 2, 4, 3.

1. **Root-cause the `completion` concurrency bug properly** — no serialisation
   workaround. Bisect the 27 tests to find the shared mutable state.
2. **Port `stateful_trigger_preparation_never_follows_a_replaced_isolation_root`
   to the coordinated route, prove it still catches the substitution, then
   delete `stateful_transition.rs`.** Never delete the security proof first.
3. **Build the full crash matrix** — every operation crossed with every journal
   phase, not a single scenario.
4. **Insert the archived-staging phases at ordinals 1-2 and shift the rest**,
   with a dedicated audit pass over every `ordinal()` caller to catch the
   silent-degradation risk.
