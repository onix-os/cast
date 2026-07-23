set +e
run_wrapper signal 10 >"$work/signal.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-SIGNAL' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper signal-supervisor 10 >"$work/supervisor-signal.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-SUPERVISOR-SIGNAL' "$work/supervisor-signal.out"
if ! grep -Fq 'latched supervisor reaped command and watchdog (status 143)' \
    "$evidence/fixtures-ci.log"; then
    printf '%s\n' 'bounded supervisor-signal log did not retain the final supervisor receipt:' >&2
    cat "$evidence/fixtures-ci.log" >&2
    exit 1
fi
assert_bounded_inventory

set +e
CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS=1 \
    run_wrapper freeze-supervisor 10 >"$work/frozen-supervisor.out" 2>&1
frozen_status=$?
set -e
frozen_supervisor_pid=$(cat "$outer_state/supervisor-pid")
frozen_unit=$(cat "$outer_state/unit")
test "$frozen_status" -eq 124
if process_is_live "$frozen_supervisor_pid"; then
    printf 'autonomously bounded fixture supervisor remained live: %s\n' \
        "$frozen_supervisor_pid" >&2
    exit 1
fi
grep -Fqx "$frozen_unit" "$outer_state/stops"
test ! -e "$evidence/fixtures-ci-proof.json"
grep -Fq 'status channel exceeded its 1-second bound' \
    "$work/frozen-supervisor.out"
assert_bounded_inventory

set +e
CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP=1 \
    run_wrapper success 10 >"$work/post-reap-signal.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
assert_bounded_inventory

current_case=double-signal
double_payload_gate="$gates/double-signal-payload"
double_finalize_gate="$gates/double-signal-finalize"
rm -rf "$double_payload_gate.ready" "$double_payload_gate.hold" \
    "$double_finalize_gate.ready" "$double_finalize_gate.continue" \
    "$double_finalize_gate.claim"
mkfifo -m 600 "$double_payload_gate.hold"
mkfifo -m 600 "$double_finalize_gate.continue"
tracked_gate_fifo="$double_payload_gate.hold"
tracked_gate_token=continue
set +e
FAKE_DOUBLE_SIGNAL_GATE="$double_payload_gate" \
FAKE_FINALIZE_STOP_GATE="$double_finalize_gate" \
    run_wrapper --exec double-signal 10 >"$work/double-signal.out" 2>&1 &
double_invocation_pid=$!
tracked_runner_pid=$double_invocation_pid
set -e
wait_for_receipt "$double_payload_gate.ready" "$double_invocation_pid" \
    'double-signal payload readiness'
double_wrapper_pid=$(cat "$outer_state/wrapper-pid")
test "$double_wrapper_pid" -eq "$double_invocation_pid"
process_is_live "$double_wrapper_pid"
kill -TERM "$double_wrapper_pid"
tracked_gate_fifo="$double_finalize_gate.continue"
wait_for_receipt "$double_finalize_gate.ready" "$double_wrapper_pid" \
    'double-signal finalization stop'
process_is_live "$double_wrapper_pid"
kill -TERM "$double_wrapper_pid"
printf 'continue\n' >"$double_finalize_gate.continue"
tracked_gate_fifo=
set +e
wait "$double_invocation_pid"
status=$?
set -e
tracked_runner_pid=
tracked_gate_token=
test "$status" -eq 143
require_receipt "$double_payload_gate.ready" \
    'double-signal payload readiness'
require_receipt "$double_finalize_gate.ready" \
    'double-signal finalization stop'
test -d "$double_finalize_gate.claim"
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-DOUBLE-SIGNAL' "$work/double-signal.out"
assert_bounded_inventory

rm -f "$work/late-child.pid"
signal_late_gate="$gates/signal-late-proof"
rm -rf "$signal_late_gate.ready" "$signal_late_gate.release" \
    "$signal_late_gate.payload-hold" "$signal_late_gate.drain-started" \
    "$signal_late_gate.drained" "$signal_late_gate.natural-exit"
mkfifo -m 600 "$signal_late_gate.release"
mkfifo -m 600 "$signal_late_gate.payload-hold"
set +e
FAKE_DESCENDANT_GATE="$signal_late_gate" \
    run_wrapper signal-late-proof 10 >"$work/signal-late-proof.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test -f "$work/late-child.pid"
late_child_pid=$(cat "$work/late-child.pid")
require_receipt "$signal_late_gate.ready" \
    'signal late-proof writer readiness'
require_receipt "$signal_late_gate.drain-started" \
    'signal late-proof writer drain start'
require_receipt "$signal_late_gate.drained" \
    'signal late-proof writer drain completion'
test ! -e "$signal_late_gate.natural-exit"
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
if process_is_live "$late_child_pid"; then
    printf 'detached late-proof writer outlived its bounded test window: %s\n' \
        "$late_child_pid" >&2
    exit 1
fi
grep -Fq 'BEGIN-SIGNAL-LATE-PROOF' "$work/signal-late-proof.out"
# The public log deliberately retains only its final byte window; later cleanup
# receipts may evict this early marker. Other cases prove captured-log content.
assert_bounded_inventory
