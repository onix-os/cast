#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

readonly script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly checker="${script_dir}/check-config-formats.sh"
readonly work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

readonly -a allowed=(
    '.github/dependabot.yml'
    '.github/workflows/ci.yaml'
    '.github/workflows/release.yaml'
)

case_number=0

write_fixture() {
    local fixture=$1
    shift
    printf '%s\0' "$@" > "${fixture}"
}

pass_case() {
    local name=$1
    shift
    local fixture="${work_dir}/$((++case_number)).paths0"
    local output="${fixture}.out"

    write_fixture "${fixture}" "$@"
    if ! "${checker}" --tracked-paths0 "${fixture}" > "${output}" 2>&1; then
        echo "FAIL: ${name} should pass" >&2
        cat "${output}" >&2
        exit 1
    fi
}

fail_case() {
    local name=$1
    local expected=$2
    shift 2
    local fixture="${work_dir}/$((++case_number)).paths0"
    local output="${fixture}.out"

    write_fixture "${fixture}" "$@"
    if "${checker}" --tracked-paths0 "${fixture}" > "${output}" 2>&1; then
        echo "FAIL: ${name} should fail" >&2
        cat "${output}" >&2
        exit 1
    fi
    if ! grep -F -- "${expected}" "${output}" >/dev/null; then
        echo "FAIL: ${name} did not report '${expected}'" >&2
        cat "${output}" >&2
        exit 1
    fi
}

pass_case \
    'exact allowlist plus unrelated tracked paths' \
    "${allowed[@]}" \
    'bin/boulder/data/policy/default.glu' \
    'docs/name.yaml.glu' \
    'source/looks-yamlish'

fail_case \
    'lowercase YAML outside allowlist' \
    'config/legacy.yaml' \
    "${allowed[@]}" \
    'config/legacy.yaml'

fail_case \
    'uppercase YAML extension' \
    'config/legacy.YAML' \
    "${allowed[@]}" \
    'config/legacy.YAML'

fail_case \
    'mixed-case YML extension' \
    'config/legacy.YmL' \
    "${allowed[@]}" \
    'config/legacy.YmL'

fail_case \
    'mixed-case KDL extension' \
    'config/legacy.KdL' \
    "${allowed[@]}" \
    'config/legacy.KdL'

fail_case \
    'allowlisted file with wrong case' \
    '.github/dependabot.YML' \
    '.github/dependabot.YML' \
    '.github/workflows/ci.yaml' \
    '.github/workflows/release.yaml'

fail_case \
    'missing required interface' \
    '.github/workflows/release.yaml' \
    '.github/dependabot.yml' \
    '.github/workflows/ci.yaml'

echo "Configuration format gate self-tests passed (${case_number} cases)."
