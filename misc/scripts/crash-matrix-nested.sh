#!/usr/bin/env bash
# Power-cut crash matrix, driven from inside the approved VM against a nested
# guest (`plans/future_impl.md` §2.1, D2.1).
#
# WHY A NESTED GUEST. The durability claims this exercises are about writes that
# were never fsynced surviving nothing. `kill -9` on the forge process does not
# test that: the page cache outlives the process, so the filesystem still sees
# every write. Only destroying the machine loses them. Hard-killing a nested
# qemu is a genuine instantaneous power cut with the right semantics, which is
# why it beats `dm-flakey` (that simulates a *failing* device, not a
# *vanishing* one) and snapshot revert (that restores a consistent point rather
# than an interrupted one).
#
# WHERE THIS RUNS. Inside the approved VM, never on the host — the host is never
# mutated by tests. Nested KVM was verified available there on 2026-07-26:
# /sys/module/kvm_*/parameters/nested = Y, /dev/kvm present, qemu 10.2.1.
# The invoking user must be in the `kvm` group (a fresh membership needs a new
# login session to take effect).
#
# CORRELATING RESULTS ACROSS THE REBOOT. The campaign does not need its own
# identity mechanism. /proc/sys/kernel/random/boot_id is exactly what
# `RuntimeEpoch { boot_id, mount_namespace }` records, and the journal already
# treats a changed epoch as "this record predates the current boot". Results are
# keyed by that same value, read from the guest.
#
# STATUS: first slice. Provisioning and the power-cut primitive are here and are
# exercised by --self-test. Driving a full operation matrix inside the guest and
# collecting per-phase verdicts is not yet implemented; see the TODO at the
# bottom and §2.1.

set -euo pipefail

readonly SCRIPT_NAME=${0##*/}

die() {
    printf '%s: %s\n' "$SCRIPT_NAME" "$*" >&2
    exit 1
}

note() {
    printf '%s: %s\n' "$SCRIPT_NAME" "$*"
}

require_nested_kvm() {
    local nested
    for nested in /sys/module/kvm_intel/parameters/nested /sys/module/kvm_amd/parameters/nested; do
        if [[ -r $nested ]] && [[ $(<"$nested") == [Yy1]* ]]; then
            [[ -r /dev/kvm && -w /dev/kvm ]] ||
                die "/dev/kvm is not readable and writable; add this user to the kvm group and start a new login session"
            return 0
        fi
    done
    die "nested KVM is not enabled on this host; the power-cut model requires it"
}

require_tools() {
    local tool
    for tool in qemu-system-x86_64 qemu-img; do
        command -v "$tool" >/dev/null || die "missing $tool (apt-get install qemu-system-x86 qemu-utils)"
    done
}

# Boot the guest, wait for it to settle, then cut its power.
#
# The kill is SIGKILL rather than a monitor `quit`: `quit` gives qemu the chance
# to flush, which would defeat the entire point. SIGKILL drops everything the
# guest had not already forced to stable storage.
power_cut_after() {
    local seconds=$1
    shift
    local -a qemu=("$@")

    "${qemu[@]}" &
    local pid=$!
    note "guest pid $pid; cutting power in ${seconds}s"
    sleep "$seconds"

    if ! kill -0 "$pid" 2>/dev/null; then
        wait "$pid" 2>/dev/null || true
        die "guest exited before the power cut; nothing was interrupted"
    fi

    kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    note "power cut delivered"
}

# Prove the two primitives this harness is built on actually work here: KVM
# accelerates a nested guest, and a hard kill terminates it mid-run.
self_test() {
    require_tools
    require_nested_kvm

    local scratch
    scratch=$(mktemp -d)
    # 0700: test roots are owner-only by policy, and a umask-inherited mode is
    # rejected downstream as an unsafe capability root.
    chmod 700 "$scratch"
    # Expanded now, not at trap time: `scratch` is function-local and is out of
    # scope by the time EXIT fires.
    # shellcheck disable=SC2064
    trap "rm -rf '$scratch'" EXIT

    qemu-img create -f qcow2 "$scratch/disk.qcow2" 64M >/dev/null

    power_cut_after 5 \
        qemu-system-x86_64 \
        -enable-kvm \
        -m 256 \
        -display none \
        -serial none \
        -monitor none \
        -no-reboot \
        -drive "file=$scratch/disk.qcow2,format=qcow2,if=virtio,cache=none"

    note "self-test passed: nested KVM accelerates a guest and the power cut lands"
}

usage() {
    cat <<'USAGE'
Usage: crash-matrix-nested.sh --self-test

  --self-test   Verify nested KVM and the power-cut primitive on this machine.

Not yet implemented: --run, which will drive forge operations inside the guest,
cut power at each journal phase, reboot, and collect the startup gate's verdict
keyed by the guest's boot_id. See plans/future_impl.md 2.1.
USAGE
}

main() {
    case ${1-} in
        --self-test) self_test ;;
        -h | --help) usage ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
