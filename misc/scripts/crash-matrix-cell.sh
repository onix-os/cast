#!/usr/bin/env bash
# One complete crash-matrix cell (`plans/future_impl.md` §2.1): drive a real
# forge operation inside the nested guest, cut power mid-flight, reboot the same
# disk, and read the recovery verdict.
#
# This is the loop the full matrix repeats. It composes three separately
# verified pieces: the power cut loses exactly the unsynced writes it should
# (`crash-matrix-durability-probe.sh`), a dynamically-linked binary runs in the
# initramfs (`crash-matrix-guest-binary-probe.sh`), and forge itself executes
# there (`crash-matrix-forge-guest.sh`).
#
# Verified 2026-07-26. Phase 2 reported `No repositories have been configured
# yet` with rc=0 — forge came up cleanly on a power-cut disk.
#
# Three traps cost real time here; all are load-bearing and commented inline:
#   1. `run` must `exec` qemu. Backgrounding a plain function yields the
#      *subshell's* pid, qemu survives the kill as its child, and phase 2 dies on
#      the image write-lock — which reads as a harness bug, not a stale process.
#   2. `wait` returns before qemu tears down that lock, so phase 2 needs a
#      settle loop even once the pid is right.
#   3. The operation must not need network. A repo add against an unreachable
#      URL errors out and leaves nothing durable to test.
#
# To extend into the matrix: parameterise the write-phase operation and the
# instant of the cut (per journal phase), and collect verdicts keyed by the
# guest's /proc/sys/kernel/random/boot_id.
#
# Runs inside the approved VM only; never on the host. Staged inputs as in
# `crash-matrix-forge-guest.sh`.
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

mkdir -p "$W/ir"/{bin,proc,sys,dev,mnt}
cp /usr/bin/busybox "$W/ir/bin/"; cp /tmp/cast "$W/ir/bin/cast"; chmod +x "$W/ir/bin/cast"
cp /tmp/libstone.so "$W/ir/bin/"
tar xzf /tmp/nixlibs.tgz -C "$W/ir" 2>/dev/null || true

cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
export LD_LIBRARY_PATH=/bin
MODE=$(sed -n 's/.*cell_mode=\([a-z]*\).*/\1/p' /proc/cmdline)
mount -t ext4 /dev/vda /mnt || { echo "CELL-FAIL mount"; poweroff -f; }
if [ "$MODE" = write ]; then
    mkdir -p /mnt/root
    # A real durable write with no network dependency: initialising the
    # installation root creates and fsyncs its state databases.
    cast -D /mnt/root repo list 2>&1 | tail -2
    echo "--- root after operation ---"
    find /mnt/root -maxdepth 3 | head -12
    echo "CELL-READY"
    while :; do sleep 1; done          # killed here, mid-flight
else
    echo "CELL-VERDICT-BEGIN"
    cast -D /mnt/root repo list 2>&1 | tail -5
    echo "rc=$?"
    echo "CELL-VERDICT-END"
    poweroff -f
fi
INIT
chmod +x "$W/ir/init"
( cd "$W/ir" && find . | cpio -o -H newc --quiet | gzip -1 > "$W/initrd.gz" )

qemu-img create -f raw "$W/d.img" 256M >/dev/null; mkfs.ext4 -q -F "$W/d.img"
# `exec` matters: without it, backgrounding `run` yields the *subshell's* pid,
# qemu survives the kill as its child, and phase 2 fails on the image
# write-lock. With exec, the backgrounded pid is qemu itself.
run() { exec qemu-system-x86_64 -enable-kvm -m 2048 -display none -no-reboot \
    -kernel "$KERNEL" -initrd "$W/initrd.gz" -append "console=ttyS0 cell_mode=$1" \
    -drive "file=$W/d.img,format=raw,if=virtio,cache=writeback" -serial stdio -monitor none; }

echo "== drive operation, then cut power =="
run write > "$W/o1" 2>&1 & QPID=$!
for _ in $(seq 1 90); do grep -q CELL-READY "$W/o1" 2>/dev/null && break; sleep 1; done
grep -q CELL-READY "$W/o1" || { echo "never reached READY:"; tail -8 "$W/o1"; exit 1; }
grep -E "^(Added|Error|error)" "$W/o1" | head -2 || true
kill -KILL $QPID 2>/dev/null || true; wait $QPID 2>/dev/null || true
# `wait` returns before qemu has torn down its image write-lock, so phase 2
# would fail with "Failed to get write lock" on a fast enough restart.
for _ in $(seq 1 30); do kill -0 $QPID 2>/dev/null || break; sleep 1; done
sleep 2
echo "power cut delivered"

echo "== reboot, read verdict =="
timeout 180 bash -c "$(declare -f run); W='$W'; KERNEL='$KERNEL'; run check" > "$W/o2" 2>&1 || true
if grep -q CELL-VERDICT-BEGIN "$W/o2"; then
    sed -n '/CELL-VERDICT-BEGIN/,/CELL-VERDICT-END/p' "$W/o2"
else
    echo "no verdict reached; guest tail:"; tail -12 "$W/o2"
fi
