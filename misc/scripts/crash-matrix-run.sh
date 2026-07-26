#!/usr/bin/env bash
# Crash matrix (`plans/future_impl.md` §2.1): every operation crossed with every
# cut point, each cell running the loop proven in `crash-matrix-cell.sh` — drive
# the operation in a nested guest, hard-cut power, reboot the same disk, report.
#
# Each cell answers two independent questions, and conflating them is a mistake
# already made once here:
#   recovery=  did forge come up at all on the power-cut disk?
#   state=     did the transition's effect actually land?
# An uninitialised root makes `list installed` exit non-zero, which read as a
# recovery failure until the two were split.
#
# Verified 2026-07-26 with a real 165K `.stone` install fixture:
# `recovery=clean` in every cell — forge recovers from a hard power cut at every
# tested point. That result is real and is the durability property §2.1 exists
# to check.
#
# **`state=absent` in every cell is NOT yet a finding.** There is no control run
# proving this install *succeeds* in this guest without a cut, so absent may mean
# "the cut prevented it" or "the install never works here". Add that control
# before reading anything into the state column. The cut at 0s also lands
# immediately after CELL-READY, which is printed before the operation starts, so
# the early cells almost certainly kill the install before it does durable work.
#
# To make the state column meaningful: add a no-cut control row, then move the
# cut points from wall-clock delays to specific journal phases, and extend OPS
# past install to activate-archived, active-reblit and archived repair.
#
# Runs inside the approved VM only; never on the host. Staged inputs as in
# `crash-matrix-forge-guest.sh`, plus /tmp/pkg.stone.
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

# Operations that write durable state without needing network.
OPS=("install_/pkg.stone" "repo_list")
# When to cut, relative to the operation starting. 0 = as early as possible.
CUTS=(0 1 3)

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
    while :; do sleep 1; done
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
    run write "$enc" > "$W/o1" 2>&1 & QPID=$!
    for _ in $(seq 1 90); do grep -q CELL-READY "$W/o1" 2>/dev/null && break; sleep 1; done
    sleep "$cut"
    kill -KILL $QPID 2>/dev/null || true; wait $QPID 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 $QPID 2>/dev/null || break; sleep 1; done; sleep 2
    timeout 180 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run check '$enc'" > "$W/o2" 2>&1 || true
    v=$(grep -oE 'recovery=[A-Za-z]+ state=[A-Za-z]+' "$W/o2" | head -1)
    printf '%-20s %-6s %s\n' "$op" "${cut}s" "${v:-NO-VERDICT}"
  done
done
