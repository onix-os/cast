#!/bin/sh
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

paths=$(git ls-files | grep -Ei '(^|/)[^/]*(boulder|moss)[^/]*($|/)' || true)
if [ -n "$paths" ]; then
    printf 'Retired product names remain in tracked paths:\n%s\n' "$paths" >&2
    exit 1
fi

hits=$(mktemp)
trap 'rm -f "$hits"' EXIT HUP INT TERM

git grep -n -i -E 'boulder|moss' > "$hits" || true

unexpected=$(
    while IFS= read -r hit; do
        case "$hit" in
            ACKNOWLEDGMENTS.md:* | docs/plans/gluon-migration.md:*)
                # Attribution and the explicitly archived pre-Cast migration plan.
                ;;
            bin/cast/src/lib.rs:*'temporary.path().join("boulder.1")'* | \
                bin/cast/src/lib.rs:*'temporary.path().join("moss.1")'*)
                # Negative assertions: retired manpages must never be generated.
                ;;
            crates/stone_recipe/tests/build_policy.rs:* | \
                crates/stone_recipe/tests/build_policy_layers.rs:* | \
                crates/stone_recipe/tests/package_v3.rs:* | \
                crates/triggers/tests/gluon.rs:*)
                # Negative ABI tests: retired imports must fail closed.
                ;;
            misc/scripts/check-binary-layout.sh:*)
                # The binary-layout gate rejects the retired package names.
                ;;
            crates/forge/src/repository/mod.rs:*'"moss-root-index.json"'* | \
                crates/forge/src/repository/mod.rs:*'/moss-root-index.json"'*)
                # External repository wire protocol: this exact legacy filename is server-owned, not product branding.
                ;;
            misc/scripts/check-product-names.sh:*)
                # This gate necessarily names what it rejects.
                ;;
            *)
                printf '%s\n' "$hit"
                ;;
        esac
    done < "$hits"
)

if [ -n "$unexpected" ]; then
    printf 'Active retired product names remain:\n%s\n' "$unexpected" >&2
    exit 1
fi

printf 'Product naming is clean: Cast is the sole public identity.\n'
