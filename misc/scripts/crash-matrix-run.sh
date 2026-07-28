#!/usr/bin/env bash
# Crash matrix (`plans/future_impl.md` §2.1): every operation crossed with every
# cut point. Each cell drives the operation in a nested guest, hard-cuts power,
# reboots the same disk, and reports.
#
# Each cell answers two independent questions, and conflating them is a mistake
# already made once here:
#   recovery=  did forge come up at all on the power-cut disk?
#   state=     did the transition's effect actually land?
# `list installed` exits non-zero on an uninitialised root, which read as a
# recovery failure in every cell until the two were split.
#
# **What is proven (2026-07-26):** the cut is real (unsynced writes genuinely
# vanish — `crash-matrix-durability-probe.sh`), forge runs in the guest, forge
# reports `recovery=clean` after a cut at every tested point, and verdicts
# survive a reboot.
#
# **VERIFIED END TO END 2026-07-27.** With
# `CAST_ALLOW_UNINHIBITED_TRANSACTION=1` exported in the guest init, a real
# install now completes there and the matrix produces meaningful verdicts:
#
#     OPERATION   CUT       VERDICT
#     install     control   recovery=clean state=installed
#     install     0s        recovery=clean state=absent
#     install     3s        recovery=FAILED state=absent
#
# The control cell reaching `state=installed` is what makes the whole state
# column readable: absence in a cut cell now means the cut prevented the effect,
# not that the guest cannot install.
#
# **RESULT: the durability machinery works.** Verified 2026-07-27, cut 3s into a
# real install:
#
#     install  3s  recovery=PENDING driver=recovered-at-9 state=installed
#
# Read that carefully, because a first pass got it wrong. A power cut at
# `UsrExchanged` leaves a record needing rollback. Read-only commands then
# *correctly refuse* — `recovery=PENDING` is right, not a failure. A mutating
# command drives the rollback chain one durable phase per invocation:
#
#     UsrExchanged -> RollbackDecided -> ReverseExchangeIntent -> UsrRestored
#     -> CandidatePreserveIntent -> CandidatePreserved -> FreshDbInvalidationIntent -> ...
#
# It converges after 9 invocations and the install then completes:
# `state=installed`. Recovery is correct; it is simply incremental.
#
# **The earlier "recovery=FAILED, first real defect" reading was a harness
# artefact** — the probe drove recovery six times and read "not finished yet" as
# "cannot recover". Any future cell that reports a recovery failure must first
# rule this out by driving the mutating path until it either converges or
# genuinely stops advancing (the phase in the error must stop changing).
#
# Note `install` takes a package *name*, not a path: a local `.stone` is
# `cast index`ed and added via `repo add file://.../stone.index` first. Passing a
# path silently yields "no package found" and an all-absent state column.
#
# **DEFECT FOUND 2026-07-27 — pre-exchange crashes are unrecoverable.**
#
#     CUT       PHASE REACHED                  RECOVERS?
#     control   -                              yes (recovered-at-1)
#     4s        TransactionTriggersStarted     NO  (stalled)
#     5s        TransactionTriggersStarted     NO  (stalled)
#     6s        TransactionTriggersComplete    NO  (stalled)
#     8s        post-exchange                  yes (recovered-at-7)
#
# A power cut during transaction triggers — before the `/usr` exchange — leaves
# a journal record whose `recovery_disposition()` is `BeginRollback`, and no
# startup route drives it. `startup_gate` is a chain of phase-specific
# dispatchers (`active_reblit_boot_sync_started`, `usr_rollback_*`,
# `*_commit_cleanup*`) that all cover *post-exchange* phases; a pre-exchange
# record falls through every one and surfaces as `PendingSystemTransition`.
# Every subsequent invocation repeats it, so the system never recovers.
#
# This is distinguishable from slow recovery because the driver now tracks the
# *phase* in the error: a genuine stall is the phase not changing across five
# consecutive attempts. Post-exchange cuts advance
# `UsrExchanged -> RollbackDecided -> ... -> RollbackComplete` and converge.
#
# `driver=nothing-staged` is NOT a durability outcome — it means the cut landed
# before the package was indexed, so there was nothing to install and nothing to
# recover. Do not read it as a stall.
#
# Note `install` takes a package *name*, not a path: a local `.stone` is
# `cast index`ed and added via `repo add file://.../stone.index` first. Passing a
# path silently yields "no package found" and an all-absent state column.
#
# **Widened sweep, 2026-07-27** (install x six cut points):
#
#     CUT       VERDICT
#     control   recovery=clean   driver=recovered-at-1  state=installed
#     0s        recovery=clean   driver=recovered-at-1  state=installed
#     1s        recovery=clean   driver=FAILED          state=absent
#     2s        recovery=PENDING driver=FAILED          state=absent
#     3s        recovery=PENDING driver=recovered-at-7  state=installed
#     5s        recovery=PENDING driver=recovered-at-9  state=installed
#
# The 1s and 2s cells do not converge within the 15 driver invocations this
# script allows, while 0s, 3s and 5s all do. That non-monotonic shape is
# interesting — a narrow window that behaves worse than cuts on either side of
# it — but it is NOT yet a defect claim.
#
# Before treating it as one, rule out the mistake this harness already made
# once: raise the invocation cap and check whether the phase in the error keeps
# *advancing*. Recovery is incremental, so "still pending after N tries" and
# "cannot recover" look identical at a fixed N. It is only a defect if the phase
# stops changing.
#
# **`activate` operation added 2026-07-27 — and it exposes the harness's limit.**
# `OPS=(activate)` installs, then runs `cast state activate 1` to drive an
# ActivateArchived transition. Every cut from 5s to 16s recovers:
#
#     activate  control  recovered-at-1  state=installed
#     activate  10s      recovered-at-9  state=installed
#     activate  14s      recovered-at-9  state=installed
#     activate  16s      recovered-at-1  state=installed
#
# **Do not read that as "ActivateArchived pre-exchange recovery works."** The
# `recovered-at-9` signature is the same one NewState rollback produces, and the
# setup install takes several seconds, so those cuts most likely landed inside
# the *install*, not the activation. Wall-clock cuts cannot reliably target one
# operation's window when the setup that precedes it takes seconds.
#
# This is exactly why §2.1 calls for cuts targeted at journal *phases* rather
# than delays: arm an existing `arm_*` fault hook at the phase under test and
# cut there. Until that lands, this operation's cells prove the harness runs,
# not that the transition is durable.
#
# Note the pre-exchange rollback fix is deliberately scoped to NewState
# (`plans/future_impl.md` §1.4); whether ActiveReblit and ActivateArchived share
# the gap is still unmeasured.
#
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

