#!/usr/bin/dash

# Supplemental only: this validates exact tracked and staged bytes with
# flake-provided host tools. It is not Stone/container execution, desktop
# activation, a live GUI/session/portal check, cache deployment, transaction,
# atomic update, rollback, boot, or Nix-compatibility proof.
set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
archive="$fixture_root/archives/cast-desktop-integration-fixture-1.0.0.tar"
authored="$fixture_root/source-trees/cast-desktop-integration-fixture-1.0.0"
expected_sha256=0f39867b15a8ae8f5386fdc768fd83e2874ac41f6e5c8c8711b5ce9a67887169

temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-desktop-integration-host.XXXXXXXX")
cleanup() {
    timeout 30s chmod -R u+w "$temporary" 2>/dev/null || :
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

actual_hash=$(timeout 10s sha256sum "$archive")
test "${actual_hash%% *}" = "$expected_sha256"

source_root="$temporary/source"
stage="$temporary/stage"
build="$temporary/build"
poison="$temporary/poison"
timeout 10s mkdir -p "$source_root" "$stage" "$build" \
    "$poison/home" "$poison/data-home" "$poison/data-dirs" "$poison/cache" \
    "$poison/schemas" "$poison/destdir"
timeout 10s install -m 0444 /dev/null "$poison/sentinel"
timeout 10s chmod 0555 "$poison" "$poison/home" "$poison/data-home" \
    "$poison/data-dirs" "$poison/cache" "$poison/schemas" "$poison/destdir"

poison_before="$temporary/poison-before"
poison_after="$temporary/poison-after"
timeout 10s find "$poison" -printf '%y %m %P\n' > "$temporary/poison-unsorted"
timeout 10s sort "$temporary/poison-unsorted" -o "$poison_before"

timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/cast-desktop-integration-fixture-1.0.0"
expected_inventory="$temporary/expected-inventory"
observed_inventory="$temporary/observed-inventory"
printf '%s\n' \
    'd 755 integration' \
    'f 644 COPYING' \
    'f 644 integration/application-x-cast-desktop-integration-fixture.xml' \
    'f 755 integration/cast-desktop-integration-fixture' \
    'f 644 integration/io.cast.desktop-integration-fixture.desktop' \
    'f 644 integration/io.cast.desktop-integration-fixture.gschema.xml' \
    'f 644 integration/io.cast.desktop-integration-fixture.metainfo.xml' \
    'f 644 integration/io.cast.desktop-integration-fixture.svg' \
    > "$expected_inventory"
timeout 10s sort "$expected_inventory" -o "$expected_inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' > "$temporary/inventory-unsorted"
timeout 10s sort "$temporary/inventory-unsorted" -o "$observed_inventory"
timeout 10s cmp "$expected_inventory" "$observed_inventory"

for relative in \
    COPYING \
    integration/application-x-cast-desktop-integration-fixture.xml \
    integration/cast-desktop-integration-fixture \
    integration/io.cast.desktop-integration-fixture.desktop \
    integration/io.cast.desktop-integration-fixture.gschema.xml \
    integration/io.cast.desktop-integration-fixture.metainfo.xml \
    integration/io.cast.desktop-integration-fixture.svg
do
    timeout 10s cmp "$authored/$relative" "$extracted/$relative"
done

timeout 10s install -Dm755 "$extracted/integration/cast-desktop-integration-fixture" \
    "$stage/usr/libexec/cast-desktop-integration-fixture"
timeout 10s install -Dm644 "$extracted/integration/io.cast.desktop-integration-fixture.desktop" \
    "$stage/usr/share/applications/io.cast.desktop-integration-fixture.desktop"
timeout 10s install -Dm644 "$extracted/integration/io.cast.desktop-integration-fixture.metainfo.xml" \
    "$stage/usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml"
timeout 10s install -Dm644 "$extracted/integration/io.cast.desktop-integration-fixture.gschema.xml" \
    "$stage/usr/share/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml"
timeout 10s install -Dm644 "$extracted/integration/application-x-cast-desktop-integration-fixture.xml" \
    "$stage/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml"
timeout 10s install -Dm644 "$extracted/integration/io.cast.desktop-integration-fixture.svg" \
    "$stage/usr/share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg"
timeout 10s install -Dm644 "$extracted/COPYING" \
    "$stage/usr/share/licenses/cast-desktop-integration-fixture/COPYING"

stage_expected="$temporary/stage-expected"
stage_actual="$temporary/stage-actual"
printf '%s\n' \
    '644 usr/share/applications/io.cast.desktop-integration-fixture.desktop' \
    '644 usr/share/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml' \
    '644 usr/share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg' \
    '644 usr/share/licenses/cast-desktop-integration-fixture/COPYING' \
    '644 usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml' \
    '644 usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml' \
    '755 usr/libexec/cast-desktop-integration-fixture' \
    > "$stage_expected"
timeout 10s sort "$stage_expected" -o "$stage_expected"
timeout 10s find "$stage" -type f -printf '%m %P\n' > "$temporary/stage-unsorted"
timeout 10s sort "$temporary/stage-unsorted" -o "$stage_actual"
timeout 10s cmp "$stage_expected" "$stage_actual"

host_env="HOME=$poison/home XDG_DATA_HOME=$poison/data-home XDG_DATA_DIRS=$poison/data-dirs XDG_CACHE_HOME=$poison/cache GSETTINGS_SCHEMA_DIR=$poison/schemas DBUS_SESSION_BUS_ADDRESS=unix:path=$poison/missing-bus DISPLAY=:9876 WAYLAND_DISPLAY=cast-missing LANGUAGE=zz_ZZ DESTDIR=$poison/destdir"
self_test=$(timeout 10s env $host_env dash "$stage/usr/libexec/cast-desktop-integration-fixture" --self-test)
test "$self_test" = 'cast-desktop-integration-fixture: self-test passed'
sample="$build/example.castdesk"
printf '%s\n' 'fixture document' > "$sample"
opened=$(timeout 10s env $host_env dash "$stage/usr/libexec/cast-desktop-integration-fixture" "$sample")
test "$opened" = "cast-desktop-integration-fixture: opened $sample"

timeout 30s env $host_env desktop-file-validate \
    "$stage/usr/share/applications/io.cast.desktop-integration-fixture.desktop"
timeout 30s env $host_env glib-compile-schemas --strict --dry-run \
    "$stage/usr/share/glib-2.0/schemas"
timeout 30s env $host_env appstreamcli validate --no-net --strict --pedantic \
    "$stage/usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml"
timeout 30s env $host_env xmllint --nonet --noout \
    "$stage/usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml" \
    "$stage/usr/share/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml" \
    "$stage/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml" \
    "$stage/usr/share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg"

mime_root="$build/mime-validation"
timeout 10s install -Dm644 \
    "$stage/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml" \
    "$mime_root/packages/application-x-cast-desktop-integration-fixture.xml"
timeout 30s env HOME="$poison/home" XDG_DATA_HOME="$build" XDG_DATA_DIRS="$build" \
    XDG_CACHE_HOME="$poison/cache" DBUS_SESSION_BUS_ADDRESS="unix:path=$poison/missing-bus" \
    DISPLAY=:9876 WAYLAND_DISPLAY=cast-missing LANGUAGE=zz_ZZ DESTDIR="$poison/destdir" \
    update-mime-database "$mime_root"
timeout 10s grep -Fqx 'application/x-cast-desktop-integration-fixture' "$mime_root/types"
timeout 10s grep -Fqx '80:application/x-cast-desktop-integration-fixture:*.castdesk' "$mime_root/globs2"

hash_tree() {
    tree=$1
    output=$2
    (CDPATH= cd -- "$tree" && timeout 10s find . -type f -print) > "$temporary/hash-paths"
    timeout 10s sort "$temporary/hash-paths" -o "$temporary/hash-paths"
    : > "$output"
    while IFS= read -r relative; do
        timeout 10s sha256sum "$tree/${relative#./}" >> "$output"
    done < "$temporary/hash-paths"
}
hash_tree "$mime_root" "$temporary/mime-first"
timeout 30s env HOME="$poison/home" XDG_DATA_HOME="$build" XDG_DATA_DIRS="$build" \
    XDG_CACHE_HOME="$poison/cache" DBUS_SESSION_BUS_ADDRESS="unix:path=$poison/missing-bus" \
    DISPLAY=:9876 WAYLAND_DISPLAY=cast-missing LANGUAGE=zz_ZZ DESTDIR="$poison/destdir" \
    update-mime-database "$mime_root"
hash_tree "$mime_root" "$temporary/mime-second"
timeout 10s cmp "$temporary/mime-first" "$temporary/mime-second"

for generated in \
    usr/share/glib-2.0/schemas/gschemas.compiled \
    usr/share/mime/mime.cache \
    usr/share/applications/mimeinfo.cache \
    usr/share/icons/hicolor/icon-theme.cache
do
    test ! -e "$stage/$generated"
    test ! -L "$stage/$generated"
done
timeout 10s find "$poison" -printf '%y %m %P\n' > "$temporary/poison-unsorted"
timeout 10s sort "$temporary/poison-unsorted" -o "$poison_after"
timeout 10s cmp "$poison_before" "$poison_after"

printf '%s\n' 'desktop-integration: supplemental offline staged host validation passed'
