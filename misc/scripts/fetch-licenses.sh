#!/usr/bin/env bash

set -euo pipefail

destination="${1:-target/license-list-data}"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

git clone --depth=1 https://github.com/spdx/license-list-data \
    "$work_dir/license-list-data"

rm -rf "$destination"
mkdir -p "$destination/text"
cp "$work_dir/license-list-data/text/"* "$destination/text/"
