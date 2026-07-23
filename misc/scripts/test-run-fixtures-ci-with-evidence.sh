#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
runner="$root/misc/scripts/run-fixtures-ci-with-evidence.sh"
proof_generator="$root/misc/scripts/test-support/write-fixtures-ci-proof-v2.sh"
proof_validator="$root/misc/scripts/validate-fixtures-ci-proof.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-fixtures-ci-evidence-test.XXXXXXXXXXXX")
canonical_proof="$work/canonical-fixtures-ci-proof.json"
current_case=initialization
tracked_runner_pid=
tracked_gate_fifo=
tracked_gate_token=

pid_is_live() {
    live_pid=$1
    IFS= read -r live_stat 2>/dev/null <"/proc/$live_pid/stat" || return 1
    live_tail=${live_stat##*) }
    live_state=${live_tail%% *}
    case "$live_state" in
        Z|X|'') return 1 ;;
        *) return 0 ;;
    esac
}

cleanup() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    set +e
    cleanup_gate_writer_pid=
    if [ -n "$tracked_gate_fifo" ]; then
        if [ -p "$tracked_gate_fifo" ]; then
            (
                printf '%s\n' "${tracked_gate_token:-release}" \
                    >"$tracked_gate_fifo"
            ) &
            cleanup_gate_writer_pid=$!
        else
            printf '%s\n' "${tracked_gate_token:-continue}" \
                >"$tracked_gate_fifo" 2>/dev/null || :
        fi
    fi
    if [ -n "$tracked_runner_pid" ]; then
        kill -TERM "$tracked_runner_pid" 2>/dev/null || :
        cleanup_runner_attempt=0
        while pid_is_live "$tracked_runner_pid" \
            && [ "$cleanup_runner_attempt" -lt 200 ]; do
            cleanup_runner_attempt=$((cleanup_runner_attempt + 1))
            sleep 0.01
        done
        if pid_is_live "$tracked_runner_pid"; then
            kill -KILL "$tracked_runner_pid" 2>/dev/null || :
        fi
        wait "$tracked_runner_pid" 2>/dev/null || :
    fi
    if [ -n "$cleanup_gate_writer_pid" ]; then
        kill -TERM "$cleanup_gate_writer_pid" 2>/dev/null || :
        cleanup_writer_attempt=0
        while pid_is_live "$cleanup_gate_writer_pid" \
            && [ "$cleanup_writer_attempt" -lt 100 ]; do
            cleanup_writer_attempt=$((cleanup_writer_attempt + 1))
            sleep 0.01
        done
        if pid_is_live "$cleanup_gate_writer_pid"; then
            kill -KILL "$cleanup_gate_writer_pid" 2>/dev/null || :
        fi
        wait "$cleanup_gate_writer_pid" 2>/dev/null || :
    fi
    if [ "$cleanup_status" -ne 0 ]; then
        printf 'bounded fixture CI evidence test failed during case: %s\n' \
            "$current_case" >&2
    fi
    rm -rf "$work"
    exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fakebin="$work/bin"
