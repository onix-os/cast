#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
    printf 'usage: %s <absolute-entry-ledger-path>\n' "$0" >&2
    exit 2
fi

entries=$1
case "$entries" in
    /*) ;;
    *) printf 'fixture entry ledger path must be absolute: %s\n' "$entries" >&2; exit 2 ;;
esac
if [ -L "$entries" ] || [ ! -f "$entries" ]; then
    printf 'fixture entry ledger must be one regular non-symlink file: %s\n' \
        "$entries" >&2
    exit 1
fi
entry_ledger_size=$(stat -c '%s' "$entries") || exit 1
if [ "$entry_ledger_size" -le 0 ] || [ "$entry_ledger_size" -gt 65536 ]; then
    printf 'fixture entry ledger must contain between 1 and 65536 bytes: %s\n' \
        "$entries" >&2
    exit 1
fi
command -v sha256sum >/dev/null 2>&1 || {
    printf 'sha256sum is required to calculate a fixture bundle ledger\n' >&2
    exit 1
}
command -v awk >/dev/null 2>&1 || {
    printf 'awk is required to frame a fixture bundle ledger\n' >&2
    exit 1
}

fail_entry() {
    printf 'fixture entry ledger contains a noncanonical record\n' >&2
    exit 1
}

parse_entry() {
    entry_line=$1
    entry_remainder=${entry_line#*|}
    [ "$entry_remainder" != "$entry_line" ] || fail_entry
    entry_digest=${entry_remainder#*|}
    [ "$entry_digest" != "$entry_remainder" ] || fail_entry
    case "$entry_digest" in
        *'|'*) fail_entry ;;
    esac
    entry_name=${entry_line%%|*}
    entry_byte_count=${entry_remainder%%|*}
    case "$entry_name" in
        ''|*[!A-Za-z0-9._+-]*) fail_entry ;;
    esac
    case "${entry_name%"${entry_name#?}"}" in
        [A-Za-z0-9]) ;;
        *) fail_entry ;;
    esac
    [ "${#entry_name}" -le 255 ] || fail_entry
    case "$entry_byte_count" in
        [1-9]|[1-9][0-9]|[1-9][0-9][0-9]|[1-9][0-9][0-9][0-9]|\
        [1-9][0-9][0-9][0-9][0-9]|[1-9][0-9][0-9][0-9][0-9][0-9]|\
        [1-9][0-9][0-9][0-9][0-9][0-9][0-9]|\
        [1-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]|\
        [1-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]) ;;
        *) fail_entry ;;
    esac
    [ "$entry_byte_count" -le 134217728 ] || fail_entry
    case "$entry_digest" in
        *[!0-9a-f]*) fail_entry ;;
    esac
    [ "${#entry_digest}" -eq 64 ] || fail_entry
}

entry_count=0
binary_size=35
while IFS= read -r entry_line || [ -n "$entry_line" ]; do
    parse_entry "$entry_line"
    entry_count=$((entry_count + 1))
    binary_size=$((binary_size + 8 + ${#entry_name} + 8 + 32))
done <"$entries"
[ "$entry_count" -gt 0 ] || fail_entry

umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-fixture-ledger.XXXXXXXXXXXX")
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

binary_ledger="$work/ledger.bin"
LC_ALL=C awk -F '|' -v entry_count="$entry_count" '
    function emit_octet(value) {
        printf "%c", value
    }
    function emit_u64_le(value, position) {
        for (position = 0; position < 8; position += 1) {
            emit_octet(value % 256)
            value = int(value / 256)
        }
    }
    function hex_value(character) {
        return index("0123456789abcdef", character) - 1
    }
    function emit_digest(digest, position, high, low) {
        for (position = 1; position <= 64; position += 2) {
            high = hex_value(substr(digest, position, 1))
            low = hex_value(substr(digest, position + 1, 1))
            emit_octet(high * 16 + low)
        }
    }
    BEGIN {
        printf "%s", "cast.fixtures-ci.bundle.v1"
        emit_octet(0)
        emit_u64_le(entry_count)
    }
    {
        emit_u64_le(length($1))
        printf "%s", $1
        emit_u64_le($2 + 0)
        emit_digest($3)
    }
' "$entries" >"$binary_ledger"
actual_binary_size=$(stat -c '%s' "$binary_ledger") || exit 1
if [ "$actual_binary_size" -ne "$binary_size" ]; then
    printf 'awk could not emit the canonical binary fixture ledger\n' >&2
    exit 1
fi

set -- $(sha256sum "$binary_ledger")
case "${1-}" in
    *[!0-9a-f]*|'')
        printf 'sha256sum returned a noncanonical fixture ledger digest\n' >&2
        exit 1
        ;;
esac
[ "${#1}" -eq 64 ] || {
    printf 'sha256sum returned a noncanonical fixture ledger digest\n' >&2
    exit 1
}
printf '%s\n' "$1"
