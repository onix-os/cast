#!/usr/bin/env bash

# Supplemental only: this independently compiles the exact tracked catalogs
# and build-only consumer with host tools. It is not a Stone/container run,
# runtime-dependency, locale deployment, activation, or Nix-compatibility proof.
set -euo pipefail
export LANGUAGE=

root="$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)"
fixture_root="$root/tests/fixtures/gluon/execution"
archive="$fixture_root/archives/cast-gettext-localization-fixture-1.0.0.tar"
authored="$fixture_root/source-trees/cast-gettext-localization-fixture-1.0.0"
expected_sha256=1e6b0b3267767853eb622e4155d3c50ecff677f8f3b305f5e4e1470f91fc1e5d

temporary="$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-gettext-localization-host.XXXXXXXX")"
cleanup() {
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

actual_hash="$(timeout 10s sha256sum "$archive")"
timeout 10s test "${actual_hash%% *}" = "$expected_sha256"

source_root="$temporary/source"
build="$temporary/build"
stage="$temporary/stage"
timeout 10s mkdir -p "$source_root" "$build/locale/fr/LC_MESSAGES" "$build/locale/de/LC_MESSAGES" "$stage"
timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/cast-gettext-localization-fixture-1.0.0"

expected_inventory="$temporary/expected-inventory"
observed_inventory="$temporary/observed-inventory"
unsorted_inventory="$temporary/unsorted-inventory"
printf '%s\n' \
    'd 755 po' \
    'f 644 COPYING' \
    'f 644 consumer.c' \
    'f 644 po/de.po' \
    'f 644 po/fr.po' \
    > "$expected_inventory"
timeout 10s sort "$expected_inventory" -o "$expected_inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' > "$unsorted_inventory"
timeout 10s sort "$unsorted_inventory" -o "$observed_inventory"
timeout 10s cmp "$expected_inventory" "$observed_inventory"

for relative in COPYING consumer.c po/de.po po/fr.po; do
    timeout 10s cmp "$authored/$relative" "$extracted/$relative"
done

timeout 30s msgfmt \
    --check-format \
    --check-header \
    -o "$build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo" \
    "$extracted/po/fr.po"
timeout 30s msgfmt \
    --check-format \
    --check-header \
    -o "$build/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo" \
    "$extracted/po/de.po"
fr_catalog_hash="$(timeout 10s sha256sum "$build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo")"
de_catalog_hash="$(timeout 10s sha256sum "$build/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo")"
timeout 10s test "${fr_catalog_hash%% *}" = 04b1a9c041b5ab2fe85001ce8403659cdace30989b56ff554d5a26703950f0a0
timeout 10s test "${de_catalog_hash%% *}" = 9fe202517728c7c64ccfedf2e97ef15467b94c9cbb8c981f7c69aa517fa7c7e4
timeout 30s "${CC:-cc}" \
    -std=c11 \
    -O2 \
    -g \
    -Wall \
    -Wextra \
    -Werror \
    -fstack-protector-strong \
    -D_FORTIFY_SOURCE=3 \
    -fPIE \
    "$extracted/consumer.c" \
    -Wl,-pie \
    -Wl,--build-id=sha1 \
    -Wl,-z,relro,-z,now \
    -Wl,-z,noexecstack \
    -Wl,-z,separate-code \
    -Wl,--as-needed \
    -o "$build/gettext-consumer"

timeout 10s "$build/gettext-consumer" fr_FR.utf8 "$build/locale" 'Bonjour de Cast'
timeout 10s "$build/gettext-consumer" de_DE.utf8 "$build/locale" 'Hallo von Cast'

timeout 10s install -Dm644 \
    "$build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo" \
    "$stage/usr/share/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo"
timeout 10s install -Dm644 \
    "$build/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo" \
    "$stage/usr/share/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo"
timeout 10s install -Dm644 "$extracted/COPYING" \
    "$stage/usr/share/licenses/cast-gettext-localization-fixture/COPYING"

stage_inventory="$temporary/stage-inventory"
timeout 10s find "$stage" -type f -printf '%m %P\n' > "$stage_inventory"
timeout 10s test "$(timeout 10s wc -l < "$stage_inventory")" -eq 3
timeout 10s grep -Fqx '644 usr/share/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo' "$stage_inventory"
timeout 10s grep -Fqx '644 usr/share/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo' "$stage_inventory"
timeout 10s grep -Fqx '644 usr/share/licenses/cast-gettext-localization-fixture/COPYING' "$stage_inventory"
timeout 10s test ! -e "$stage/usr/bin/gettext-consumer"

hidden_catalog="$temporary/fr.mo"
timeout 10s mv "$build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo" "$hidden_catalog"
set +e
timeout 10s "$build/gettext-consumer" fr_FR.utf8 "$build/locale" 'Bonjour de Cast' \
    >"$temporary/fallback.stdout" 2>"$temporary/fallback.stderr"
fallback_status=$?
set -e
timeout 10s test "$fallback_status" -eq 67
timeout 10s grep -Fqx 'untranslated gettext fallback rejected' "$temporary/fallback.stderr"

printf '%s\n' 'gettext-localization: supplemental host compilation and translation validation passed'
