#!/usr/bin/env bash

set -euo pipefail
umask 077

script_dir=${BASH_SOURCE[0]%/*}
[[ $script_dir != "${BASH_SOURCE[0]}" ]] || script_dir=.
top_dir=$(cd -- "$script_dir/../.." && pwd -P)
readonly top_dir
readonly archive="$top_dir/tests/fixtures/gluon/execution/archives/cast-python-module-fixture-1.0.0.tar"
if ! python=$(type -P python3); then
    printf '%s\n' 'python3 is required for the supplemental Python module host test' >&2
    exit 1
fi
readonly python
readonly host_path=$PATH
readonly expected_hash=c21e8157b3119a2453aa76de45672b7c8f63a761c973d4fdbd20893958b402f7
readonly wheel_name=cast_python_module_fixture-1.0.0-py3-none-any.whl
diagnostic_context='initializing the hostile-host Python fixture'

[[ -f $archive && ! -L $archive ]]
timeout --kill-after=2s 20s env -i \
    PATH="$host_path" \
    PYTHONDONTWRITEBYTECODE=1 \
    "$python" -I -c 'import hashlib, pathlib, sys
path = pathlib.Path(sys.argv[1])
assert path.stat().st_nlink == 1
assert hashlib.sha256(path.read_bytes()).hexdigest() == sys.argv[2]
' "$archive" "$expected_hash"

temporary=$(timeout --kill-after=2s 10s mktemp -d "${TMPDIR:-/tmp}/cast-python-module-host.XXXXXX")
readonly temporary

print_build_logs() {
    local log
    local -a logs
    shopt -s nullglob
    logs=("$temporary"/*-build.log)
    if ((${#logs[@]} == 0)); then
        printf '%s\n' 'no build log was created before the failure' >&2
        return
    fi
    for log in "${logs[@]}"; do
        printf '%s\n' \
            "--- ${log##*/} (preview capped at 65536 bytes) ---" >&2
        if ! timeout --kill-after=2s 10s head -c 65536 -- "$log" >&2; then
            printf '%s\n' 'could not read the bounded build-log preview' >&2
        fi
        printf '\n%s\n' "--- full build log preserved at: $log ---" >&2
    done
}

cleanup() {
    local status=$?
    trap - EXIT HUP INT TERM
    if ((status == 0)); then
        if timeout --kill-after=2s 10s rm -rf -- "$temporary"; then
            exit 0
        fi
        printf '%s\n' "could not remove successful fixture workspace: $temporary" >&2
        exit 1
    fi
    printf '%s\n' \
        "Python module host test failed while ${diagnostic_context} (status ${status})." \
        "Preserving the fixture workspace and build logs at: $temporary" >&2
    print_build_logs
    exit "$status"
}

trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
timeout --kill-after=2s 10s chmod 700 "$temporary"

assert_pinned_toolchain() {
    local home=$1
    timeout --kill-after=2s 20s install -d -m 700 "$home"
    timeout --kill-after=2s 30s env -i \
        HOME="$home" \
        PATH="$host_path" \
        LANG=C.UTF-8 \
        LC_ALL=C.UTF-8 \
        PYTHONDONTWRITEBYTECODE=1 \
        PYTHONHASHSEED=0 \
        PYTHONNOUSERSITE=1 \
        "$python" -I -c 'from importlib import import_module, metadata
import sys

try:
    from packaging.version import InvalidVersion, Version
except Exception as error:
    raise SystemExit(
        f"packaging 25.0 required exactly for PEP 440 validation: {error}"
    ) from error

exact_versions = {
    "setuptools": "82.0.1",
    "wheel": "0.47.0",
    "packaging": "25.0",
    "typing-extensions": "4.15.0",
}
required_modules = ("setuptools.build_meta", "wheel", "packaging.version", "typing_extensions")

def validate(python_version, version_lookup, module_importer):
    found_errors = []
    found_versions = {}
    if python_version != "3.14.3":
        found_errors.append(f"Python 3.14.3 required exactly, found {python_version}")
    for distribution, expected in exact_versions.items():
        try:
            found = version_lookup(distribution)
        except metadata.PackageNotFoundError:
            found_errors.append(
                f"{distribution} {expected} required exactly, distribution is absent"
            )
            continue
        found_versions[distribution] = found
        try:
            parsed = Version(found)
        except InvalidVersion:
            found_errors.append(f"{distribution} has non-PEP-440 version {found!r}")
            continue
        if parsed != Version(expected) or found != expected:
            found_errors.append(f"{distribution} {expected} required exactly, found {found}")
    for module in required_modules:
        try:
            module_importer(module)
        except Exception as error:
            found_errors.append(f"cannot import {module}: {error}")
    return found_errors, found_versions

def version_lookup(available):
    def lookup(distribution):
        try:
            return available[distribution]
        except KeyError:
            raise metadata.PackageNotFoundError(distribution) from None
    return lookup

def missing_module_importer(module):
    if module == "wheel":
        raise ImportError("blocked by hostile dependency probe")

missing_errors, _ = validate(
    "3.14.3",
    version_lookup({
        "setuptools": "82.0.1",
        "packaging": "25.0",
        "typing-extensions": "4.15.0",
    }),
    missing_module_importer,
)
expected_missing_errors = [
    "wheel 0.47.0 required exactly, distribution is absent",
    "cannot import wheel: blocked by hostile dependency probe",
]
if missing_errors != expected_missing_errors:
    raise SystemExit(
        "missing-dependency diagnostic self-test failed:\n  - " + "\n  - ".join(missing_errors)
    )

mismatched_errors, _ = validate(
    "3.14.4",
    version_lookup({
        "setuptools": "82.0.2",
        "wheel": "0.46.1",
        "packaging": "26.0",
        "typing-extensions": "4.15.1",
    }),
    lambda _: None,
)
expected_mismatched_errors = [
    "Python 3.14.3 required exactly, found 3.14.4",
    "setuptools 82.0.1 required exactly, found 82.0.2",
    "wheel 0.47.0 required exactly, found 0.46.1",
    "packaging 25.0 required exactly, found 26.0",
    "typing-extensions 4.15.0 required exactly, found 4.15.1",
]
if mismatched_errors != expected_mismatched_errors:
    raise SystemExit(
        "mismatched-dependency diagnostic self-test failed:\n  - "
        + "\n  - ".join(mismatched_errors)
    )

errors, versions = validate(
    sys.version.split()[0],
    metadata.version,
    import_module,
)

if errors:
    raise SystemExit("pinned Python toolchain is unsatisfied:\n  - " + "\n  - ".join(errors))

print(
    "validated exact Python toolchain: "
    f"Python {sys.version.split()[0]}, "
    + ", ".join(f"{name} {versions[name]}" for name in exact_versions)
)
'
}

