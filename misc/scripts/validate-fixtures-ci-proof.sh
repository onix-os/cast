#!/bin/sh

set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s <absolute-proof-path> <canonical-git-commit>\n' "$0" >&2
    exit 2
fi

proof=$1
expected_commit=$2
script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
filter="$script_directory/validate-fixtures-ci-proof.jq"
ledger_calculator="$script_directory/calculate-fixtures-ci-ledger.sh"

case "$proof" in
    /*) ;;
    *) printf 'fixture CI proof path must be absolute: %s\n' "$proof" >&2; exit 2 ;;
esac
case "$expected_commit" in
    ''|*[!0-9a-f]*)
        printf 'fixture CI proof commit must be canonical lowercase hexadecimal\n' >&2
        exit 2
        ;;
esac
if [ "${#expected_commit}" -ne 40 ] && [ "${#expected_commit}" -ne 64 ]; then
    printf 'fixture CI proof commit must contain exactly 40 or 64 hexadecimal bytes\n' >&2
    exit 2
fi

if [ -L "$filter" ] || [ ! -f "$filter" ]; then
    printf 'fixture CI proof jq validator is unavailable or unsafe: %s\n' "$filter" >&2
    exit 1
fi
if [ -L "$ledger_calculator" ] || [ ! -f "$ledger_calculator" ] \
    || [ ! -x "$ledger_calculator" ]; then
    printf 'fixture CI proof ledger calculator is unavailable or unsafe: %s\n' \
        "$ledger_calculator" >&2
    exit 1
fi
if [ -L "$proof" ] || [ ! -f "$proof" ]; then
    printf 'fixture CI proof must be one regular non-symlink file: %s\n' "$proof" >&2
    exit 1
fi

proof_owner=$(stat -c '%u' "$proof") || exit 1
proof_mode=$(stat -c '%a' "$proof") || exit 1
proof_links=$(stat -c '%h' "$proof") || exit 1
proof_size=$(stat -c '%s' "$proof") || exit 1
if [ "$proof_owner" -ne "$(id -u)" ] \
    || [ "$proof_mode" != 644 ] \
    || [ "$proof_links" -ne 1 ]; then
    printf 'fixture CI proof must be caller-owned, mode 644, and singly linked: %s\n' \
        "$proof" >&2
    exit 1
fi
if [ "$proof_size" -le 0 ] || [ "$proof_size" -gt 131072 ]; then
    printf 'fixture CI proof must contain between 1 and 131072 bytes: %s\n' \
        "$proof" >&2
    exit 1
fi

command -v jq >/dev/null 2>&1 || {
    printf 'jq is required to validate fixture CI proof content\n' >&2
    exit 1
}
command -v cmp >/dev/null 2>&1 || {
    printf 'cmp is required to validate fixture CI proof key uniqueness\n' >&2
    exit 1
}

umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-fixture-proof-validator.XXXXXXXXXXXX")
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

normalized="$work/normalized.json"
proof_snapshot="$work/proof.json"
original_stream="$work/original.stream"
normalized_stream="$work/normalized.stream"
if ! cp -- "$proof" "$proof_snapshot"; then
    printf 'fixture CI proof could not be snapshotted for validation\n' >&2
    exit 1
fi
snapshot_size=$(stat -c '%s' "$proof_snapshot") || exit 1
if [ "$snapshot_size" -le 0 ] || [ "$snapshot_size" -gt 131072 ]; then
    printf 'fixture CI proof changed while it was being snapshotted\n' >&2
    exit 1
fi
if ! jq -c . "$proof_snapshot" >"$normalized" \
    || ! jq --stream -c . "$proof_snapshot" >"$original_stream" \
    || ! jq --stream -c . "$normalized" >"$normalized_stream"; then
    printf 'fixture CI proof is not valid JSON\n' >&2
    exit 1
fi
if ! cmp -s "$original_stream" "$normalized_stream"; then
    printf 'fixture CI proof must not contain duplicate object keys\n' >&2
    exit 1
fi
if ! jq -s -e --arg commit "$expected_commit" -f "$filter" "$normalized" \
    >/dev/null; then
    printf 'fixture CI proof does not exactly match the required v2 execution ledger\n' >&2
    exit 1
fi

# The Rust producer hashes each artifact's raw bytes. Recomputing the canonical
# framing here binds its ledger field to those byte digests, names, and sizes;
# it deliberately does not claim to re-read artifacts which no longer exist.
fixture_index=0
while [ "$fixture_index" -lt 16 ]; do
    ledger_entries="$work/entries.$fixture_index"
    if ! jq -r --argjson index "$fixture_index" \
        '.fixtures[$index].artifacts.entries[]
         | [.name, (.byte_count | tostring), .sha256]
         | join("|")' \
        "$normalized" >"$ledger_entries"; then
        printf 'fixture CI proof artifact ledger could not be extracted\n' >&2
        exit 1
    fi
    expected_ledger=$(jq -r --argjson index "$fixture_index" \
        '.fixtures[$index].artifacts.ledger_sha256' "$normalized") || exit 1
    computed_ledger=$("$ledger_calculator" "$ledger_entries") || exit 1
    if [ "$computed_ledger" != "$expected_ledger" ]; then
        printf 'fixture CI proof artifact ledger does not match its names, sizes, and content digests\n' >&2
        exit 1
    fi
    fixture_index=$((fixture_index + 1))
done