hostile_bash_bin="$work/hostile-bash-bin"
evidence="$work/evidence"
outer_state="$work/outer-state"
gates="$work/gates"
bash_env_hook="$work/bash-env-hook"
bash_env_start_marker="$work/bash-env-started"
bash_env_fd9_marker="$work/bash-env-saw-fd9"
real_tee=$(command -v tee)
real_jq=$(command -v jq)
real_chmod=$(command -v chmod)
real_bash=$(command -v bash)
case "$real_bash" in
    /*) ;;
    *) printf 'test Bash must resolve absolutely: %s\n' "$real_bash" >&2; exit 1 ;;
esac
test -f "$real_bash"
test -x "$real_bash"
fake_commit=$(timeout 10s git -C "$root" rev-parse --verify HEAD)
mkdir -p "$fakebin" "$hostile_bash_bin" "$evidence" "$outer_state" "$gates"
chmod 700 "$evidence"
cat >"$bash_env_hook" <<'EOF'
if [ -n "${BASH_ENV_TEST_START_MARKER-}" ]; then
    : >"$BASH_ENV_TEST_START_MARKER"
fi
if [ -e "/proc/$$/fd/9" ]; then
    : >"${BASH_ENV_FD9_MARKER:?}"
    flock --unlock 9 >/dev/null 2>&1 || :
fi
EOF
chmod 600 "$bash_env_hook"
if [ -L "$proof_generator" ] || [ ! -f "$proof_generator" ] \
    || [ ! -x "$proof_generator" ]; then
    printf 'fixture proof test generator is unavailable or unsafe: %s\n' \
        "$proof_generator" >&2
    exit 1
fi
if [ -L "$proof_validator" ] || [ ! -f "$proof_validator" ] \
    || [ ! -x "$proof_validator" ]; then
    printf 'fixture proof validator is unavailable or unsafe: %s\n' \
        "$proof_validator" >&2
    exit 1
fi
timeout 120s "$proof_generator" "$canonical_proof" "$fake_commit"
timeout 120s "$proof_validator" "$canonical_proof" "$fake_commit"
test -f "$canonical_proof"
test ! -L "$canonical_proof"
test "$(timeout 10s stat -c '%a' "$canonical_proof")" = 644

unavailable_shebang_root="$work/unavailable-shebang-root"
unavailable_shebang_scripts="$unavailable_shebang_root/misc/scripts"
unavailable_shebang_runner="$unavailable_shebang_scripts/run-fixtures-ci-with-evidence.sh"
missing_bash="$work/unavailable-bin/bash"
mkdir -p "$unavailable_shebang_scripts"
for helper in \
    run-latched-command.sh \
    stop-owned-fixture-unit.sh \
    validate-fixtures-ci-proof.sh \
    calculate-fixtures-ci-ledger.sh
do
    install -m 0755 "$root/misc/scripts/$helper" \
        "$unavailable_shebang_scripts/$helper"
done
install -m 0644 "$root/misc/scripts/validate-fixtures-ci-proof.jq" \
    "$unavailable_shebang_scripts/validate-fixtures-ci-proof.jq"
install -m 0644 "$root/misc/scripts/fixture-runtime-budgets.sh" \
    "$unavailable_shebang_scripts/fixture-runtime-budgets.sh"
{
    printf '#!%s\n' "$missing_bash"
    tail -n +2 "$runner"
} >"$unavailable_shebang_runner"
chmod 755 "$unavailable_shebang_runner"

cat >"$hostile_bash_bin/bash" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"
: >"$FAKE_OUTER_STATE/hostile-bash-used"
exit 97
EOF
chmod 755 "$hostile_bash_bin/bash"

grep -Fq 'CAST_FIXTURE_EVIDENCE_DIR="$${CAST_FIXTURE_EVIDENCE_DIR:-$(TOP_DIR)/target/fixture-evidence}"' \
    "$root/misc/make/execution-fixtures.mk"
if grep -Fq 'FIXTURE_EVIDENCE_DIR ?=' "$root/Makefile" \
    || grep -R -Fq --include='*.mk' 'FIXTURE_EVIDENCE_DIR ?=' "$root/misc/make"; then
    printf '%s\n' 'fixture evidence must not cross a Make-expanded path variable' >&2
    exit 1
fi
grep -Fq 'IFS= read -r process_stat 2>/dev/null <"$stat_file" || continue' \
    "$runner"
if grep -Fq 'IFS= read -r process_stat <"$stat_file" 2>/dev/null' "$runner"; then
    printf '%s\n' 'volatile /proc reads must silence open failures before redirection' >&2
    exit 1
fi

cat >"$fakebin/make" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_MAKE_MODE:?}"
: "${CAST_FIXTURE_EVIDENCE_DIR:?}"
: "${FAKE_EXPECTED_DELEGATED_RUNTIME_MAX_SECONDS:?}"
test "${CAST_DELEGATED_RUNTIME_MAX_SECONDS-}" \
    = "$FAKE_EXPECTED_DELEGATED_RUNTIME_MAX_SECONDS"
test "${1-}" = --no-print-directory
test "${2-}" = -C
case "${4-}" in
    SHELL=/*) fixture_shell=${4#SHELL=} ;;
    *) exit 2 ;;
esac
test -f "$fixture_shell"
test -x "$fixture_shell"
test "${5-}" = fixtures-ci
test "$#" -eq 5
repository=$3
if [ "${FAKE_REQUIRE_LOCALE_ARCHIVE-}" = 1 ]; then
    : "${FAKE_EXPECTED_LOCALE_ARCHIVE:?}"
    test "${LOCALE_ARCHIVE-}" = "$FAKE_EXPECTED_LOCALE_ARCHIVE"
fi
if [ "${FAKE_REQUIRE_LOCALE_ARCHIVE_UNSET-}" = 1 ]; then
    test "${LOCALE_ARCHIVE+x}" != x
fi
test "${LOCPATH+x}" != x
test "${LOCALE_ARCHIVE_2_27+x}" != x

emit_proof() {
    commit=$(git -C "$repository" rev-parse --verify HEAD)
    : "${FAKE_PROOF_GENERATOR:?}"
    "$FAKE_PROOF_GENERATOR" \
        "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json" "$commit"
    printf 'temporary-proof-from-%s\n' "$FAKE_MAKE_MODE" \
        >"$CAST_FIXTURE_EVIDENCE_DIR/.fixtures-ci-proof.json.tmp"
}

install_canonical_proof() {
    : "${FAKE_CANONICAL_PROOF:?}"
    if [ -L "$FAKE_CANONICAL_PROOF" ] || [ ! -f "$FAKE_CANONICAL_PROOF" ]; then
        printf 'canonical fixture proof is unavailable or unsafe: %s\n' \
            "$FAKE_CANONICAL_PROOF" >&2
        return 1
    fi
    timeout 10s install -m 0644 "$FAKE_CANONICAL_PROOF" \
        "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
    printf 'temporary-proof-from-%s\n' "$FAKE_MAKE_MODE" \
        >"$CAST_FIXTURE_EVIDENCE_DIR/.fixtures-ci-proof.json.tmp"
}

wait_for_child_receipt() {
    receipt_path=$1
    child_pid=$2
    receipt_label=$3
    receipt_attempt=0
    while [ ! -f "$receipt_path" ]; do
        if ! kill -0 "$child_pid" 2>/dev/null; then
            printf '%s child %s exited before publishing %s: %s\n' \
                "$receipt_label" "$child_pid" "$receipt_label" "$receipt_path" >&2
            return 1
        fi
        receipt_attempt=$((receipt_attempt + 1))
        if [ "$receipt_attempt" -gt 1000 ]; then
            printf 'timed out waiting for %s from child %s: %s\n' \
                "$receipt_label" "$child_pid" "$receipt_path" >&2
            return 1
        fi
        sleep 0.01
    done
    if [ -L "$receipt_path" ]; then
        printf '%s receipt must not be a symlink: %s\n' \
            "$receipt_label" "$receipt_path" >&2
        return 1
    fi
}

case "$FAKE_MAKE_MODE" in
    success)
        printf 'BEGIN-SUCCESS\n'
        index=0
        while [ "$index" -lt 80 ]; do
            printf 'bounded-success-line-%03d-abcdefghijklmnopqrstuvwxyz\n' "$index"
            index=$((index + 1))
        done
        printf 'END-SUCCESS\n'
        emit_proof
        ;;
    success-public-late-proof)
        : "${FAKE_PUBLIC_EVIDENCE_DIR:?}"
        : "${FAKE_OUTER_STATE:?}"
        : "${FAKE_DESCENDANT_GATE:?}"
        setsid sh -c '
            set -eu
            trap "" HUP INT TERM
            proof_path=$1
            ready_path=$2
            release_path=$3
            natural_exit_path=$4
            : >"$ready_path"
            IFS= read -r release_token <"$release_path"
            test "$release_token" = release
            printf "%s\n" forged-after-success >"$proof_path"
            : >"$natural_exit_path"
        ' public-proof-writer \
            "$FAKE_PUBLIC_EVIDENCE_DIR/fixtures-ci-proof.json" \
            "$FAKE_DESCENDANT_GATE.ready" \
            "$FAKE_DESCENDANT_GATE.release" \
            "$FAKE_DESCENDANT_GATE.natural-exit" \
            </dev/null >/dev/null 2>&1 &
        descendant_pid=$!
        printf '%s\n' "$descendant_pid" >"$FAKE_OUTER_STATE/descendant-pid"
        wait_for_child_receipt "$FAKE_DESCENDANT_GATE.ready" \
            "$descendant_pid" 'public late-proof writer readiness'
        emit_proof
        printf 'SUCCESS-WITH-CGROUP-DESCENDANT\n'
        ;;
    success-no-proof)
        printf 'SUCCESS-WITHOUT-PROOF\n'
        ;;
    malformed-proof)
        printf '%s\n' '{"result":"passed"}' >"$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
        chmod 644 "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
        printf 'MALFORMED-PROOF\n'
        ;;
    failure)
        printf 'BEGIN-FAILURE\n'
        index=0
        while [ "$index" -lt 80 ]; do
            printf 'bounded-failure-line-%03d-abcdefghijklmnopqrstuvwxyz\n' "$index"
            index=$((index + 1))
        done
        printf 'END-FAILURE\n'
        exit 37
        ;;
    failure-fifo-descendant)
        : "${FAKE_OUTER_STATE:?}"
        : "${FAKE_DESCENDANT_GATE:?}"
        setsid sh -c '
            set -eu
            trap "" HUP INT TERM
            ready_path=$1
            release_path=$2
            natural_exit_path=$3
            : >"$ready_path"
            IFS= read -r release_token <"$release_path"
            test "$release_token" = release
            : >"$natural_exit_path"
        ' fifo-holder "$FAKE_DESCENDANT_GATE.ready" \
            "$FAKE_DESCENDANT_GATE.release" \
            "$FAKE_DESCENDANT_GATE.natural-exit" &
        descendant_pid=$!
        printf '%s\n' "$descendant_pid" >"$FAKE_OUTER_STATE/descendant-pid"
        wait_for_child_receipt "$FAKE_DESCENDANT_GATE.ready" \
            "$descendant_pid" 'FIFO descendant readiness'
        printf 'FAILURE-WITH-FIFO-DESCENDANT\n'
        exit 37
        ;;
    emit-then-fail)
        emit_proof
        printf 'EMIT-THEN-FAIL\n'
        exit 38
        ;;
    timeout)
        : "${FAKE_TIMEOUT_GATE:?}"
        printf 'BEGIN-TIMEOUT\n'
        # Install the precomputed proof before entering an explicit blocker.
        # The receipt proves the one-second runtime reached the intended
        # long-lived phase rather than expiring during matrix-sized setup.
        install_canonical_proof
        : >"$FAKE_TIMEOUT_GATE.ready"
        IFS= read -r timeout_token <"$FAKE_TIMEOUT_GATE.hold"
        test "$timeout_token" = unreachable
        ;;
    ignore-term)
        trap '' TERM
        printf 'BEGIN-IGNORE-TERM\n'
        # Install the hostile signal policy before matrix-sized setup so this
        # case deterministically exercises the SIGKILL escalation path.
        emit_proof
        while :; do
            sleep 1
        done
        ;;
    signal)
        emit_proof
        printf 'BEGIN-SIGNAL\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        kill -TERM "$wrapper_pid"
        exit 143
        ;;
    signal-supervisor)
        emit_proof
        printf 'BEGIN-SUPERVISOR-SIGNAL\n'
        kill -TERM "${CAST_LATCHED_SUPERVISOR_PID:?}"
        sleep 5
        exit 143
        ;;
    parent-sigkill)
        : "${FAKE_OUTER_STATE:?}"
        printf '%s\n' "${CAST_FIXTURE_WRAPPER_PID:?}" \
            >"$FAKE_OUTER_STATE/wrapper-pid"
        printf '%s\n' "${CAST_LATCHED_SUPERVISOR_PID:?}" \
            >"$FAKE_OUTER_STATE/supervisor-pid"
        printf 'BEGIN-PARENT-SIGKILL\n'
        ;;
    parent-sigkill-active)
        : "${FAKE_OUTER_STATE:?}"
        emit_proof
        printf '%s\n' "${CAST_FIXTURE_WRAPPER_PID:?}" \
            >"$FAKE_OUTER_STATE/wrapper-pid"
        printf '%s\n' "${CAST_LATCHED_SUPERVISOR_PID:?}" \
            >"$FAKE_OUTER_STATE/supervisor-pid"
        printf '%s\n' "$$" >"$FAKE_OUTER_STATE/payload-pid"
        printf 'BEGIN-ACTIVE-PARENT-SIGKILL\n'
        : >"$FAKE_OUTER_STATE/active-parent-ready"
        while :; do
            sleep 1
        done
        ;;
    freeze-supervisor)
        : "${FAKE_OUTER_STATE:?}"
        printf '%s\n' "${CAST_FIXTURE_WRAPPER_PID:?}" \
            >"$FAKE_OUTER_STATE/wrapper-pid"
        printf '%s\n' "${CAST_LATCHED_SUPERVISOR_PID:?}" \
            >"$FAKE_OUTER_STATE/supervisor-pid"
        kill -STOP "${CAST_LATCHED_SUPERVISOR_PID:?}"
        : >"$FAKE_OUTER_STATE/frozen-supervisor-ready"
        # Freeze and identify the supervisor before matrix-sized setup so the
        # one-second status bound tests the frozen channel, not proof latency.
        emit_proof
        while :; do
            sleep 1
        done
        ;;
    double-signal)
        : "${FAKE_OUTER_STATE:?}"
        : "${FAKE_DOUBLE_SIGNAL_GATE:?}"
        emit_proof
        printf 'BEGIN-DOUBLE-SIGNAL\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        printf '%s\n' "$wrapper_pid" >"$FAKE_OUTER_STATE/wrapper-pid"
        : >"$FAKE_DOUBLE_SIGNAL_GATE.ready"
        IFS= read -r hold_token <"$FAKE_DOUBLE_SIGNAL_GATE.hold"
        test "$hold_token" = continue
        ;;
    signal-late-proof)
        trap '' HUP INT TERM
        : "${FAKE_LATE_PID_FILE:?}"
        : "${FAKE_DESCENDANT_GATE:?}"
        setsid sh -c '
            set -eu
            trap "" HUP INT TERM
            proof_path=$1
            ready_path=$2
            release_path=$3
            natural_exit_path=$4
            : >"$ready_path"
            IFS= read -r release_token <"$release_path"
            test "$release_token" = release
            printf "%s\n" late-proof >"$proof_path" 2>/dev/null || :
            : >"$natural_exit_path"
        ' late-proof-writer \
            "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json" \
            "$FAKE_DESCENDANT_GATE.ready" \
            "$FAKE_DESCENDANT_GATE.release" \
            "$FAKE_DESCENDANT_GATE.natural-exit" \
            </dev/null >/dev/null 2>&1 &
        descendant_pid=$!
        printf '%s\n' "$descendant_pid" >"$FAKE_LATE_PID_FILE"
        printf '%s\n' "$descendant_pid" >"$FAKE_OUTER_STATE/descendant-pid"
        wait_for_child_receipt "$FAKE_DESCENDANT_GATE.ready" \
            "$descendant_pid" 'signal late-proof writer readiness'
        printf 'BEGIN-SIGNAL-LATE-PROOF\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        kill -TERM "$wrapper_pid"
        IFS= read -r hold_token <"$FAKE_DESCENDANT_GATE.payload-hold"
        test "$hold_token" = release
        ;;
    finalize-failure)
        emit_proof
        evidence_parent=$(dirname "$CAST_FIXTURE_EVIDENCE_DIR")
        rm -f "$evidence_parent/.fixtures-ci.log.tmp"
        mkdir "$evidence_parent/.fixtures-ci.log.tmp"
        printf 'FINALIZE-FAILURE\n'
        ;;
    current-run-nested)
        emit_proof
        mkdir "$CAST_FIXTURE_EVIDENCE_DIR/injected-directory"
        printf 'CURRENT-RUN-NESTED\n'
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/make"

cat >"$fakebin/git" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_GIT_COMMIT:?}"
: "${FAKE_OUTER_STATE:?}"
test "${1-}" = -C
shift 2
case "${1-}" in
    rev-parse)
        test "${2-}" = --verify
        test "${3-}" = HEAD
        printf '%s\n' "$FAKE_GIT_COMMIT"
        ;;
    status)
        test "${2-}" = --porcelain
        test "${3-}" = --untracked-files=normal
        case "${FAKE_GIT_STATUS_MODE-clean}" in
            clean) ;;
            dirty) printf '%s\n' ' M fixture-input' ;;
            fail) exit 71 ;;
            *) exit 2 ;;
        esac
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/git"

cat >"$fakebin/jq" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"
: "${REAL_JQ:?}"
calls=0
if [ -f "$FAKE_OUTER_STATE/jq-calls" ]; then
    calls=$(cat "$FAKE_OUTER_STATE/jq-calls")
fi
calls=$((calls + 1))
printf '%s\n' "$calls" >"$FAKE_OUTER_STATE/jq-calls"
if [ -n "${FAKE_JQ_SIGNAL_CALL-}" ] \
    && [ "$calls" -eq "$FAKE_JQ_SIGNAL_CALL" ]; then
    kill -TERM "$PPID"
fi
exec "$REAL_JQ" "$@"
EOF
chmod 755 "$fakebin/jq"

cat >"$fakebin/systemd-run" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"
if [ -e "/proc/$$/fd/9" ]; then
    printf '%s\n' 'evidence lock crossed the delegated payload boundary' >&2
    exit 73
fi
# Start from a poisoned user-manager value so only an explicit set-or-unset
# boundary can give the service the caller's intended locale state.
LOCALE_ARCHIVE=/poisoned-manager-locale-archive
export LOCALE_ARCHIVE
LOCPATH=/poisoned-manager-locpath
export LOCPATH
LOCALE_ARCHIVE_2_27=/poisoned-manager-legacy-locale-archive
export LOCALE_ARCHIVE_2_27

process_is_live() {
    process_pid=$1
    IFS= read -r process_stat 2>/dev/null <"/proc/$process_pid/stat" || return 1
    process_tail=${process_stat##*) }
    process_state=${process_tail%% *}
    case "$process_state" in
        Z|X|'') return 1 ;;
        *) return 0 ;;
    esac
}

unit=
working_directory=
exit_type=
kill_mode=
runtime_max=
stop_timeout=
: >"$FAKE_OUTER_STATE/environment"
: >"$FAKE_OUTER_STATE/systemd-run-args"
while [ "$#" -gt 0 ]; do
    printf '%s\n' "$1" >>"$FAKE_OUTER_STATE/systemd-run-args"
    case "$1" in
        --user|--wait|--pipe|--collect|--no-ask-password|--expand-environment=no|--service-type=exec)
            shift
            ;;
        --unit=*) unit=${1#--unit=}; shift ;;
        --working-directory=*) working_directory=${1#--working-directory=}; shift ;;
        --setenv=*)
            assignment=${1#--setenv=}
            export "$assignment"
            printf '%s\n' "$assignment" >>"$FAKE_OUTER_STATE/environment"
            shift
            ;;
        --property=ExitType=*) exit_type=${1#--property=ExitType=}; shift ;;
        --property=KillMode=*) kill_mode=${1#--property=KillMode=}; shift ;;
        --property=RuntimeMaxSec=*) runtime_max=${1#--property=RuntimeMaxSec=}; shift ;;
        --property=TimeoutStopSec=*) stop_timeout=${1#--property=TimeoutStopSec=}; shift ;;
        --property=SendSIGKILL=yes) shift ;;
        --property=UnsetEnvironment=LOCALE_ARCHIVE) unset LOCALE_ARCHIVE; shift ;;
        --property=UnsetEnvironment=LOCPATH) unset LOCPATH; shift ;;
        --property=UnsetEnvironment=LOCALE_ARCHIVE_2_27) unset LOCALE_ARCHIVE_2_27; shift ;;
        --) shift; break ;;
        *) printf 'unexpected fake systemd-run argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
test -n "$unit"
test -n "$working_directory"
test "$exit_type" = main
test "$kill_mode" = control-group
case "$runtime_max" in *s) runtime_seconds=${runtime_max%s} ;; *) exit 2 ;; esac
case "$runtime_seconds" in ''|0|*[!0-9]*) exit 2 ;; esac
case "$stop_timeout" in *s) stop_seconds=${stop_timeout%s} ;; *) exit 2 ;; esac
case "$stop_seconds" in ''|0|*[!0-9]*) exit 2 ;; esac
test "$#" -gt 0
if [ "${LOCALE_ARCHIVE+x}" = x ]; then
    printf '%s\n' "$LOCALE_ARCHIVE" >"$FAKE_OUTER_STATE/locale-archive-effective"
else
    printf '%s\n' '<unset>' >"$FAKE_OUTER_STATE/locale-archive-effective"
fi
printf '%s\n' "$unit" >"$FAKE_OUTER_STATE/unit"
: >"$FAKE_OUTER_STATE/active"
(
    cd "$working_directory"
    exec timeout --kill-after="$stop_timeout" "$runtime_max" "$@"
) &
command_pid=$!
printf '%s\n' "$command_pid" >"$FAKE_OUTER_STATE/command-pid"
set +e
wait "$command_pid"
status=$?
set -e

# Simulate systemd's main-process exit plus KillMode=control-group: a
# session-changing descendant remains in the service cgroup and must be gone
# before the client can report completion.
if [ -f "$FAKE_OUTER_STATE/descendant-pid" ]; then
    descendant=$(cat "$FAKE_OUTER_STATE/descendant-pid")
    descendant_gate=${FAKE_DESCENDANT_GATE-}
    if [ -n "$descendant_gate" ]; then
        if [ ! -f "$descendant_gate.ready" ] \
            || [ -L "$descendant_gate.ready" ]; then
            printf 'descendant readiness receipt is missing or unsafe: %s\n' \
                "$descendant_gate.ready" >&2
            exit 72
        fi
        : >"$descendant_gate.drain-started"
    fi
    kill -TERM "$descendant" 2>/dev/null || :
    attempts=0
    while process_is_live "$descendant"; do
        attempts=$((attempts + 1))
        if [ "$attempts" -gt 20 ]; then
            kill -KILL "$descendant" 2>/dev/null || :
        fi
        test "$attempts" -le 40 || exit 70
        sleep 0.05
    done
    if [ -n "$descendant_gate" ]; then
        : >"$descendant_gate.drained"
    fi
fi
rm -f "$FAKE_OUTER_STATE/active" "$FAKE_OUTER_STATE/command-pid"
exit "$status"
EOF
chmod 755 "$fakebin/systemd-run"

cat >"$fakebin/systemctl" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"

process_is_live() {
    process_pid=$1
    IFS= read -r process_stat 2>/dev/null <"/proc/$process_pid/stat" || return 1
    process_tail=${process_stat##*) }
    process_state=${process_tail%% *}
    case "$process_state" in
        Z|X|'') return 1 ;;
        *) return 0 ;;
    esac
}

test "${1-}" = --user
shift
case "${1-}" in
    show-environment)
        exit 0
        ;;
    show)
        unit=${2-}
        property=${3-}
        test "${4-}" = --value
        case "$property" in
            --property=LoadState)
                if [ -n "${FAKE_LOAD_STATE_GATE-}" ]; then
                    : >"$FAKE_LOAD_STATE_GATE.ready"
                    IFS= read -r gate_token <"$FAKE_LOAD_STATE_GATE.continue"
                    test "$gate_token" = continue
                fi
                if [ -f "$FAKE_OUTER_STATE/active" ] \
                    && [ "$(cat "$FAKE_OUTER_STATE/unit")" = "$unit" ]; then
                    printf '%s\n' loaded
                else
                    printf '%s\n' not-found
                fi
                ;;
            --property=Environment)
                test -f "$FAKE_OUTER_STATE/active" || exit 1
                tr '\n' ' ' <"$FAKE_OUTER_STATE/environment"
                printf '\n'
                ;;
            --property=ActiveState)
                if [ -f "$FAKE_OUTER_STATE/active" ]; then
                    printf '%s\n' active
                else
                    printf '%s\n' inactive
                fi
                ;;
            *) exit 2 ;;
        esac
        ;;
    stop|kill)
        command=${1}
        shift
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --kill-whom=*|--signal=*) shift ;;
                *) unit=$1; shift ;;
            esac
        done
        if [ -f "$FAKE_OUTER_STATE/unit" ]; then
            test "$(cat "$FAKE_OUTER_STATE/unit")" = "$unit"
        fi
        printf '%s\n' "$unit" >>"$FAKE_OUTER_STATE/stops"
        if [ "$command" = stop ] \
            && [ -n "${FAKE_FINALIZE_STOP_GATE-}" ] \
            && mkdir "$FAKE_FINALIZE_STOP_GATE.claim" 2>/dev/null; then
            : >"$FAKE_FINALIZE_STOP_GATE.ready"
            IFS= read -r gate_token <"$FAKE_FINALIZE_STOP_GATE.continue"
            test "$gate_token" = continue
        fi
        signal=TERM
        test "$command" = stop || signal=KILL
        if [ -f "$FAKE_OUTER_STATE/command-pid" ]; then
            kill -"$signal" "$(cat "$FAKE_OUTER_STATE/command-pid")" 2>/dev/null || :
        fi
        if [ -f "$FAKE_OUTER_STATE/descendant-pid" ]; then
            descendant=$(cat "$FAKE_OUTER_STATE/descendant-pid")
            descendant_gate=${FAKE_DESCENDANT_GATE-}
            if [ "$command" = stop ] && [ -n "$descendant_gate" ]; then
                if [ ! -f "$descendant_gate.ready" ] \
                    || [ -L "$descendant_gate.ready" ]; then
                    printf 'descendant readiness receipt is missing or unsafe: %s\n' \
                        "$descendant_gate.ready" >&2
                    exit 72
                fi
                : >"$descendant_gate.drain-started"
            fi
            kill -"$signal" "$descendant" 2>/dev/null || :
            if [ "$command" = stop ]; then
                attempts=0
                while process_is_live "$descendant"; do
                    attempts=$((attempts + 1))
                    if [ "$attempts" -gt 20 ]; then
                        kill -KILL "$descendant" 2>/dev/null || :
                    fi
                    test "$attempts" -le 40 || break
                    sleep 0.05
                done
                if process_is_live "$descendant"; then
                    printf 'fake systemctl could not drain live descendant %s\n' \
                        "$descendant" >&2
                    exit 70
                fi
                if [ -n "$descendant_gate" ]; then
                    : >"$descendant_gate.drained"
                fi
            fi
        fi
        if [ "$command" = stop ]; then
            # The user manager, not the systemd-run client, owns unit state.
            # A successful stop makes the modeled unit inactive even if the
            # client itself was concurrently terminated.
            rm -f "$FAKE_OUTER_STATE/active" "$FAKE_OUTER_STATE/environment"
        fi
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/systemctl"

cat >"$fakebin/tee" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_TEE_MODE:?}"
: "${REAL_TEE:?}"
case "$FAKE_TEE_MODE" in
    pass) exec "$REAL_TEE" "$@" ;;
    fail)
        cat >/dev/null
        exit 75
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/tee"

run_wrapper() {
    wrapper_launch=call
    if [ "${1-}" = --exec ]; then
        wrapper_launch=exec
        shift
    fi
    wrapper_case=$1
    wrapper_timeout=$2
    wrapper_program=${RUN_WRAPPER_TEST_PROGRAM-$runner}
    wrapper_interpreter=${RUN_WRAPPER_TEST_INTERPRETER-$real_bash}
    wrapper_outer_state=${RUN_WRAPPER_TEST_OUTER_STATE-$outer_state}
    wrapper_path="$fakebin:$PATH"
    if [ -n "${RUN_WRAPPER_TEST_PATH_PREFIX-}" ]; then
        wrapper_path="$RUN_WRAPPER_TEST_PATH_PREFIX:$wrapper_path"
    fi
    current_case=$wrapper_case
    mkdir -p "$wrapper_outer_state"
    rm -f "$wrapper_outer_state"/*
    set -- env \
        PATH="$wrapper_path" \
        BASH_ENV="$bash_env_hook" \
        BASH_ENV_TEST_START_MARKER="$bash_env_start_marker" \
        BASH_ENV_FD9_MARKER="$bash_env_fd9_marker" \
        MAKE="$fakebin/make" \
        FIXTURE_EVIDENCE_DIR="$evidence" \
        CAST_FIXTURE_LOG_MAX_BYTES=256 \
        CAST_DELEGATED_RUNTIME_MAX_SECONDS="${CAST_DELEGATED_RUNTIME_MAX_SECONDS-}" \
        CAST_FIXTURE_CI_KILL_AFTER_SECONDS="${CAST_FIXTURE_CI_KILL_AFTER_SECONDS-1}" \
        CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS="${CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS-}" \
        FAKE_LATE_PID_FILE="$work/late-child.pid" \
        FAKE_PROOF_GENERATOR="$proof_generator" \
        FAKE_CANONICAL_PROOF="$canonical_proof" \
        FAKE_GIT_COMMIT="$fake_commit" \
        FAKE_GIT_STATUS_MODE="${FAKE_GIT_STATUS_MODE-clean}" \
        FAKE_JQ_SIGNAL_CALL="${FAKE_JQ_SIGNAL_CALL-}" \
        FAKE_DESCENDANT_GATE="${FAKE_DESCENDANT_GATE-}" \
        FAKE_TIMEOUT_GATE="${FAKE_TIMEOUT_GATE-}" \
        FAKE_DOUBLE_SIGNAL_GATE="${FAKE_DOUBLE_SIGNAL_GATE-}" \
        FAKE_FINALIZE_STOP_GATE="${FAKE_FINALIZE_STOP_GATE-}" \
        FAKE_LOAD_STATE_GATE="${FAKE_LOAD_STATE_GATE-}" \
        FAKE_OUTER_STATE="$wrapper_outer_state" \
        FAKE_EXPECTED_DELEGATED_RUNTIME_MAX_SECONDS="${CAST_DELEGATED_RUNTIME_MAX_SECONDS:-14400}" \
        FAKE_PUBLIC_EVIDENCE_DIR="$evidence" \
        FAKE_TEE_MODE="${FAKE_TEE_MODE-pass}" \
        CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP="${CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP-}" \
        CAST_FIXTURE_TEST_LATCHED_RELEASE_GATE="${CAST_FIXTURE_TEST_LATCHED_RELEASE_GATE-}" \
        REAL_JQ="$real_jq" \
        REAL_CHMOD="$real_chmod" \
        REAL_TEE="$real_tee" \
        FAKE_MAKE_MODE="$wrapper_case"
    if [ "$wrapper_timeout" != default ]; then
        set -- "$@" "CAST_FIXTURE_CI_TIMEOUT_SECONDS=$wrapper_timeout"
    fi
    if [ -n "$wrapper_interpreter" ]; then
        set -- "$@" "$wrapper_interpreter"
    fi
    set -- "$@" "$wrapper_program"
    if [ "$wrapper_launch" = exec ]; then
        exec "$@"
    fi
    "$@"
}

assert_bounded_inventory() {
    log="$evidence/fixtures-ci.log"
    test -f "$log"
    test ! -L "$log"
    test "$(stat -c '%a' "$log")" = 600
    test "$(stat -c '%s' "$log")" -le 256
    test -z "$(find "$evidence" -maxdepth 1 -name '.fixtures-ci.full.*' -print -quit)"
    test ! -e "$evidence/.fixtures-ci.log.tmp"
    test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
    test ! -e "$evidence/.fixtures-ci.output.fifo"
    test -z "$(find "$evidence" -maxdepth 1 -name '.fixtures-ci-run.*' -print -quit)"
    flock --exclusive --nonblock "$evidence" true
    test -f "$bash_env_start_marker"
    test ! -e "$bash_env_fd9_marker"
}

process_is_live() {
    process_pid=$1
    IFS= read -r process_stat 2>/dev/null <"/proc/$process_pid/stat" || return 1
    process_tail=${process_stat##*) }
    process_state=${process_tail%% *}
    case "$process_state" in
        Z|X|'') return 1 ;;
        *) return 0 ;;
    esac
}

wait_for_receipt() {
    receipt_path=$1
    receipt_owner_pid=$2
    receipt_label=$3
    receipt_attempt=0
    while [ ! -f "$receipt_path" ]; do
        if ! process_is_live "$receipt_owner_pid"; then
            printf 'case=%s label=%s receipt=%s owner=%s exited early\n' \
                "$current_case" "$receipt_label" "$receipt_path" \
                "$receipt_owner_pid" >&2
            return 1
        fi
        receipt_attempt=$((receipt_attempt + 1))
        if [ "$receipt_attempt" -gt 1000 ]; then
            printf 'case=%s label=%s receipt=%s owner=%s timed out\n' \
                "$current_case" "$receipt_label" "$receipt_path" \
                "$receipt_owner_pid" >&2
            return 1
        fi
        sleep 0.01
    done
    if [ -L "$receipt_path" ]; then
        printf 'case=%s label=%s receipt=%s owner=%s is a symlink\n' \
            "$current_case" "$receipt_label" "$receipt_path" \
            "$receipt_owner_pid" >&2
        return 1
    fi
}

require_receipt() {
    receipt_path=$1
    receipt_label=$2
    if [ ! -f "$receipt_path" ] || [ -L "$receipt_path" ]; then
        printf 'case=%s label=%s receipt=%s missing or unsafe\n' \
            "$current_case" "$receipt_label" "$receipt_path" >&2
        return 1
    fi
}

current_case=path-bash-bypasses-unavailable-shebang
set +e
"$unavailable_shebang_runner" --capture-log /dev/null 1 \
    "$work/direct-capture.log" >/dev/null 2>&1
direct_capture_status=$?
set -e
test "$direct_capture_status" -ne 0
test ! -e "$work/direct-capture.log"
RUN_WRAPPER_TEST_PROGRAM="$unavailable_shebang_runner" \
RUN_WRAPPER_TEST_INTERPRETER="$real_bash" \
RUN_WRAPPER_TEST_PATH_PREFIX="$hostile_bash_bin" \
    run_wrapper success default >"$work/path-bash-capture.out" 2>&1
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
grep -Fqx -- '--property=RuntimeMaxSec=21600s' \
    "$outer_state/systemd-run-args"
grep -Fqx -- '--setenv=CAST_DELEGATED_RUNTIME_MAX_SECONDS=14400' \
    "$outer_state/systemd-run-args"
test ! -e "$outer_state/hostile-bash-used"
assert_bounded_inventory

. "$root/misc/scripts/test-run-fixtures-ci-lifecycle-cases.sh"
. "$root/misc/scripts/test-run-fixtures-ci-result-cases.sh"
. "$root/misc/scripts/test-run-fixtures-ci-signal-cases.sh"
. "$root/misc/scripts/test-run-fixtures-ci-boundary-cases.sh"

printf '%s\n' 'bounded fixture CI evidence tests passed'