extract_source() {
    local destination=$1
    timeout --kill-after=2s 10s mkdir -p "$destination"
    timeout --kill-after=2s 30s env -i \
        PATH="$host_path" \
        PYTHONDONTWRITEBYTECODE=1 \
        "$python" -I -c 'import pathlib, sys, tarfile
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
    timeout --kill-after=2s 10s mkdir -p "$home" "$dist"
    timeout --kill-after=2s 10s chmod 700 "$home"
    (
        cd "$source_root"
        umask 022
        timeout --kill-after=5s 120s env -i \
            HOME="$home" \
            PATH="$host_path" \
            LANG=C.UTF-8 \
            LC_ALL=C.UTF-8 \
            TZ=UTC \
            SOURCE_DATE_EPOCH=1700000000 \
            PYTHONHASHSEED=0 \
            PYTHONDONTWRITEBYTECODE=1 \
            PYTHONNOUSERSITE=1 \
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
            "$python" -s -P -c 'import setuptools.build_meta as backend, sys
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
    timeout --kill-after=2s 10s mkdir -p "$stage"
    timeout --kill-after=2s 30s env -i \
        PATH="$host_path" \
        PYTHONDONTWRITEBYTECODE=1 \
        "$python" -I -c 'import base64, csv, hashlib, io, pathlib, sys, zipfile
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

diagnostic_context='validating the pinned Python build and runtime toolchain'
assert_pinned_toolchain "$temporary/toolchain-home"

diagnostic_context='extracting the two deterministic source trees'
extract_source "$temporary/first"
extract_source "$temporary/second"
first_source="$temporary/first/cast-python-module-fixture-1.0.0"
second_source="$temporary/second/cast-python-module-fixture-1.0.0"
diagnostic_context='building the first deterministic wheel'
build_wheel "$first_source" "$temporary/home-first" "$temporary/first-build.log"
diagnostic_context='building the second deterministic wheel'
build_wheel "$second_source" "$temporary/home-second" "$temporary/second-build.log"
first_wheel="$first_source/dist/$wheel_name"
second_wheel="$second_source/dist/$wheel_name"
diagnostic_context='comparing independently built wheels byte for byte'
timeout --kill-after=2s 10s cmp -s "$first_wheel" "$second_wheel"

diagnostic_context='auditing the exact wheel layout, metadata, RECORD, and payloads'
inspect_wheel "$first_wheel" "$temporary/stage" "$first_source"
diagnostic_context='executing the installed wheel under the hostile environment'
actual=$(cd "$temporary" && timeout --kill-after=2s 30s env -i \
    PATH="$host_path" \
    PYTHONPATH="$temporary/stage" \
    PYTHONHASHSEED=0 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONNOUSERSITE=1 \
    "$python" -s -P -m cast_python_module_fixture --self-test)
[[ $actual == 'cast python module fixture: offline PEP 517 wheel' ]]
[[ ! -e $temporary/stage/cast_python_module_fixture/__pycache__ ]]

diagnostic_context='proving a tampered source tree cannot pass the content audit'
extract_source "$temporary/tampered"
tampered_source="$temporary/tampered/cast-python-module-fixture-1.0.0"
timeout --kill-after=2s 10s rm -- "$tampered_source/src/cast_python_module_fixture/codec.py"
build_wheel "$tampered_source" "$temporary/home-tampered" "$temporary/tampered-build.log"
if inspect_wheel "$tampered_source/dist/$wheel_name" "$temporary/tampered-stage" "$tampered_source" >/dev/null 2>&1; then
    printf '%s\n' 'tampered Python wheel passed the exact content audit' >&2
    exit 1
fi

diagnostic_context='reporting successful hostile-host validation'
printf '%s\n' 'supplemental hostile-host PEP 517 wheel checks passed (not delegated Stone execution)'
