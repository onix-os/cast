#!/usr/bin/env bash

set -euo pipefail

readonly script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly checker="${script_dir}/check-config-formats.sh"
readonly work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

readonly -a allowed=(
    '.github/dependabot.yml'
    '.github/workflows/ci.yaml'
    '.github/workflows/release.yaml'
)

case_number=0

write_fixture() {
    local fixture=$1
    shift
    printf '%s\0' "$@" > "${fixture}"
}

write_tracked_file() {
    local path=$1
    local contents=$2

    mkdir -p -- "${work_dir}/$(dirname -- "${path}")"
    printf '%s\n' "${contents}" > "${work_dir}/${path}"
}

pass_case() {
    local name=$1
    shift
    local fixture="${work_dir}/$((++case_number)).paths0"
    local output="${fixture}.out"

    write_fixture "${fixture}" "$@"
    if ! "${checker}" \
        --repo-root "${work_dir}" \
        --tracked-paths0 "${fixture}" > "${output}" 2>&1; then
        echo "FAIL: ${name} should pass" >&2
        cat "${output}" >&2
        exit 1
    fi
}

fail_case() {
    local name=$1
    local expected=$2
    shift 2
    local fixture="${work_dir}/$((++case_number)).paths0"
    local output="${fixture}.out"

    write_fixture "${fixture}" "$@"
    if "${checker}" \
        --repo-root "${work_dir}" \
        --tracked-paths0 "${fixture}" > "${output}" 2>&1; then
        echo "FAIL: ${name} should fail" >&2
        cat "${output}" >&2
        exit 1
    fi
    if ! grep -F -- "${expected}" "${output}" >/dev/null; then
        echo "FAIL: ${name} did not report '${expected}'" >&2
        cat "${output}" >&2
        exit 1
    fi
}

pass_content_case() {
    local name=$1
    local path=$2
    local contents=$3

    write_tracked_file "${path}" "${contents}"
    pass_case "${name}" "${allowed[@]}" "${path}"
}

fail_content_case() {
    local name=$1
    local expected=$2
    local path=$3
    local contents=$4

    write_tracked_file "${path}" "${contents}"
    fail_case "${name}" "${expected}" "${allowed[@]}" "${path}"
}

pass_case \
    'exact allowlist plus unrelated tracked paths' \
    "${allowed[@]}" \
    'crates/mason/data/policy/default.glu' \
    'docs/name.glu' \
    'source/libyaml-helper.rs' \
    'source/looks-yamlish'

fail_case \
    'lowercase YAML outside allowlist' \
    'config/legacy.yaml' \
    "${allowed[@]}" \
    'config/legacy.yaml'

fail_case \
    'uppercase YAML extension' \
    'config/legacy.YAML' \
    "${allowed[@]}" \
    'config/legacy.YAML'

fail_case \
    'mixed-case YML extension' \
    'config/legacy.YmL' \
    "${allowed[@]}" \
    'config/legacy.YmL'

fail_case \
    'mixed-case KDL extension' \
    'config/legacy.KdL' \
    "${allowed[@]}" \
    'config/legacy.KdL'

fail_case \
    'compound YAML extension' \
    'recipes/stone.yaml.glu' \
    "${allowed[@]}" \
    'recipes/stone.yaml.glu'

fail_case \
    'compound mixed-case YML extension' \
    'recipes/stone.YmL.in' \
    "${allowed[@]}" \
    'recipes/stone.YmL.in'

fail_case \
    'KDL directory component' \
    'fixtures/control.kdl/canonical.glu' \
    "${allowed[@]}" \
    'fixtures/control.kdl/canonical.glu'

fail_case \
    'allowlisted path with a compound suffix is not allowlisted' \
    '.github/dependabot.yml.backup' \
    "${allowed[@]}" \
    '.github/dependabot.yml.backup'

fail_case \
    'allowlisted file with wrong case' \
    '.github/dependabot.YML' \
    '.github/dependabot.YML' \
    '.github/workflows/ci.yaml' \
    '.github/workflows/release.yaml'

fail_case \
    'missing required interface' \
    '.github/workflows/release.yaml' \
    '.github/dependabot.yml' \
    '.github/workflows/ci.yaml'

fail_content_case \
    'direct YAML dependency in a Cargo manifest' \
    'Forbidden YAML/KDL Cargo dependency references' \
    'Cargo.toml' \
    'serde_yaml = "0.9"'

fail_content_case \
    'KDL dependency in a member Cargo manifest' \
    'crates/example/Cargo.toml:2: kdl.workspace = true' \
    'crates/example/Cargo.toml' \
    $'[dependencies]\nkdl.workspace = true'

fail_content_case \
    'transitive YAML dependency in Cargo.lock' \
    'Cargo.lock:2: name = "unsafe-libyaml"' \
    'Cargo.lock' \
    $'[[package]]\nname = "unsafe-libyaml"\nversion = "0.2.11"'

fail_content_case \
    'removed YAML workspace path dependency' \
    'crates/yaml' \
    'crates/example/Cargo.toml' \
    'legacy = { path = "../../crates/yaml" }'

fail_content_case \
    'KDL namespace in owned runtime source' \
    'crates/example/src/lib.rs:1: kdl::KdlDocument::parse(source)' \
    'crates/example/src/lib.rs' \
    'kdl::KdlDocument::parse(source)'

fail_content_case \
    'YAML loader symbol in owned runtime source' \
    'bin/example/src/main.rs:1: fn load_yaml_configuration() {}' \
    'bin/example/src/main.rs' \
    'fn load_yaml_configuration() {}'

