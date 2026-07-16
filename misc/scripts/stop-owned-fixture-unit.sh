#!/bin/sh

set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s <unit> <ownership-marker> <stop-bound-seconds>\n' "$0" >&2
    exit 2
fi

unit=$1
marker=$2
kill_after_seconds=$3
caller_uid=$(id -u)

case "$unit" in
    cast-fixtures-ci-*)
        identity=${unit#cast-fixtures-ci-}
        marker_name=CAST_FIXTURE_CI_UNIT_TOKEN
        ;;
    cast-delegated-fixture-*)
        identity=${unit#cast-delegated-fixture-}
        marker_name=CAST_DELEGATED_FIXTURE_TOKEN
        ;;
    *) printf 'refusing non-fixture unit: %s\n' "$unit" >&2; exit 1 ;;
esac
case "$identity" in
    "$caller_uid"-*) identity=${identity#"$caller_uid"-} ;;
    *) printf 'owned fixture unit UID does not match the caller: %s\n' "$unit" >&2; exit 1 ;;
esac
owner_pid=${identity%%-*}
token_and_suffix=${identity#*-}
case "$token_and_suffix" in
    *.service) token=${token_and_suffix%.service} ;;
    *) printf 'owned fixture unit lacks the exact service suffix: %s\n' "$unit" >&2; exit 1 ;;
esac
case "$owner_pid" in
    ''|*[!0-9]*) printf 'owned fixture unit PID is invalid: %s\n' "$unit" >&2; exit 1 ;;
esac
case "$token" in
    ''|*[!A-Za-z0-9]*) printf 'owned fixture unit token is invalid: %s\n' "$unit" >&2; exit 1 ;;
esac
expected_marker="$marker_name=$token"
if [ "$marker" != "$expected_marker" ]; then
    printf 'owned fixture unit marker does not match its family and token: %s\n' \
        "$unit" >&2
    exit 1
fi
case "$kill_after_seconds" in
    ''|*[!0-9]*) printf 'owned fixture unit stop bound must be decimal\n' >&2; exit 2 ;;
esac
case "$kill_after_seconds" in
    0|0*|????*)
        printf 'owned fixture unit stop bound must be between 1 and 300 seconds\n' >&2
        exit 2
        ;;
esac
if [ "$kill_after_seconds" -lt 1 ] || [ "$kill_after_seconds" -gt 300 ]; then
    printf 'owned fixture unit stop bound must be between 1 and 300 seconds\n' >&2
    exit 2
fi

command -v timeout >/dev/null 2>&1 || {
    printf 'timeout is required to stop an owned fixture unit\n' >&2
    exit 1
}
command -v systemctl >/dev/null 2>&1 || {
    printf 'systemctl is required to stop an owned fixture unit\n' >&2
    exit 1
}

load_state=$(timeout --kill-after=1s 10s systemctl --user show "$unit" \
    --property=LoadState --value 2>/dev/null) || {
    printf 'could not verify owned fixture unit load state: %s\n' "$unit" >&2
    exit 1
}
[ "$load_state" != not-found ] || exit 0
environment=$(timeout --kill-after=1s 10s systemctl --user show "$unit" \
    --property=Environment --value 2>/dev/null) || {
    printf 'could not verify owned fixture unit environment: %s\n' "$unit" >&2
    exit 1
}
case " $environment " in
    *" $marker "*) ;;
    *)
        printf 'refusing to stop fixture unit without this invocation marker: %s\n' \
            "$unit" >&2
        exit 1
        ;;
esac

if ! timeout --kill-after=1s "${kill_after_seconds}s" \
    systemctl --user stop "$unit" >/dev/null 2>&1; then
    printf 'normal stop failed for owned fixture unit %s; forcing its control group down\n' \
        "$unit" >&2
    timeout --kill-after=1s 10s systemctl --user kill \
        --kill-whom=all --signal=SIGKILL "$unit" >/dev/null 2>&1 || {
        printf 'forced kill failed for owned fixture unit: %s\n' "$unit" >&2
    }
    timeout --kill-after=1s 10s systemctl --user stop "$unit" \
        >/dev/null 2>&1 || {
        printf 'post-kill stop failed for owned fixture unit: %s\n' "$unit" >&2
    }
fi

final_load_state=$(timeout --kill-after=1s 10s systemctl --user show "$unit" \
    --property=LoadState --value 2>/dev/null) || {
    printf 'could not verify final owned fixture unit load state: %s\n' "$unit" >&2
    exit 1
}
[ "$final_load_state" != not-found ] || exit 0
active_state=$(timeout --kill-after=1s 10s systemctl --user show "$unit" \
    --property=ActiveState --value 2>/dev/null) || {
    printf 'could not verify final owned fixture unit active state: %s\n' "$unit" >&2
    exit 1
}
case "$active_state" in
    inactive|failed) exit 0 ;;
    *)
        printf 'owned fixture unit remained active after bounded cleanup: %s (%s)\n' \
            "$unit" "$active_state" >&2
        exit 1
        ;;
esac
