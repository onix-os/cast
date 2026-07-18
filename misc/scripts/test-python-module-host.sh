#!/usr/bin/env bash

set -euo pipefail

top_dir=$(cd -- "$(dirname -- "$0")/../.." && pwd -P)
archive="$top_dir/tests/fixtures/gluon/execution/archives/cast-python-module-fixture-1.0.0.tar"
python=$(command -v python3)
host_path=$PATH
expected_hash=c21e8157b3119a2453aa76de45672b7c8f63a761c973d4fdbd20893958b402f7
wheel_name=cast_python_module_fixture-1.0.0-py3-none-any.whl

[[ -f $archive && ! -L $archive ]]
"$python" -c 'import hashlib, os, pathlib, sys
path = pathlib.Path(sys.argv[1])
assert path.stat().st_nlink == 1
assert hashlib.sha256(path.read_bytes()).hexdigest() == sys.argv[2]
' "$archive" "$expected_hash"

temporary=$(mktemp -d "${TMPDIR:-/tmp}/cast-python-module-host.XXXXXX")
trap 'rm -rf -- "$temporary"' EXIT HUP INT TERM
chmod 700 "$temporary"

extract_source() {
    local destination=$1
    mkdir -p "$destination"
    "$python" -c 'import pathlib, sys, tarfile
archive = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
root = "cast-python-module-fixture-1.0.0"
expected = {
    root,
    f"{root}/LICENSE",
    f"{root}/README.md",
    f"{root}/pyproject.toml",
    f"{root}/src",
    f"{root}/src/cast_python_module_fixture",
    f"{root}/src/cast_python_module_fixture/__init__.py",
    f"{root}/src/cast_python_module_fixture/__main__.py",
    f"{root}/src/cast_python_module_fixture/codec.py",
    f"{root}/tests",
    f"{root}/tests/test_codec.py",
}
with tarfile.open(archive, "r:") as source:
    members = source.getmembers()
    assert {member.name for member in members} == expected
    for member in members:
        path = pathlib.PurePosixPath(member.name)
        assert not path.is_absolute() and ".." not in path.parts
        assert member.isdir() or member.isfile()
        assert not member.issym() and not member.islnk()
        assert member.uid == 0 and member.gid == 0
        assert member.mode == (0o755 if member.isdir() else 0o644)
    source.extractall(destination, filter="data")
' "$archive" "$destination"
}

