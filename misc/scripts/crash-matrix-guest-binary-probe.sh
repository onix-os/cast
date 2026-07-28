#!/usr/bin/env bash
# Prove a dynamically-linked binary and its shared-library closure run inside the
# nested guest (`plans/future_impl.md` §2.1).
#
# Forge is not statically linked, so the crash matrix cannot put it in a busybox
# initramfs by copying one file. This builds the initramfs the campaign needs:
# the target binary plus its full `ldd` closure and the dynamic loader, each at
# the absolute path the interpreter expects rather than a rewritten rpath.
#
# Verified 2026-07-26 with `/bin/ls`, which reported
# `ls (uutils coreutils) 0.8.0` from inside the guest — loader, libraries and
# execution all working, in a 7.6M initramfs.
#
# Note the basename handling: multi-call binaries (busybox, coreutils) dispatch
# on argv[0], so the target must keep its original name. Copying it as a fixed
# name made `ls` fail with "unknown program", which reads as a broken initramfs
# rather than a renamed binary.
#
# Runs inside the approved VM only; never on the host.
set -euo pipefail
BIN=${1:?usage: guest-binary-probe.sh /path/to/binary [args...]}
shift || true
W=$(mktemp -d); chmod 700 "$W"; trap "rm -rf '$W'" EXIT
KERNEL=$(ls /boot/vmlinuz-* | head -1)

mkdir -p "$W/ir"/{bin,proc,sys,dev,mnt,lib,lib64,usr/lib}
cp /usr/bin/busybox "$W/ir/bin/"
# Keep the original basename: multi-call binaries (busybox, coreutils)
# dispatch on argv[0], so renaming breaks them.
BASE=$(basename "$BIN")
cp "$BIN" "$W/ir/bin/$BASE"
printf '%s\n' "$BASE" > "$W/ir/target-name"

# Copy the ldd closure. Paths are preserved so the interpreter finds them where
# the binary expects, rather than relying on a rewritten rpath.
ldd "$BIN" 2>/dev/null | grep -oE '/[^ ]+\.so[^ ]*' | sort -u | while read -r lib; do
    [ -f "$lib" ] || continue
    mkdir -p "$W/ir$(dirname "$lib")"
    cp -L "$lib" "$W/ir$lib"
done
# The dynamic loader itself.
INTERP=$(ldd "$BIN" 2>/dev/null | grep -oE '/lib64/ld-linux[^ ]*|/lib/ld-linux[^ ]*' | head -1)
[ -n "$INTERP" ] && { mkdir -p "$W/ir$(dirname "$INTERP")"; cp -L "$INTERP" "$W/ir$INTERP"; }

cat > "$W/ir/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
echo "GUEST-BINARY-BEGIN"
TARGET=$(cat /target-name)
"/bin/$TARGET" --version 2>&1 | head -3 || echo "GUEST-BINARY-EXEC-FAILED rc=$?"
echo "GUEST-BINARY-END"
poweroff -f
INIT
chmod +x "$W/ir/init"
( cd "$W/ir" && find . | cpio -o -H newc --quiet | gzip -9 > "$W/initrd.gz" )
echo "initramfs: $(du -h "$W/initrd.gz" | cut -f1)"

timeout 120 qemu-system-x86_64 -enable-kvm -m 1024 -display none -no-reboot \
    -kernel "$KERNEL" -initrd "$W/initrd.gz" -append "console=ttyS0" \
    -serial stdio -monitor none 2>&1 | sed -n '/GUEST-BINARY-BEGIN/,/GUEST-BINARY-END/p'
