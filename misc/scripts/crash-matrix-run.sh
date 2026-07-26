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
# **What is NOT proven: anything in the `state` column.** `CUTS` includes a
# `control` cell that runs the operation with no cut and shuts down cleanly. It
# also reports `state=absent` — so the package fails to install in this minimal
# guest regardless of any power cut. Without that control, "the package never
# lands after a power cut" would have read as a durability bug across every row.
# It is a guest-environment gap, not a finding.
#
# Next, in order:
#   1. Make the control cell green. **Diagnosed 2026-07-26: the guest needs
#      dbus.** The install itself works in the minimal initramfs — index, repo
#      add and the blit all succeed and it prints `Installed bash-completion` —
#      and then fails at the last step:
#
#          Error: install: protect state mutation from interruption:
#                 failed to connect to dbus: No such file or directory
#
#      Forge takes a dbus inhibitor lock so a state mutation cannot be
#      interrupted. Either run a session dbus-daemon in the guest init, or give
#      forge a way to proceed without an inhibitor when nothing can interrupt it.
#      Note the irony worth keeping in mind: the lock that exists to protect
#      against interruption is what blocks the harness built to interrupt it.
#
#      Also note the local-package flow, which is not obvious: `install` takes a
#      package *name*, not a path, so a `.stone` must first be `cast index`ed and
#      the resulting `stone.index` added with `repo add file://...`.
#
#      Until `control` reports `state=installed`, no other row's state column
#      means anything.
#   2. Move cut points from wall-clock delays to specific journal phases.
#   3. Extend OPS past install to activate-archived, active-reblit, archived
#      repair.
#
# Runs inside the approved VM only; never on the host. Staged inputs as in
# `crash-matrix-forge-guest.sh`, plus /tmp/pkg.stone.
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
MODE=$(sed -n 's/.*cell_mode=\([a-z]*\).*/\1/p' /proc/cmdline)
OP=$(sed -n 's/.*cell_op=\([a-zA-Z0-9_./-]*\).*/\1/p' /proc/cmdline | tr '_' ' ')
mount -t ext4 /dev/vda /mnt || { echo "CELL-FAIL mount"; poweroff -f; }
mkdir -p /mnt/root
# The journal correlates by boot_id; the campaign keys verdicts by the same value.
echo "BOOT-ID $(cat /proc/sys/kernel/random/boot_id)"
if [ "$MODE" = write ]; then
    echo "CELL-READY"
    cast -D /mnt/root -y $OP >/dev/null 2>&1
    echo "CELL-OP-DONE"
    while :; do sleep 1; done
elif [ "$MODE" = control ]; then
    # No cut: let the operation finish and shut down cleanly. Without this the
    # `state` column cannot be read — an absent package could equally mean the
    # cut worked or the install never works in this guest.
    cast -D /mnt/root -y $OP >/tmp/op 2>&1
    echo "CONTROL-OP-OUTPUT:"; tail -4 /tmp/op
    sync
    echo "CELL-OP-DONE"
    poweroff -f
else
    echo "CELL-VERDICT-BEGIN"
    # Two independent questions. "Does forge come up at all" is the recovery
    # verdict; "did the package land" is the transition outcome. Conflating them
    # made an uninitialised root read as a recovery failure.
    if cast -D /mnt/root repo list >/dev/null 2>&1; then R=clean; else R=FAILED; fi
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
  done
done
