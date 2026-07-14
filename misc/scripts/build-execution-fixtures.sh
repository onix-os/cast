#!/bin/sh
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
source_root="$fixture_root/source-trees"
archive_root="$fixture_root/archives"
temporary=

cleanup() {
    if [ -n "$temporary" ]; then
        rm -f "$temporary"
    fi
}

trap cleanup EXIT HUP INT TERM

mkdir -p "$archive_root"

for fixture in \
    cast-autotools-fixture-1.0.0 \
    cast-cargo-fixture-1.0.0 \
    cast-cmake-fixture-1.0.0 \
    cast-custom-fixture-1.0.0 \
    cast-meson-fixture-1.0.0 \
    cast-split-fixture-1.0.0
do
    source="$source_root/$fixture"
    output="$archive_root/$fixture.tar"
    temporary="$archive_root/.$fixture.tar.tmp"

    test -d "$source"
    rm -f "$temporary"
    tar \
        --format=ustar \
        --sort=name \
        --mtime=@1700000000 \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        --mode='u+rwX,go+rX,go-w' \
        -C "$source_root" \
        -cf "$temporary" \
        "$fixture"
    chmod 0644 "$temporary"
    mv -f "$temporary" "$output"
    temporary=
done
