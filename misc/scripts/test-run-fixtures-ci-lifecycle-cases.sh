set +e
run_wrapper success 10 >"$work/success.out" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
    cat "$work/success.out" >&2
    exit 1
fi
grep -Fq 'BEGIN-SUCCESS' "$work/success.out"
grep -Fq 'END-SUCCESS' "$work/success.out"
test "$(grep -c '^bounded-success-line-' "$work/success.out")" -eq 80
if ! tr -d '\000' <"$work/success.out" | cmp -s - "$work/success.out"; then
    printf '%s\n' 'redirected fixture output contains a NUL hole' >&2
    exit 1
fi
grep -Fq 'END-SUCCESS' "$evidence/fixtures-ci.log"
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory

# Parent death before command completion must be observed by the private
# release monitor. It stops the authenticated unit, terminates the supervisor
# group, and lets the independent logger finish on FIFO EOF.
current_case=parent-sigkill-active
set +e
run_wrapper --exec parent-sigkill-active 10 \
    >"$work/active-parent-sigkill.out" 2>&1 &
active_invocation_pid=$!
tracked_runner_pid=$active_invocation_pid
set -e
active_probe=0
while [ ! -f "$outer_state/active-parent-ready" ]; do
    active_probe=$((active_probe + 1))
    test "$active_probe" -le 300
    sleep 0.01
done
active_wrapper_pid=$(cat "$outer_state/wrapper-pid")
test "$active_wrapper_pid" -eq "$active_invocation_pid"
active_supervisor_pid=$(cat "$outer_state/supervisor-pid")
active_payload_pid=$(cat "$outer_state/payload-pid")
active_logger_pid=
active_logger_probe=0
while [ -z "$active_logger_pid" ] && [ "$active_logger_probe" -lt 300 ]; do
    active_logger_probe=$((active_logger_probe + 1))
    children_path="/proc/$active_wrapper_pid/task/$active_wrapper_pid/children"
    if [ -r "$children_path" ]; then
        for child_pid in $(cat "$children_path"); do
            child_command=
            if [ -r "/proc/$child_pid/cmdline" ]; then
                child_command=$(tr '\000' ' ' 2>/dev/null \
                    <"/proc/$child_pid/cmdline" || :)
            fi
            case "$child_command" in
                *--capture-log*) active_logger_pid=$child_pid ;;
            esac
        done
    fi
    [ -n "$active_logger_pid" ] || sleep 0.01
done
if [ -z "$active_logger_pid" ]; then
    printf 'could not identify the bounded logger child for wrapper %s\n' \
        "$active_wrapper_pid" >&2
    exit 1
fi
active_unit=$(cat "$outer_state/unit")
kill -KILL "$active_wrapper_pid"
set +e
wait "$active_invocation_pid"
active_status=$?
set -e
tracked_runner_pid=
test "$active_status" -eq 137
active_drain=0
while process_is_live "$active_supervisor_pid" \
    || process_is_live "$active_payload_pid" \
    || process_is_live "$active_logger_pid" \
    || [ -e "$outer_state/active" ]; do
    active_drain=$((active_drain + 1))
    test "$active_drain" -le 500
    sleep 0.01
done
grep -Fqx "$active_unit" "$outer_state/stops"
test ! -e "$evidence/fixtures-ci-proof.json"
rm -rf "$evidence"/.fixtures-ci-run.*
rm -f "$evidence"/.fixtures-ci.log.tmp "$evidence/fixtures-ci.log"

# Kill the parent only after it consumed the status record but before release.
# The supervisor is then blocked on the release FIFO and must exit on real EOF.
rm -f "$work/parent-release-gate.ready" "$work/parent-release-gate.continue"
current_case=parent-sigkill-release
set +e
CAST_FIXTURE_TEST_LATCHED_RELEASE_GATE="$work/parent-release-gate" \
    run_wrapper --exec parent-sigkill 10 >"$work/parent-sigkill.out" 2>&1 &
