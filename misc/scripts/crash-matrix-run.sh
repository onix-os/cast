#!/usr/bin/env bash
# Crash matrix (`plans/future_impl.md` §2.1): every operation crossed with every
# cut point, each cell running the loop proven in `crash-matrix-cell.sh` —
# drive the operation in a nested guest, hard-cut power, reboot the same disk,
# report whether forge recovers.
#
# Verified 2026-07-26: 3 operations x 3 cut points, 9/9 `recovery=clean`.
#
# **Read that result carefully.** All-clean here does not yet mean the durability
# machinery is proven. The operations currently in `OPS` are cheap and largely
# read-only, so they write little durable state and there is correspondingly
# little for a power cut to tear. The harness is what has been demonstrated: the
# cut is real (unsynced writes genuinely vanish — see
# `crash-matrix-durability-probe.sh`), forge runs in the guest, and verdicts come
# back across a reboot.
#
# The valuable next step is replacing `OPS` with real state-mutating
# transitions — install, activate-archived, active-reblit, archived repair — and
# replacing the wall-clock `CUTS` with cuts targeted at specific journal phases.
# Those are the cells that can actually fail, and the ones §2.1 exists to run.
# Until then this reports "the harness works", not "the transitions are durable".
#
# Runs inside the approved VM only; never on the host. Staged inputs as in
# `crash-matrix-forge-guest.sh`.
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

# Operations that write durable state without needing network.
OPS=("repo list" "list installed" "boot status")
# When to cut, relative to the operation starting. 0 = as early as possible.
CUTS=(0 1 3)

mkdir -p "$W/ir"/{bin,proc,sys,dev,mnt}
cp /usr/bin/busybox "$W/ir/bin/"; cp /tmp/cast "$W/ir/bin/cast"; chmod +x "$W/ir/bin/cast"
cp /tmp/libstone.so "$W/ir/bin/"; tar xzf /tmp/nixlibs.tgz -C "$W/ir" 2>/dev/null || true
cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
export LD_LIBRARY_PATH=/bin
MODE=$(sed -n 's/.*cell_mode=\([a-z]*\).*/\1/p' /proc/cmdline)
OP=$(sed -n 's/.*cell_op=\([a-z_]*\).*/\1/p' /proc/cmdline | tr '_' ' ')
mount -t ext4 /dev/vda /mnt || { echo "CELL-FAIL mount"; poweroff -f; }
mkdir -p /mnt/root
# The journal correlates by boot_id; the campaign keys verdicts by the same value.
echo "BOOT-ID $(cat /proc/sys/kernel/random/boot_id)"
if [ "$MODE" = write ]; then
    echo "CELL-READY"
    cast -D /mnt/root $OP >/dev/null 2>&1
    while :; do sleep 1; done
else
    echo "CELL-VERDICT-BEGIN"
    cast -D /mnt/root repo list >/dev/null 2>&1 && echo "recovery=clean" || echo "recovery=FAILED"
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

printf '%-16s %-6s %s\n' OPERATION CUT VERDICT
for op in "${OPS[@]}"; do
  enc=${op// /_}
  for cut in "${CUTS[@]}"; do
    qemu-img create -f raw "$W/d.img" 256M >/dev/null; mkfs.ext4 -q -F "$W/d.img"
    run write "$enc" > "$W/o1" 2>&1 & QPID=$!
    for _ in $(seq 1 90); do grep -q CELL-READY "$W/o1" 2>/dev/null && break; sleep 1; done
    sleep "$cut"
    kill -KILL $QPID 2>/dev/null || true; wait $QPID 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 $QPID 2>/dev/null || break; sleep 1; done; sleep 2
    timeout 180 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run check '$enc'" > "$W/o2" 2>&1 || true
    v=$(grep -oE 'recovery=[A-Za-z]+' "$W/o2" | head -1)
    printf '%-16s %-6s %s\n' "$op" "${cut}s" "${v:-NO-VERDICT}"
  done
done
