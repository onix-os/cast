#!/usr/bin/env bash
# Run the real forge binary inside the nested guest (`plans/future_impl.md` §2.1).
#
# Composes the two verified probes: `crash-matrix-durability-probe.sh` proved a
# hard kill loses exactly the unsynced writes a power cut would, and
# `crash-matrix-guest-binary-probe.sh` proved a dynamically-linked binary runs in
# the initramfs. This puts forge itself in that guest, which is the last piece
# needed before a crash cell can drive a real operation.
#
# Verified 2026-07-26: reported `cast 0.27.0` from inside the guest, 35M
# initramfs, 2048M guest.
#
# **The binary is nix-linked.** Its loader and libraries live at `/nix/store/...`
# paths that do not exist on the Ubuntu VM, so the `ldd` closure must ship at its
# absolute paths (see the `nixlibs.tgz` staging below). A plain copy of `cast`
# fails to execute with no useful diagnostic, because the interpreter itself is
# missing — expect that symptom if the closure is stale after a rebuild.
#
# Staged inputs, uploaded to the VM before running:
#   /tmp/cast         stripped `target/debug/cast` (233M -> 89M)
#   /tmp/libstone.so  its sibling shared object
#   /tmp/nixlibs.tgz  `tar -P` of the ldd closure, extracted at absolute paths
#
# Runs inside the approved VM only; never on the host.
set -euo pipefail
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

mkdir -p "$W/ir"/{bin,proc,sys,dev,mnt}
cp /usr/bin/busybox "$W/ir/bin/"
cp /tmp/cast "$W/ir/bin/cast"; chmod +x "$W/ir/bin/cast"
cp /tmp/libstone.so "$W/ir/bin/"
# The binary is nix-linked: its loader and libraries live at /nix/store paths
# that do not exist on this host, so the closure ships at its absolute paths.
tar xzf /tmp/nixlibs.tgz -C "$W/ir" 2>/dev/null || true

cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
export LD_LIBRARY_PATH=/bin
echo "FORGE-GUEST-BEGIN"
/bin/cast --version 2>&1 | head -3 || echo "FORGE-EXEC-FAILED rc=$?"
echo "FORGE-GUEST-END"
poweroff -f
INIT
chmod +x "$W/ir/init"
( cd "$W/ir" && find . | cpio -o -H newc --quiet | gzip -1 > "$W/initrd.gz" )
echo "initramfs: $(du -h "$W/initrd.gz" | cut -f1)"

timeout 180 qemu-system-x86_64 -enable-kvm -m 2048 -display none -no-reboot \
    -kernel "$KERNEL" -initrd "$W/initrd.gz" -append "console=ttyS0" \
    -serial stdio -monitor none 2>&1 | sed -n '/FORGE-GUEST-BEGIN/,/FORGE-GUEST-END/p'
