#!/bin/sh
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
metadata=$(cd "$root" && cargo metadata --format-version 1 --no-deps)

binaries=$(printf '%s' "$metadata" | jq -c '[.packages[].targets[] | select(.kind | index("bin")) | .name] | sort')
if [ "$binaries" != '["cast"]' ]; then
    printf 'Expected Cast to be the sole binary target, found: %s\n' "$binaries" >&2
    exit 1
fi

retired_packages=$(
    printf '%s' "$metadata" \
        | jq -c '[.packages[].name | select(. == "boulder" or . == "moss")] | sort'
)
if [ "$retired_packages" != '[]' ]; then
    printf 'Retired executable packages remain in the workspace: %s\n' "$retired_packages" >&2
    exit 1
fi

printf 'Binary layout is clean: cast is the sole executable.\n'
