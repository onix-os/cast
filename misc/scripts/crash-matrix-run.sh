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
# **FIRST REAL DEFECT FOUND (2026-07-27).** The 3s cut reproduces reliably and
# the startup error is:
#
#     Error: repo: setup client: establish clean system-client startup baseline:
#       state transition <id> at UsrExchanged requires BeginRollback { source:
#       UsrExchanged }; recovery effects remain blocked by []
#
# A power cut at `UsrExchanged` leaves a journal record that startup correctly
# identifies as needing rollback — and then refuses to proceed, reporting an
# **empty** blocker list. An empty `blocked by []` alongside a refusal to
# recover is self-contradictory: either something blocks recovery and should be
# named, or nothing does and recovery should run. Control and 0s cells in the
# same run recover cleanly, so this is specific to being interrupted with the
# exchange durable but the transition unfinished.
#
# Not yet diagnosed further. See `plans/future_impl.md` §2.1.
#
# Note `install` takes a package *name*, not a path: a local `.stone` is
# `cast index`ed and added via `repo add file://.../stone.index` first. Passing a
# path silently yields "no package found" and an all-absent state column.
#
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

# Operations that write durable state without needing network.
OPS=("install_/pkg.stone" "repo_list")
# When to cut, relative to the operation starting. 0 = as early as possible.
CUTS=(control 0 1 3)

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
    stage_and_install >/dev/null 2>&1
    echo "CELL-OP-DONE"
    while :; do sleep 1; done
elif [ "$MODE" = control ]; then
    # No cut: let the operation finish and shut down cleanly. Without this the
    # `state` column cannot be read — an absent package could equally mean the
    # cut worked or the install never works in this guest.
    stage_and_install
    sync
    echo "CELL-OP-DONE"
    poweroff -f
else
    echo "CELL-VERDICT-BEGIN"
    # Two independent questions. "Does forge come up at all" is the recovery
    # verdict; "did the package land" is the transition outcome. Conflating them
    # made an uninitialised root read as a recovery failure.
    if REC=$(cast -D /mnt/root repo list 2>&1); then R=clean; else R=FAILED; echo "RECOVERY-ERROR: $REC"; fi
    if cast -D /mnt/root list installed 2>/dev/null | grep -q bash-completion; then S=installed; else S=absent; fi
    echo "recovery=$R state=$S"
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
    timeout 180 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run check '$enc'" > "$W/o2" 2>&1 || true
    v=$(grep -oE 'recovery=[A-Za-z]+ state=[A-Za-z]+' "$W/o2" | head -1)
    printf '%-20s %-6s %s\n' "$op" "${cut}s" "${v:-NO-VERDICT}"
    if [[ ${v:-} == *FAILED* || -z ${v:-} ]]; then
        echo "--- verdict phase output ---"; sed -n '/CELL-VERDICT-BEGIN/,/CELL-VERDICT-END/p' "$W/o2"
    fi
  done
done