build_wheel() {
    local source_root=$1
    local home=$2
    local log=$3
    local dist=$source_root/dist
    mkdir -p "$home" "$dist"
    chmod 700 "$home"
    (
        cd "$source_root"
        umask 022
        env -i \
            HOME="$home" \
            PATH="$host_path" \
            LANG=C.UTF-8 \
            LC_ALL=C.UTF-8 \
            TZ=UTC \
            SOURCE_DATE_EPOCH=1700000000 \
            PYTHONHASHSEED=0 \
            PYTHONDONTWRITEBYTECODE=1 \
            PIP_CONFIG_FILE=/dev/null \
            PIP_DISABLE_PIP_VERSION_CHECK=1 \
            PIP_NO_INDEX=1 \
            PIP_REQUIRE_VIRTUALENV=1 \
            http_proxy=http://127.0.0.1:9 \
            https_proxy=http://127.0.0.1:9 \
            HTTP_PROXY=http://127.0.0.1:9 \
            HTTPS_PROXY=http://127.0.0.1:9 \
            ALL_PROXY=socks5://127.0.0.1:9 \
            NO_PROXY= \
            "$python" -c 'import setuptools.build_meta as backend, sys
name = backend.build_wheel(sys.argv[1])
assert name == "cast_python_module_fixture-1.0.0-py3-none-any.whl"
' "$dist" >"$log" 2>&1
    )
    local wheels=("$dist"/*.whl)
    [[ ${#wheels[@]} -eq 1 && ${wheels[0]} == "$dist/$wheel_name" && -s ${wheels[0]} ]]
}

inspect_wheel() {
    local wheel=$1
    local stage=$2
    local source_root=$3
    mkdir -p "$stage"
    "$python" -c 'import base64, csv, hashlib, io, pathlib, sys, zipfile
wheel = pathlib.Path(sys.argv[1])
stage = pathlib.Path(sys.argv[2])
source_root = pathlib.Path(sys.argv[3])
dist_info = "cast_python_module_fixture-1.0.0.dist-info"
expected = [
    "cast_python_module_fixture/__init__.py",
    "cast_python_module_fixture/__main__.py",
    "cast_python_module_fixture/codec.py",
    f"{dist_info}/licenses/LICENSE",
    f"{dist_info}/METADATA",
    f"{dist_info}/WHEEL",
    f"{dist_info}/entry_points.txt",
    f"{dist_info}/top_level.txt",
    f"{dist_info}/RECORD",
]
with zipfile.ZipFile(wheel) as archive:
    infos = archive.infolist()
    names = [info.filename for info in infos]
    assert names == expected
    assert len(names) == len(set(names))
    for info in infos:
        path = pathlib.PurePosixPath(info.filename)
        assert not path.is_absolute() and ".." not in path.parts
        assert not info.is_dir()
        assert info.date_time == (2023, 11, 14, 22, 13, 20)
        expected_mode = 0o664 if info.filename == f"{dist_info}/RECORD" else 0o644
        assert ((info.external_attr >> 16) & 0o7777) == expected_mode
    metadata = archive.read(f"{dist_info}/METADATA").decode("utf-8")
    for marker in [
        "Name: cast-python-module-fixture\n",
        "Version: 1.0.0\n",
        "Requires-Python: >=3.14\n",
        "Requires-Dist: typing-extensions>=4.15\n",
    ]:
        assert metadata.count(marker) == 1
    assert archive.read(f"{dist_info}/entry_points.txt") == (
        b"[console_scripts]\n"
        b"cast-python-module-fixture = cast_python_module_fixture.__main__:main\n"
    )
    assert archive.read(f"{dist_info}/top_level.txt") == b"cast_python_module_fixture\n"
    record = list(csv.reader(io.TextIOWrapper(
        archive.open(f"{dist_info}/RECORD"), encoding="utf-8", newline=""
    )))
    assert {row[0] for row in record} == set(names)
    assert len(record) == len(names) and all(len(row) == 3 for row in record)
    record_by_name = {row[0]: row[1:] for row in record}
    for name in names:
        digest, length = record_by_name[name]
        if name == f"{dist_info}/RECORD":
            assert digest == "" and length == ""
            continue
        payload = archive.read(name)
        expected_digest = base64.urlsafe_b64encode(hashlib.sha256(payload).digest()).rstrip(b"=").decode("ascii")
        assert digest == f"sha256={expected_digest}"
        assert length == str(len(payload))
    tracked_payloads = {
        "cast_python_module_fixture/__init__.py": source_root / "src/cast_python_module_fixture/__init__.py",
        "cast_python_module_fixture/__main__.py": source_root / "src/cast_python_module_fixture/__main__.py",
        "cast_python_module_fixture/codec.py": source_root / "src/cast_python_module_fixture/codec.py",
        f"{dist_info}/licenses/LICENSE": source_root / "LICENSE",
    }
    for name, tracked in tracked_payloads.items():
        assert archive.read(name) == tracked.read_bytes()
    archive.extractall(stage)
' "$wheel" "$stage" "$source_root" || return 1
    [[ ! -e $stage/cast_python_module_fixture/__pycache__ ]]
    [[ ! -e $stage/tests && ! -e $stage/pyproject.toml && ! -e $stage/setup.py ]]
}

extract_source "$temporary/first"
extract_source "$temporary/second"
first_source="$temporary/first/cast-python-module-fixture-1.0.0"
second_source="$temporary/second/cast-python-module-fixture-1.0.0"
build_wheel "$first_source" "$temporary/home-first" "$temporary/first-build.log"
build_wheel "$second_source" "$temporary/home-second" "$temporary/second-build.log"
first_wheel="$first_source/dist/$wheel_name"
second_wheel="$second_source/dist/$wheel_name"
cmp -s "$first_wheel" "$second_wheel"

inspect_wheel "$first_wheel" "$temporary/stage" "$first_source"
actual=$(cd "$temporary" && env -i \
    PATH="$host_path" \
    PYTHONPATH="$temporary/stage" \
    PYTHONHASHSEED=0 \
    PYTHONDONTWRITEBYTECODE=1 \
    "$python" -m cast_python_module_fixture --self-test)
[[ $actual == 'cast python module fixture: offline PEP 517 wheel' ]]
[[ ! -e $temporary/stage/cast_python_module_fixture/__pycache__ ]]

extract_source "$temporary/tampered"
tampered_source="$temporary/tampered/cast-python-module-fixture-1.0.0"
rm -- "$tampered_source/src/cast_python_module_fixture/codec.py"
build_wheel "$tampered_source" "$temporary/home-tampered" "$temporary/tampered-build.log"
if inspect_wheel "$tampered_source/dist/$wheel_name" "$temporary/tampered-stage" "$tampered_source" >/dev/null 2>&1; then
    printf '%s\n' 'tampered Python wheel passed the exact content audit' >&2
    exit 1
fi

printf '%s\n' 'supplemental hostile-host PEP 517 wheel checks passed (not delegated Stone execution)'