fail_content_case \
    'legacy recipe fallback path in owned runtime source' \
    'stone.yaml' \
    'bin/example/src/main.rs' \
    'let fallback = root.join("stone.yaml");'

pass_content_case \
    'unrelated symlink identifier is not a YML reference' \
    'crates/example/src/lib.rs' \
    'pub fn symlink_target() {}'

pass_content_case \
    'exact no-fallback negative-test strings' \
    'crates/config/src/gluon/tests.rs' \
    $'fn load_gluon_never_falls_back_to_yaml_or_kdl() {\nwrite(temporary.path().join("dummy.yaml"), "not: valid: yaml");\nwrite(temporary.path().join("dummy.kdl"), "not valid kdl {");\nwrite(path.with_extension("yaml"), "legacy: true");\nassert!(path.with_extension("kdl").exists());\n}'

fail_content_case \
    'negative-test line is allowed only inside the test module' \
    'Forbidden YAML/KDL owned-runtime references' \
    'crates/config/src/gluon.rs' \
    'write(temporary.path().join("dummy.yaml"), "not: valid: yaml");'

fail_content_case \
    'negative-test source path is not a blanket exception' \
    'serde_yaml::from_str' \
    'crates/config/src/gluon/tests.rs' \
    'let legacy = serde_yaml::from_str(source)?;'

pass_content_case \
    'exact package-search YAML strings are data, not loaders' \
    'crates/forge/src/cli/search.rs' \
    $'#[cfg(test)]\nmod tests {\n"libyaml",\n"YAML 1.1 library",\n&[provider(pkgconfig, "yaml-0.1")],\nfn test_provider_soname_finds_libyaml() {\n}\n}'

pass_content_case \
    'upstream Ruby index name is not owned configuration' \
    'crates/mason/src/draft/build/ruby.rs' \
    '"checksums.yaml.gz" if file.depth() == 0 => state.increment_confidence(50),'

pass_content_case \
    'exact desktop bootstrap libfyaml package metadata is test data' \
    'crates/mason/src/planner/tests/bootstrap/desktop_integration.rs' \
    $'const LIBFYAML_PACKAGE_ID: &str = "a035a7509f5d58b2d4072ccd0f8b450e5b58504bdf61857bd3d2b08d1cd641eb";\nLIBFYAML_PACKAGE_ID,\n"libfyaml",\n"../../../pool/v0/libf/libfyaml/libfyaml-0.9.6-7-1-x86_64.stone",'

fail_content_case \
    'desktop bootstrap test path is not a blanket YAML exception' \
    'serde_yaml::from_str' \
    'crates/mason/src/planner/tests/bootstrap/desktop_integration.rs' \
    'let legacy = serde_yaml::from_str(source)?;'

pass_content_case \
    'completed historical migration document is an audit exception' \
    'docs/plans/gluon-migration.md' \
    'The former loader called serde_yaml::from_slice and parsed control.kdl.'

pass_content_case \
    'exact README removal statement is an audit exception' \
    'README.md' \
    'Cast does not fall back to YAML or KDL. The only YAML allowlist belongs to'

fail_content_case \
    'obsolete README wording is not retained in the exception set' \
    'README.md:1: OS Tools does not fall back to YAML or KDL. The only YAML allowlist is' \
    'README.md' \
    'OS Tools does not fall back to YAML or KDL. The only YAML allowlist is'

pass_content_case \
    'exact acknowledgment migration statement is an audit exception' \
    'ACKNOWLEDGMENTS.md' \
    'origin. Onix is taking responsibility for replacing the inherited YAML/KDL'

fail_content_case \
    'README is not a whole-file exception' \
    'README.md:1: Recipes may use YAML compatibility loaders.' \
    'README.md' \
    'Recipes may use YAML compatibility loaders.'

pass_content_case \
    'exact PLAN removal requirement is an audit exception' \
    'PLAN.md' \
    '- YAML and KDL support must be completely absent from owned configuration,'

fail_content_case \
    'PLAN is not a whole-file exception' \
    'PLAN.md:1: Keep the YAML fallback until a later release.' \
    'PLAN.md' \
    'Keep the YAML fallback until a later release.'

fail_content_case \
    'new compatibility documentation requires explicit review' \
    'Unreviewed YAML/KDL documentation references' \
    'docs/compatibility.md' \
    'Recipes may fall back to stone.yaml through serde_yaml.'

fail_content_case \
    'reStructuredText documentation is scanned' \
    'docs/compatibility.rst:1: Recipes may fall back to stone.yaml.' \
    'docs/compatibility.rst' \
    'Recipes may fall back to stone.yaml.'

fail_content_case \
    'AsciiDoc documentation is scanned' \
    'docs/compatibility.adoc:1: Recipes may fall back to control.kdl.' \
    'docs/compatibility.adoc' \
    'Recipes may fall back to control.kdl.'

fail_content_case \
    'text documentation is scanned' \
    'docs/compatibility.txt:1: Recipes may fall back to stone.yaml.' \
    'docs/compatibility.txt' \
    'Recipes may fall back to stone.yaml.'

fail_content_case \
    'extensionless root documentation is scanned' \
    'NOTICE:1: Legacy KDL configuration remains supported.' \
    'NOTICE' \
    'Legacy KDL configuration remains supported.'

echo "Configuration format gate self-tests passed (${case_number} cases)."