parent_invocation_pid=$!
tracked_runner_pid=$parent_invocation_pid
tracked_gate_fifo="$work/parent-release-gate.continue"
tracked_gate_token=continue
set -e
parent_probe=0
while { [ ! -f "$outer_state/wrapper-pid" ] \
    || [ ! -f "$outer_state/supervisor-pid" ] \
    || [ ! -f "$work/parent-release-gate.ready" ]; }
do
    parent_probe=$((parent_probe + 1))
    test "$parent_probe" -le 200
    sleep 0.01
done
killed_wrapper_pid=$(cat "$outer_state/wrapper-pid")
test "$killed_wrapper_pid" -eq "$parent_invocation_pid"
killed_supervisor_pid=$(cat "$outer_state/supervisor-pid")
kill -KILL "$killed_wrapper_pid"
set +e
wait "$parent_invocation_pid"
parent_status=$?
set -e
tracked_runner_pid=
tracked_gate_fifo=
tracked_gate_token=
test "$parent_status" -eq 137
supervisor_probe=0
while process_is_live "$killed_supervisor_pid"; do
    supervisor_probe=$((supervisor_probe + 1))
    test "$supervisor_probe" -le 300
    sleep 0.01
done
test ! -e "$evidence/fixtures-ci-proof.json"
rm -rf "$evidence"/.fixtures-ci-run.*
rm -f "$evidence"/.fixtures-ci.log.tmp "$evidence/fixtures-ci.log"

current_case=setup-signal
setup_gate="$gates/setup-signal"
rm -rf "$setup_gate.ready" "$setup_gate.continue"
mkfifo -m 600 "$setup_gate.continue"
tracked_gate_fifo="$setup_gate.continue"
tracked_gate_token=continue
set +e
FAKE_LOAD_STATE_GATE="$setup_gate" run_wrapper --exec success 10 \
    >"$work/setup-signal.out" 2>&1 &
setup_wrapper_pid=$!
tracked_runner_pid=$setup_wrapper_pid
set -e
wait_for_receipt "$setup_gate.ready" "$setup_wrapper_pid" \
    'LoadState setup gate'
kill -TERM "$setup_wrapper_pid"
printf 'continue\n' >"$setup_gate.continue"
tracked_gate_fifo=
set +e
wait "$setup_wrapper_pid"
status=$?
set -e
tracked_runner_pid=
tracked_gate_token=
test "$status" -eq 143
test ! -e "$outer_state/unit"
test ! -e "$outer_state/command-pid"
test ! -e "$evidence/fixtures-ci-proof.json"
assert_bounded_inventory

public_late_gate="$gates/success-public-late-proof"
rm -rf "$public_late_gate.ready" "$public_late_gate.release" \
    "$public_late_gate.drain-started" "$public_late_gate.drained" \
    "$public_late_gate.natural-exit"
mkfifo -m 600 "$public_late_gate.release"
set +e
FAKE_DESCENDANT_GATE="$public_late_gate" \
    run_wrapper success-public-late-proof 10 \
    >"$work/success-public-late-proof.out" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
    cat "$work/success-public-late-proof.out" >&2
    exit 1
fi
public_late_pid=$(cat "$outer_state/descendant-pid")
require_receipt "$public_late_gate.ready" \
    'public late-proof writer readiness'
require_receipt "$public_late_gate.drain-started" \
    'public late-proof writer drain start'
require_receipt "$public_late_gate.drained" \
    'public late-proof writer drain completion'
test ! -e "$public_late_gate.natural-exit"
if process_is_live "$public_late_pid"; then
    printf 'successful service descendant escaped its control group: %s\n' \
    "$public_late_pid" >&2
    exit 1
fi
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
if grep -Fq 'forged-after-success' "$evidence/fixtures-ci-proof.json"; then
    printf '%s\n' 'successful fixture proof was replaced after promotion' >&2
    exit 1
fi
assert_bounded_inventory

for validation_call in 1 2; do
    set +e
    FAKE_JQ_SIGNAL_CALL="$validation_call" run_wrapper success 10 \
        >"$work/validation-signal-$validation_call.out" 2>&1
    status=$?
    set -e
    test "$status" -eq 143
    test ! -e "$evidence/fixtures-ci-proof.json"
    assert_bounded_inventory
done