# Operations that write durable state without needing network.
OPS=(activate)
# When to cut, relative to the operation starting. 0 = as early as possible.
CUTS=(10 12 14 16)

mkdir -p "$W/ir"/{bin,proc,sys,dev,mnt}
cp /usr/bin/busybox "$W/ir/bin/"; cp /tmp/cast "$W/ir/bin/cast"; chmod +x "$W/ir/bin/cast"
cp /tmp/libstone.so "$W/ir/bin/"; tar xzf /tmp/nixlibs.tgz -C "$W/ir" 2>/dev/null || true
cp /tmp/pkg.stone "$W/ir/pkg.stone"
cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
export LD_LIBRARY_PATH=/bin
# No session manager in this guest, so nothing can interrupt a transaction and
# forge's logind inhibitor cannot be satisfied (`plans/future_impl.md` §2.1).
export CAST_ALLOW_UNINHIBITED_TRANSACTION=1
stage_and_activate() {
    stage_and_install
    # State 1 is archived once the install created state 2; activating it back
    # drives an ActivateArchived transition.
    cast -D /mnt/root -y state activate 1 2>&1 | tail -2
}
stage_and_install() {
    mkdir -p /mnt/repo
    cp /pkg.stone /mnt/repo/
    cast index /mnt/repo 2>&1 | tail -1
    cast -D /mnt/root -y repo add local file:///mnt/repo/stone.index 2>&1 | tail -1
    cast -D /mnt/root -y install bash-completion 2>&1 | tail -2
}
MODE=$(sed -n 's/.*cell_mode=\([a-z]*\).*/\1/p' /proc/cmdline)
OP=$(sed -n 's/.*cell_op=\([a-zA-Z0-9_./-]*\).*/\1/p' /proc/cmdline | tr '_' ' ')
mount -t ext4 /dev/vda /mnt || { echo "CELL-FAIL mount"; poweroff -f; }
mkdir -p /mnt/root
# The journal correlates by boot_id; the campaign keys verdicts by the same value.
echo "BOOT-ID $(cat /proc/sys/kernel/random/boot_id)"
if [ "$MODE" = write ]; then
    echo "CELL-READY"
    case "$OP" in activate) stage_and_activate >/dev/null 2>&1 ;; *) stage_and_install >/dev/null 2>&1 ;; esac
    echo "CELL-OP-DONE"
    while :; do sleep 1; done
