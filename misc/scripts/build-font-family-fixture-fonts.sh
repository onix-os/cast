#!/usr/bin/dash

set -eu

mode=write
case "${1-}" in
    '') ;;
    --check) mode=check ;;
    *) printf 'usage: %s [--check]\n' "$0" >&2; exit 2 ;;
esac

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
tree="$root/tests/fixtures/gluon/execution/source-trees/cast-font-family-fixture-1.0.0"
source_file="$tree/source/generate_cast_aster_fixture.rs"
tracked="$tree/fonts"
temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-font-family-generator.XXXXXXXX")

cleanup() {
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

generator="$temporary/generate-cast-aster-fixture"
generated="$temporary/fonts"
timeout 60s rustc --edition=2024 -C opt-level=2 "$source_file" -o "$generator"
timeout 10s mkdir -m 0700 "$generated"
timeout 10s env SOURCE_DATE_EPOCH=1700000000 "$generator" "$generated"

for name in CastAsterFixture-Bold.ttf CastAsterFixture-Regular.ttf; do
    case "$mode" in
        check)
            timeout 10s cmp "$generated/$name" "$tracked/$name"
            ;;
        write)
            timeout 10s install -m 0644 "$generated/$name" "$tracked/$name"
            ;;
    esac
done

if [ "$mode" = check ]; then
    printf '%s\n' 'font-family: deterministic TrueType regeneration passed'
fi
