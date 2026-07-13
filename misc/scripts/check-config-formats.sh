#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

readonly allowed=$'.github/dependabot.yml\n.github/workflows/ci.yaml\n.github/workflows/release.yaml'

found="$({
    git ls-files '*.yaml' '*.yml' '*.kdl'
} | LC_ALL=C sort)"

if [[ "${found}" != "${allowed}" ]]; then
    echo "Unexpected tracked YAML/KDL files." >&2
    echo "OS Tools-owned configuration must be Gluon; only these external interfaces are allowed:" >&2
    printf '%s\n' "${allowed}" >&2
    echo >&2
    echo "Tracked YAML/KDL files:" >&2
    if [[ -n "${found}" ]]; then
        printf '%s\n' "${found}" >&2
    else
        echo "(none)" >&2
    fi
    exit 1
fi

echo "Configuration format allowlist is clean."
