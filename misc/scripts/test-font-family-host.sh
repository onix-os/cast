#!/usr/bin/dash

# Supplemental only: this proves deterministic tracked font bytes, direct
# hostile-environment metadata scanning, and exact staged placement. It is not
# graphical rendering, system font discovery, cache activation, a transaction,
# rollback, boot, or Nix-compatibility proof.
set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
tree_name=cast-font-family-fixture-1.0.0
archive="$fixture_root/archives/$tree_name.tar"
authored="$fixture_root/source-trees/$tree_name"
font_builder="$root/misc/scripts/build-font-family-fixture-fonts.sh"
expected_archive_sha256=8710f0728fbde240fd94ce8bce46c4e4d71336b8470416e8da7c0895dc2d700c

temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-font-family-host.XXXXXXXX")
cleanup() {
    timeout 30s chmod -R u+w "$temporary" 2>/dev/null || :
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

timeout 120s dash "$font_builder" --check
archive_hash=$(timeout 10s sha256sum "$archive")
test "${archive_hash%% *}" = "$expected_archive_sha256"
test "$(timeout 10s stat -c '%a:%s' "$archive")" = 644:30720

source_root="$temporary/source"
stage="$temporary/stage"
poison="$temporary/poison"
timeout 10s mkdir -p "$source_root" "$stage" "$poison/config" \
    "$poison/home" "$poison/cache" "$poison/data" "$poison/destdir"
timeout 10s cat >"$poison/config/fonts.conf" <<'EOF'
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/definitely-not-a-cast-font-directory</dir>
  <cachedir>/definitely-not-a-writable-cast-cache</cachedir>
</fontconfig>
EOF
timeout 10s install -m 0444 /dev/null "$poison/sentinel"
timeout 10s chmod 0444 "$poison/config/fonts.conf"
timeout 10s chmod 0555 "$poison" "$poison/config" "$poison/home" \
    "$poison/cache" "$poison/data" "$poison/destdir"

timeout 10s find "$poison" -printf '%y %m %P\n' >"$temporary/poison-unsorted"
timeout 10s sort "$temporary/poison-unsorted" -o "$temporary/poison-before"
timeout 10s sha256sum "$poison/config/fonts.conf" "$poison/sentinel" \
    >"$temporary/poison-hashes-before"

timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/$tree_name"
timeout 10s cat >"$temporary/expected-inventory" <<'EOF'
d 755 fonts
d 755 source
f 644 OFL.txt
f 644 PROVENANCE
f 644 fonts/CastAsterFixture-Bold.ttf
f 644 fonts/CastAsterFixture-Regular.ttf
f 644 source/generate_cast_aster_fixture.rs
EOF
timeout 10s sort "$temporary/expected-inventory" -o "$temporary/expected-inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' \
    >"$temporary/observed-inventory-unsorted"
timeout 10s sort "$temporary/observed-inventory-unsorted" \
    -o "$temporary/observed-inventory"
timeout 10s cmp "$temporary/expected-inventory" "$temporary/observed-inventory"

for relative in \
    OFL.txt \
    PROVENANCE \
    fonts/CastAsterFixture-Bold.ttf \
    fonts/CastAsterFixture-Regular.ttf \
    source/generate_cast_aster_fixture.rs
do
    timeout 10s cmp "$authored/$relative" "$extracted/$relative"
done

font_root="$stage/usr/share/fonts/truetype/cast-aster-fixture"
timeout 10s install -Dm644 "$extracted/fonts/CastAsterFixture-Regular.ttf" \
    "$font_root/CastAsterFixture-Regular.ttf"
timeout 10s install -Dm644 "$extracted/fonts/CastAsterFixture-Bold.ttf" \
    "$font_root/CastAsterFixture-Bold.ttf"
timeout 10s install -Dm644 "$extracted/OFL.txt" \
    "$stage/usr/share/licenses/cast-font-family-fixture/OFL.txt"

timeout 10s cat >"$temporary/expected-stage" <<'EOF'
644 usr/share/fonts/truetype/cast-aster-fixture/CastAsterFixture-Bold.ttf
644 usr/share/fonts/truetype/cast-aster-fixture/CastAsterFixture-Regular.ttf
644 usr/share/licenses/cast-font-family-fixture/OFL.txt
EOF
timeout 10s find "$stage" -type f -printf '%m %P\n' \
    >"$temporary/observed-stage-unsorted"
timeout 10s sort "$temporary/observed-stage-unsorted" -o "$temporary/observed-stage"
timeout 10s cmp "$temporary/expected-stage" "$temporary/observed-stage"

scan_font() {
    font=$1
    timeout 30s env \
        HOME="$poison/home" \
        XDG_CACHE_HOME="$poison/cache" \
        XDG_CONFIG_HOME="$poison/config" \
        XDG_DATA_HOME="$poison/data" \
        FONTCONFIG_FILE="$poison/config/fonts.conf" \
        FONTCONFIG_PATH="$poison/config" \
        LANG=C \
        LC_ALL=C \
        TZ=Pacific/Kiritimati \
        SOURCE_DATE_EPOCH=1 \
        DESTDIR="$poison/destdir" \
        fc-scan --format \
        '%{family[0]}|%{style[0]}|%{fontformat}|%{fullname[0]}|%{postscriptname}\n' \
        "$font"
}

for root_under_test in "$authored/fonts" "$extracted/fonts" "$font_root"; do
    regular=$(scan_font "$root_under_test/CastAsterFixture-Regular.ttf")
    bold=$(scan_font "$root_under_test/CastAsterFixture-Bold.ttf")
    test "$regular" = \
        'Cast Aster Fixture|Regular|TrueType|Cast Aster Fixture Regular|CastAsterFixture-Regular'
    test "$bold" = \
        'Cast Aster Fixture|Bold|TrueType|Cast Aster Fixture Bold|CastAsterFixture-Bold'
done

test "$(timeout 10s sha256sum "$authored/fonts/CastAsterFixture-Regular.ttf")" = \
    '2e8f53f901eed7937f2ae68651055cb2e8a45e14ae37e53e72dda1813457ce4e  '"$authored/fonts/CastAsterFixture-Regular.ttf"
test "$(timeout 10s sha256sum "$authored/fonts/CastAsterFixture-Bold.ttf")" = \
    'd911446419017339d2efc88a700b908bc0322239cf90de606339fd63c936b017  '"$authored/fonts/CastAsterFixture-Bold.ttf"

if timeout 10s find "$stage" \
    \( -name 'fonts.cache-*' -o -name fonts.dir -o -name fonts.scale \) \
    -print -quit | timeout 10s grep -q .; then
    printf '%s\n' 'font-family fixture leaked a generated font cache' >&2
    exit 1
else
    status=$?
    test "$status" -eq 1
fi
timeout 10s find "$poison" -printf '%y %m %P\n' >"$temporary/poison-unsorted"
timeout 10s sort "$temporary/poison-unsorted" -o "$temporary/poison-after"
timeout 10s cmp "$temporary/poison-before" "$temporary/poison-after"
timeout 10s sha256sum "$poison/config/fonts.conf" "$poison/sentinel" \
    >"$temporary/poison-hashes-after"
timeout 10s cmp "$temporary/poison-hashes-before" "$temporary/poison-hashes-after"

printf '%s\n' 'font-family: supplemental deterministic hostile-host validation passed'
