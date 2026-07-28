#!/usr/bin/env bash
# Validate the crash matrix's core premise (`plans/future_impl.md` §2.1).
#
# Everything the matrix will assert rests on one claim: that hard-killing a
# nested qemu loses exactly the writes a real power cut would lose — those never
# forced to stable storage — and keeps those that were. That claim is worth
# proving before any campaign is built on it, because a harness that silently
# preserves unsynced writes would report every durability bug as "passed".
#
# Boots a busybox initramfs against a real ext4 disk, writes one forced and one
# unforced file, hard-kills the guest, reboots the same disk, and reports which
# survived. Expected: `unsynced=absent synced=present`.
#
# Two false passes were hit getting here, both worth keeping in mind when
# extending this:
#   1. `sync` is global — it flushes every dirty file, not the one you name.
#   2. Under ext4 `data=ordered`, an fsync commits the running transaction and
#      writes out every co-transaction data block with it. An unsynced write
#      issued *before* the fsync is therefore flushed too. It must be issued
#      after, into a fresh transaction that is never committed.
# Both reported `unsynced=present` and would have been read as "the power cut
# does not work" rather than "the probe is wrong".
#
# Runs inside the approved VM only; never on the host.
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

# --- initramfs: busybox + an init that writes then idles, or checks and reports
mkdir -p "$W/ir/bin" "$W/ir/proc" "$W/ir/sys" "$W/ir/mnt" "$W/ir/dev"
cp /usr/bin/busybox "$W/ir/bin/"
cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
MODE=$(sed -n 's/.*probe_mode=\([a-z]*\).*/\1/p' /proc/cmdline)
mount -t ext4 /dev/vda /mnt 2>/dev/null || { echo "PROBE-FAIL mount"; poweroff -f; }
if [ "$MODE" = write ]; then
    # Order matters, twice over:
    #  - `sync` would be global and flush both files (mistake #1).
    #  - Under ext4 `data=ordered`, an fsync commits the *running transaction*
    #    and writes out every co-transaction data block with it — so writing the
    #    unsynced file first still flushed it (mistake #2).
    # So: force the synced file first, then write the unsynced one into a fresh
    # transaction that is never committed.
    echo synced | dd of=/mnt/synced.txt conv=fsync 2>/dev/null
    echo unsynced > /mnt/unsynced.txt
    echo "PROBE-READY"
    while :; do sleep 1; done                  # wait to be killed mid-flight
else
    printf 'PROBE-RESULT unsynced=%s synced=%s\n' \
        "$([ -f /mnt/unsynced.txt ] && echo present || echo absent)" \
        "$([ -f /mnt/synced.txt ] && echo present || echo absent)"
    poweroff -f
fi
INIT
chmod +x "$W/ir/init"
( cd "$W/ir" && find . | cpio -o -H newc --quiet | gzip -9 > "$W/initrd.gz" )

# --- a real ext4 disk the guest can mount
qemu-img create -f raw "$W/d.img" 64M >/dev/null
mkfs.ext4 -q -F "$W/d.img"

run() { # mode, extra qemu args...
    local mode=$1; shift
    qemu-system-x86_64 -enable-kvm -m 512 -display none -no-reboot \
        -kernel "$KERNEL" -initrd "$W/initrd.gz" \
        -append "console=ttyS0 probe_mode=$mode" \
        -drive "file=$W/d.img,format=raw,if=virtio,cache=writeback" \
        -serial stdio -monitor none "$@"
}

echo "== phase 1: write, then hard power cut =="
run write > "$W/out1" 2>&1 &
QPID=$!
for _ in $(seq 1 60); do grep -q PROBE-READY "$W/out1" 2>/dev/null && break; sleep 1; done
grep -q PROBE-READY "$W/out1" || { echo "guest never reached READY:"; tail -5 "$W/out1"; exit 1; }
kill -KILL $QPID 2>/dev/null || true; wait $QPID 2>/dev/null || true
echo "power cut delivered"

echo "== phase 2: reboot same disk, report survivors =="
timeout 120 bash -c "$(declare -f run); W='$W' KERNEL='$KERNEL'; run check" > "$W/out2" 2>&1 || true
grep -o "PROBE-RESULT.*" "$W/out2" || { echo "no result:"; tail -5 "$W/out2"; exit 1; }