elif [ "$MODE" = control ]; then
    # No cut: let the operation finish and shut down cleanly. Without this the
    # `state` column cannot be read — an absent package could equally mean the
    # cut worked or the install never works in this guest.
    case "$OP" in activate) stage_and_activate ;; *) stage_and_install ;; esac
    sync
    echo "CELL-OP-DONE"
    poweroff -f
else
    echo "CELL-VERDICT-BEGIN"
    # Two independent questions. "Does forge come up at all" is the recovery
    # verdict; "did the package land" is the transition outcome. Conflating them
    # made an uninitialised root read as a recovery failure.
    # Two distinct questions. A read-only command may legitimately refuse to act
    # on a system with a pending transition, so it cannot tell us whether
    # recovery *works* — only a path that drives recovery can.
    if REC=$(cast -D /mnt/root repo list 2>&1); then R=clean; else R=PENDING; echo "READONLY: $REC"; fi
    # Recovery appears to advance one phase per invocation, so drive it
    # repeatedly and report whether it converges or stalls.
    D=FAILED
    # Recovery is incremental: one durable phase per invocation. So "still
    # pending after N tries" and "cannot recover" are indistinguishable at a
    # fixed N — that confusion already produced one false defect report. Track
    # the phase in the error instead: a genuine stall is the phase *not*
    # changing across several attempts.
    PREV_PHASE=""; STALL=0
    for attempt in $(seq 1 60); do
        if DRV=$(cast -D /mnt/root -y install bash-completion 2>&1); then D=recovered-at-$attempt; break; fi
        # "no package found" means the cut landed before the repo was indexed,
        # so there is nothing to install and nothing to recover. That is not a
        # durability outcome and must not be reported as a stall.
        case "$DRV" in *"no package found"*) D=nothing-staged; break ;; esac
        PHASE=$(echo "$DRV" | grep -oE 'at [A-Za-z]+ requires' | head -1 | awk '{print $2}')
        if [ "$PHASE" = "$PREV_PHASE" ]; then
            STALL=$((STALL + 1))
            if [ "$STALL" -ge 5 ]; then D=stalled-at-${PHASE:-unknown}; echo "STALL: $(echo "$DRV" | tail -1 | cut -c1-200)"; break; fi
        else
            STALL=0
            echo "PHASE-$attempt: ${PHASE:-?}"
        fi
        PREV_PHASE=$PHASE
    done
    if cast -D /mnt/root list installed 2>/dev/null | grep -q bash-completion; then S=installed; else S=absent; fi
    echo "recovery=$R driver=$D state=$S"
    echo "CELL-VERDICT-END"
    poweroff -f
fi
INIT
chmod +x "$W/ir/init"
( cd "$W/ir" && find . | cpio -o -H newc --quiet | gzip -1 > "$W/initrd.gz" )

run() { exec qemu-system-x86_64 -enable-kvm -m 2048 -display none -no-reboot \
    -kernel "$KERNEL" -initrd "$W/initrd.gz" \
    -append "console=ttyS0 cell_mode=$1 cell_op=$2" \
    -drive "file=$W/d.img,format=raw,if=virtio,cache=writeback" -serial stdio -monitor none; }

printf '%-20s %-6s %s\n' OPERATION CUT VERDICT
for op in "${OPS[@]}"; do
  enc=${op// /_}
  for cut in "${CUTS[@]}"; do
    qemu-img create -f raw "$W/d.img" 256M >/dev/null; mkfs.ext4 -q -F "$W/d.img"
    if [ "$cut" = control ]; then
        timeout 240 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run control '$enc'" > "$W/o1" 2>&1 || true
    else
    run write "$enc" > "$W/o1" 2>&1 & QPID=$!
    for _ in $(seq 1 90); do grep -q CELL-READY "$W/o1" 2>/dev/null && break; sleep 1; done
    sleep "$cut"
    kill -KILL $QPID 2>/dev/null || true; wait $QPID 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 $QPID 2>/dev/null || break; sleep 1; done; sleep 2
    fi
    timeout 600 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run check '$enc'" > "$W/o2" 2>&1 || true
    # `|| true`: a missing verdict makes grep exit non-zero, and under `set -e`
    # the assignment inherits that and kills the run with no output at all.
    v=$(grep -oE 'recovery=[A-Za-z]+ driver=[A-Za-z0-9-]+ state=[A-Za-z]+' "$W/o2" | head -1 || true)
    printf '%-20s %-6s %s\n' "$op" "${cut}s" "${v:-NO-VERDICT}"
    if [[ ${v:-} == *FAILED* || ${v:-} == *stalled* || -z ${v:-} ]]; then
        echo "--- verdict phase output ---"; sed -n '/CELL-VERDICT-BEGIN/,/CELL-VERDICT-END/p' "$W/o2"
    fi
  done
done
