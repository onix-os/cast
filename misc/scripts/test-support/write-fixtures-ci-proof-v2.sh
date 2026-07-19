#!/bin/sh

set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s <absolute-output-path> <canonical-git-commit>\n' "$0" >&2
    exit 2
fi

output=$1
commit=$2
script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
ledger_calculator="$script_directory/../calculate-fixtures-ci-ledger.sh"
case "$output" in
    /*) ;;
    *) printf 'test fixture proof output path must be absolute: %s\n' "$output" >&2; exit 2 ;;
esac
case "$commit" in
    ''|*[!0-9a-f]*)
        printf 'test fixture proof commit must be canonical lowercase hexadecimal\n' >&2
        exit 2
        ;;
esac
if [ "${#commit}" -ne 40 ] && [ "${#commit}" -ne 64 ]; then
    printf 'test fixture proof commit must contain exactly 40 or 64 hexadecimal bytes\n' >&2
    exit 2
fi
if [ -L "$output" ] || { [ -e "$output" ] && [ ! -f "$output" ]; }; then
    printf 'refusing unsafe test fixture proof output: %s\n' "$output" >&2
    exit 1
fi
parent=${output%/*}
[ -n "$parent" ] || parent=/
if [ ! -d "$parent" ] || [ -L "$parent" ]; then
    printf 'test fixture proof parent must be an existing non-symlink directory: %s\n' \
        "$parent" >&2
    exit 1
fi
if [ -L "$ledger_calculator" ] || [ ! -f "$ledger_calculator" ] \
    || [ ! -x "$ledger_calculator" ]; then
    printf 'test fixture proof ledger calculator is unavailable or unsafe: %s\n' \
        "$ledger_calculator" >&2
    exit 1
fi

umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-proof-v2-generator.XXXXXXXXXXXX")
complete=0
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$complete" -ne 1 ]; then
        rm -f "$output"
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

set -- \
    autotools \
    autotools-options \
    cargo \
    cargo-features \
    cargo-vendored \
    cmake \
    custom \
    daemon-generated \
    desktop-integration \
    external-test-vectors \
    factory-override \
    font-family \
    generated-config \
    generated-shell \
    gettext-localization \
    go-module \
    header-only-library \
    hooks-patch \
    meson \
    multiple-sources \
    plugin-output \
    post-install-smoke-test \
    python-module \
    split \
    system-integration-assets \
    userspace-profile

stone_count_for_fixture() {
    case "$1" in
        autotools|autotools-options|cargo|cargo-features|cargo-vendored|cmake|custom|factory-override|hooks-patch|meson|multiple-sources|post-install-smoke-test)
            stone_count=9
            ;;
        header-only-library) stone_count=2 ;;
        daemon-generated|plugin-output) stone_count=3 ;;
        split) stone_count=5 ;;
        desktop-integration|external-test-vectors|font-family|generated-config|generated-shell|gettext-localization|go-module|python-module|system-integration-assets|userspace-profile) stone_count=1 ;;
        *) printf 'unknown test fixture proof fixture: %s\n' "$1" >&2; exit 1 ;;
    esac
}

fixture_total_bytes() {
    fixture_number=$1
    fixture_stones=$2
    fixture_total=0
    stone_number=1
    while [ "$stone_number" -le "$fixture_stones" ]; do
        fixture_total=$((fixture_total + 1000 + fixture_number * 100 + stone_number))
        stone_number=$((stone_number + 1))
    done
    fixture_total=$((fixture_total + 2000 + fixture_number * 100))
    fixture_total=$((fixture_total + 2001 + fixture_number * 100))
}

hex64() {
    printf '%064x' "$1"
}

artifact_bytes=0
fixture_number=1
for fixture_name in "$@"; do
    stone_count_for_fixture "$fixture_name"
    fixture_total_bytes "$fixture_number" "$stone_count"
    artifact_bytes=$((artifact_bytes + fixture_total))
    fixture_number=$((fixture_number + 1))
done

: >"$output"
chmod 600 "$output"
cat >>"$output" <<EOF_HEADER
{
  "schema": "cast.fixtures-ci-proof.v2",
  "git_commit": "$commit",
  "git_tree": "clean",
  "selection": "all",
  "required_execution": true,
  "bundle_ledger_schema": "cast.fixtures-ci.bundle.v1",
  "totals": {
    "fixture_count": 26,
    "execution_count": 52,
    "bundle_validation_count": 78,
    "stone_count": 131,
    "manifest_count": 52,
    "artifact_count": 183,
    "artifact_bytes": $artifact_bytes
  },
  "fixtures": [
EOF_HEADER

fixture_number=1
for fixture_name in "$@"; do
    stone_count_for_fixture "$fixture_name"
    fixture_total_bytes "$fixture_number" "$stone_count"
    artifact_count=$((stone_count + 2))
    plan_bytes=$((3000 + fixture_number))
    plan_hash=$(hex64 "$fixture_number")
    lock_bytes=$((4000 + fixture_number))
    lock_hash=$(hex64 "$((100 + fixture_number))")
    entries_file="$work/entries.$fixture_number"
    : >"$entries_file"
    stone_number=1
    while [ "$stone_number" -le "$stone_count" ]; do
        entry_name=$(printf 'artifact-%02d.stone' "$stone_number")
        entry_bytes=$((1000 + fixture_number * 100 + stone_number))
        entry_hash=$(hex64 "$((fixture_number * 1000 + stone_number))")
        printf '%s|%s|%s\n' "$entry_name" "$entry_bytes" "$entry_hash" \
            >>"$entries_file"
        stone_number=$((stone_number + 1))
    done
    manifest_bin_bytes=$((2000 + fixture_number * 100))
    manifest_jsonc_bytes=$((2001 + fixture_number * 100))
    manifest_bin_hash=$(hex64 "$((fixture_number * 1000 + 100))")
    manifest_jsonc_hash=$(hex64 "$((fixture_number * 1000 + 101))")
    printf '%s|%s|%s\n' \
        manifest.x86_64.bin "$manifest_bin_bytes" "$manifest_bin_hash" \
        >>"$entries_file"
    printf '%s|%s|%s\n' \
        manifest.x86_64.jsonc "$manifest_jsonc_bytes" "$manifest_jsonc_hash" \
        >>"$entries_file"
    ledger_hash=$("$ledger_calculator" "$entries_file")
    cat >>"$output" <<EOF_FIXTURE
    {
      "name": "$fixture_name",
      "plans": {
        "first": {"byte_count": $plan_bytes, "sha256": "$plan_hash", "derivation_id": "$plan_hash"},
        "repeat": {"byte_count": $plan_bytes, "sha256": "$plan_hash", "derivation_id": "$plan_hash"}
      },
      "build_locks": {
        "first": {"write_outcome": "written", "byte_count": $lock_bytes, "sha256": "$lock_hash"},
        "repeat": {"write_outcome": "unchanged", "byte_count": $lock_bytes, "sha256": "$lock_hash"}
      },
      "publications": {"first": "published", "repeat": "reused"},
      "artifacts": {
        "stone_count": $stone_count,
        "manifest_count": 2,
        "artifact_count": $artifact_count,
        "total_bytes": $fixture_total,
        "ledger_sha256": "$ledger_hash",
        "entries": [
EOF_FIXTURE
    entry_number=1
    while IFS='|' read -r entry_name entry_bytes entry_hash; do
        case "$entry_name" in
            manifest.x86_64.bin) entry_kind=manifest-bin ;;
            manifest.x86_64.jsonc) entry_kind=manifest-jsonc ;;
            *.stone) entry_kind=stone ;;
            *) printf 'unknown generated fixture artifact: %s\n' "$entry_name" >&2; exit 1 ;;
        esac
        if [ "$entry_number" -lt "$artifact_count" ]; then
            entry_suffix=,
        else
            entry_suffix=
        fi
        printf '          {"name": "%s", "kind": "%s", "byte_count": %s, "sha256": "%s"}%s\n' \
            "$entry_name" "$entry_kind" "$entry_bytes" "$entry_hash" \
            "$entry_suffix" >>"$output"
        entry_number=$((entry_number + 1))
    done <"$entries_file"
    [ "$entry_number" -eq "$((artifact_count + 1))" ] || exit 1
    cat >>"$output" <<EOF_ENTRIES
        ]
      },
      "bundle_observations": [
        {"point": "published-after-first", "artifact_count": $artifact_count, "total_bytes": $fixture_total, "ledger_sha256": "$ledger_hash"},
        {"point": "staged-after-repeat", "artifact_count": $artifact_count, "total_bytes": $fixture_total, "ledger_sha256": "$ledger_hash"},
        {"point": "published-after-repeat", "artifact_count": $artifact_count, "total_bytes": $fixture_total, "ledger_sha256": "$ledger_hash"}
      ]
    }
EOF_ENTRIES
    if [ "$fixture_number" -lt 26 ]; then
        printf '    ,\n' >>"$output"
    fi
    fixture_number=$((fixture_number + 1))
done

cat >>"$output" <<'EOF_FOOTER'
  ],
  "result": "passed"
}
EOF_FOOTER
chmod 644 "$output"
complete=1
