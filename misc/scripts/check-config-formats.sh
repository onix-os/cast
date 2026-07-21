#!/usr/bin/env bash

set -euo pipefail

readonly -a allowed_config_files=(
    '.github/dependabot.yml'
    '.github/workflows/ci.yaml'
    '.github/workflows/release.yaml'
)

usage() {
    cat <<'EOF'
Usage: check-config-formats.sh [--repo-root DIR] [--tracked-paths0 FILE]

Prove that owned configuration is Gluon-only by rejecting:

  * tracked paths with YAML or KDL components outside the exact
    external-service allowlist;
  * YAML/KDL dependency references in tracked Cargo manifests and Cargo.lock;
  * YAML/KDL references in owned runtime source, except exact negative-test and
    package-search fixtures; and
  * YAML/KDL prose outside narrow historical and negative audit allowances.

FILE must contain NUL-delimited tracked paths. Without the option, paths are
read from `git ls-files -z` below DIR. DIR defaults to the Git worktree root.
EOF
}

repo_root=''
tracked_paths_file=''
while (( $# != 0 )); do
    case "$1" in
        --repo-root)
            if [[ $# -lt 2 ]]; then
                usage >&2
                exit 2
            fi
            repo_root=$2
            shift 2
            ;;
        --tracked-paths0)
            if [[ $# -lt 2 ]]; then
                usage >&2
                exit 2
            fi
            tracked_paths_file=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "${repo_root}" ]]; then
    repo_root=$(git rev-parse --show-toplevel)
fi
if [[ ! -d "${repo_root}" ]]; then
    echo "Repository root is not a directory: ${repo_root}" >&2
    exit 2
fi
repo_root=$(cd -- "${repo_root}" && pwd -P)

declare -A seen_config_files=()
declare -a found_config_files=()
declare -a missing_config_files=()
declare -a unexpected_config_files=()
declare -a dependency_references=()
declare -a runtime_references=()
declare -a documentation_references=()

trim_line() {
    local value=$1
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s' "${value}"
}

contains_format_reference() {
    local lower=${1,,}

    [[ "${lower}" == *yaml* \
        || "${lower}" == *kdl* \
        || "${lower}" =~ (^|[^[:alnum:]])yml([^[:alnum:]]|$) ]]
}

is_allowed_runtime_reference() {
    local path=$1
    local line=$2
    local in_test_module=$3

    case "${path}" in
        'crates/config/src/gluon/tests.rs')
            # This whole file is the cfg(test)-only external test module for
            # the Gluon loader. Keep the exception bound to its canonical
            # path and to the exact negative-test statements below.
            case "${line}" in
                'fn load_gluon_never_falls_back_to_yaml_or_kdl() {' \
                    | 'write(temporary.path().join("dummy.yaml"), "not: valid: yaml");' \
                    | 'write(temporary.path().join("dummy.kdl"), "not valid kdl {");' \
                    | 'write(temporary.path().join("dummy.d/fragment.yaml"), "\"yaml\"");' \
                    | 'write(temporary.path().join("dummy.d/fragment.kdl"), "\"kdl\"");' \
                    | 'write(path.with_extension("yaml"), "legacy: true");' \
                    | 'write(path.with_extension("kdl"), "legacy #true");' \
                    | 'assert!(path.with_extension("yaml").exists());' \
                    | 'assert!(path.with_extension("kdl").exists());')
                    return 0
                    ;;
            esac
            ;;
        'crates/forge/src/cli/search.rs')
            [[ "${in_test_module}" == true ]] || return 1
            case "${line}" in
                '"libyaml",' \
                    | '"YAML 1.1 library",' \
                    | '&[provider(soname, "libyaml-0.so.2(x86_64)")],' \
                    | '"libyaml-devel",' \
                    | '"Development files for libyaml",' \
                    | '&[provider(pkgconfig, "yaml-0.1")],' \
                    | 'fn test_provider_soname_finds_libyaml() {' \
                    | 'let output_provides_flag = test_handle("search --provides=soname libyaml-0.so.2(x86_64)");' \
                    | 'let output_dependency_syntax = test_handle("search soname(libyaml-0.so.2(x86_64))");' \
                    | 'assert_eq!(names_provides_flag, vec!["libyaml"]);' \
                    | 'fn test_provider_pkgconfig_finds_libyaml_devel() {' \
                    | 'let output_provides_flag = test_handle("search --provides=pkgconfig yaml-0.1");' \
                    | 'let output_dependency_syntax = test_handle("search pkgconfig(yaml-0.1)");' \
                    | 'assert_eq!(names_provides_flag, vec!["libyaml-devel"]);')
                    return 0
                    ;;
            esac
            ;;
        'crates/mason/src/draft/build/ruby.rs')
            # RubyGems publishes this upstream index name; Cast does not
            # parse it as OS Tools configuration.
            if [[ "${line}" == '"checksums.yaml.gz" if file.depth() == 0 => state.increment_confidence(50),' ]]; then
                return 0
            fi
            ;;
        'crates/mason/src/planner/tests/bootstrap/desktop_integration.rs')
            # The pinned AppStream closure contains libfyaml package metadata.
            # Bind this test-data exception to its exact path, ID, name, and
            # Stone URI; it is not a YAML loader or configuration dependency.
            case "${line}" in
                'const LIBFYAML_PACKAGE_ID: &str = "a035a7509f5d58b2d4072ccd0f8b450e5b58504bdf61857bd3d2b08d1cd641eb";' \
                    | 'LIBFYAML_PACKAGE_ID,' \
                    | '"libfyaml",' \
                    | '"../../../pool/v0/libf/libfyaml/libfyaml-0.9.6-7-1-x86_64.stone",')
                    return 0
                    ;;
            esac
            ;;
    esac

    return 1
}

is_config_format_path() {
    local remainder=$1
    local component
    local lower

    # Treat the format name as a complete dot-delimited component, not merely
    # as the final extension. This catches paths such as stone.yaml.glu and
    # directories such as fixtures/control.kdl/ without flagging libyaml.
    while true; do
        component=${remainder%%/*}
        lower=${component,,}
        case ".${lower}." in
            *'.yaml.'* | *'.yml.'* | *'.kdl.'*) return 0 ;;
        esac

        [[ "${remainder}" == */* ]] || return 1
        remainder=${remainder#*/}
    done
}

is_allowed_config_file() {
    local candidate=$1
    local allowed

    for allowed in "${allowed_config_files[@]}"; do
        if [[ "${candidate}" == "${allowed}" ]]; then
            return 0
        fi
    done
    return 1
}

is_documentation_path() {
    local path=$1
    local lower=${path,,}

    case "${lower}" in
        *.md | *.rst | *.adoc | *.txt) return 0 ;;
    esac

    # Extensionless and unusually suffixed project documents at the repository
    # root are prose too. Do not let renaming README.md to README hide a new
    # compatibility promise from this gate.
    [[ "${path}" != */* ]] || return 1
    case "${lower}" in
        readme | readme.* \
            | plan | plan.* \
            | acknowledgments | acknowledgments.* \
            | acknowledgements | acknowledgements.* \
            | authors | authors.* \
            | changelog | changelog.* \
            | contributing | contributing.* \
            | copying | copying.* \
            | license | license.* \
            | notice | notice.* \
            | security | security.* \
            | code_of_conduct | code_of_conduct.* \
            | code-of-conduct | code-of-conduct.*)
            return 0
            ;;
    esac
    return 1
}

is_allowed_documentation_reference() {
    local path=$1
    local line=$2

    # This exact document is the completed historical migration record. It is
    # retained to explain what was removed, not as current compatibility
    # documentation. Other plans and readmes receive no whole-file exemption.
    if [[ "${path}" == 'docs/plans/gluon-migration.md' ]]; then
        return 0
    fi

    case "${path}" in
        'README.md')
            case "${line}" in
                'Cast does not fall back to YAML or KDL. The only YAML allowlist belongs to' \
                    | '`config-formats` gate so owned YAML or KDL paths fail validation.')
                    return 0
                    ;;
            esac
            ;;
        'ACKNOWLEDGMENTS.md')
            if [[ "${line}" == 'origin. Onix is taking responsibility for replacing the inherited YAML/KDL' ]]; then
                return 0
            fi
            ;;
        'PLAN.md')
            case "${line}" in
                '> YAML/KDL removal guarantees rather than reopening the format migration.' \
                    | '- YAML and KDL support must be completely absent from owned configuration,' \
                    | 'documentation. YAML required by external services such as GitHub is not OS' \
                    | '- [x] YAML and KDL configuration paths have been removed from OS Tools.' \
                    | 'legacy YAML block syntax.' \
                    | '- [x] Audit the repository for YAML/KDL loaders, fallbacks, compatibility' \
                    | 'paths, examples, and documentation. The only owned YAML files are the' \
                    | '`config-formats` gate rejects any tracked YAML/KDL path outside the exact' \
                    | '- no OS Tools YAML/KDL compatibility path remains;' \
                    | 'runs, Mason executes only that plan, and no YAML, KDL, legacy recipe, or')
                    return 0
                    ;;
            esac
            ;;
        'docs/gluon-configuration.md')
            case "${line}" in
                'boundary. YAML and KDL are not compatibility formats and are not used as' \
                    | 'OS Tools configuration has no YAML or KDL compatibility loader, fallback, or' \
                    | 'dual-write path. The YAML updater, KDL control-file overlay, and KDL' \
                    | 'KDL system-model round trip were removed. A file using an old configuration' \
                    | 'The exact external-service YAML allowlist is `.github/dependabot.yml`,' \
                    | '`.github/workflows/ci.yaml`, and `.github/workflows/release.yaml`. No KDL files' \
                    | 'are tracked. Negative no-fallback tests, package names containing “yaml”, and' \
                    | 'YAML/KDL paths with this exact allowlist, and `make test` runs the target before' \
                    | 'tools; YAML/KDL and their compatibility dependencies are absent from the final')
                    return 0
                    ;;
            esac
            ;;
    esac

    return 1
}

scan_file() {
    local path=$1
    local absolute_path="${repo_root}/${path}"
    local line
    local line_number=0
    local rendered
    local in_test_module=false

    [[ -f "${absolute_path}" ]] || return 0

    case "${path}" in
        'Cargo.lock' | 'Cargo.toml' | */Cargo.toml)
            while IFS= read -r line || [[ -n "${line}" ]]; do
                line_number=$((line_number + 1))
                if contains_format_reference "${line}"; then
                    rendered=$(trim_line "${line}")
                    dependency_references+=("${path}:${line_number}: ${rendered}")
                fi
            done < "${absolute_path}"
            return 0
            ;;
    esac

    case "${path}" in
        bin/*.rs | bin/*.glu | bin/*.py | bin/*.c | bin/*.h | bin/*.sh \
            | bin/*/*.rs | bin/*/*.glu | bin/*/*.py | bin/*/*.c | bin/*/*.h | bin/*/*.sh \
            | crates/*.rs | crates/*.glu | crates/*.py | crates/*.c | crates/*.h | crates/*.sh \
            | crates/*/*.rs | crates/*/*.glu | crates/*/*.py | crates/*/*.c | crates/*/*.h | crates/*/*.sh)
            while IFS= read -r line || [[ -n "${line}" ]]; do
                line_number=$((line_number + 1))
                if [[ "${line}" =~ ^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$ ]]; then
                    in_test_module=true
                fi
                if ! contains_format_reference "${line}"; then
                    continue
                fi
                rendered=$(trim_line "${line}")
                if ! is_allowed_runtime_reference "${path}" "${rendered}" "${in_test_module}"; then
                    runtime_references+=("${path}:${line_number}: ${rendered}")
                fi
            done < "${absolute_path}"
            ;;
        *)
            is_documentation_path "${path}" || return 0
            while IFS= read -r line || [[ -n "${line}" ]]; do
                line_number=$((line_number + 1))
                if contains_format_reference "${line}"; then
                    rendered=$(trim_line "${line}")
                    if ! is_allowed_documentation_reference "${path}" "${rendered}"; then
                        documentation_references+=("${path}:${line_number}: ${rendered}")
                    fi
                fi
            done < "${absolute_path}"
            ;;
    esac
}

inspect_path() {
    local path=$1

    # Git paths are byte strings. Format components are ASCII
    # case-insensitive; complete allowlisted paths remain case-sensitive.
    if is_config_format_path "${path}"; then
        found_config_files+=("${path}")
        if is_allowed_config_file "${path}"; then
            seen_config_files["${path}"]=1
        else
            unexpected_config_files+=("${path}")
        fi
    fi

    scan_file "${path}"
}

if [[ -n "${tracked_paths_file}" ]]; then
    while IFS= read -r -d '' path; do
        inspect_path "${path}"
    done < "${tracked_paths_file}"
else
    while IFS= read -r -d '' path; do
        inspect_path "${path}"
    done < <(git -C "${repo_root}" ls-files -z)
fi

for path in "${allowed_config_files[@]}"; do
    if [[ ! -v "seen_config_files[${path}]" ]]; then
        missing_config_files+=("${path}")
    fi
done

if (( ${#found_config_files[@]} != 0 )); then
    mapfile -t found_config_files < <(printf '%s\n' "${found_config_files[@]}" | LC_ALL=C sort)
fi
if (( ${#missing_config_files[@]} != 0 )); then
    mapfile -t missing_config_files < <(printf '%s\n' "${missing_config_files[@]}" | LC_ALL=C sort)
fi
if (( ${#unexpected_config_files[@]} != 0 )); then
    mapfile -t unexpected_config_files < <(printf '%s\n' "${unexpected_config_files[@]}" | LC_ALL=C sort)
fi
if (( ${#dependency_references[@]} != 0 )); then
    mapfile -t dependency_references < <(printf '%s\n' "${dependency_references[@]}" | LC_ALL=C sort)
fi
if (( ${#runtime_references[@]} != 0 )); then
    mapfile -t runtime_references < <(printf '%s\n' "${runtime_references[@]}" | LC_ALL=C sort)
fi
if (( ${#documentation_references[@]} != 0 )); then
    mapfile -t documentation_references < <(printf '%s\n' "${documentation_references[@]}" | LC_ALL=C sort)
fi

if (( ${#missing_config_files[@]} != 0 \
    || ${#unexpected_config_files[@]} != 0 \
    || ${#dependency_references[@]} != 0 \
    || ${#runtime_references[@]} != 0 \
    || ${#documentation_references[@]} != 0 )); then
    echo "Configuration format gate failed." >&2
    echo "OS Tools-owned configuration must be Gluon; only these external interfaces are allowed:" >&2
    printf '%s\n' "${allowed_config_files[@]}" >&2

    if (( ${#missing_config_files[@]} != 0 || ${#unexpected_config_files[@]} != 0 )); then
        echo >&2
        echo "Tracked YAML/KDL files:" >&2
        if (( ${#found_config_files[@]} != 0 )); then
            printf '%s\n' "${found_config_files[@]}" >&2
        else
            echo "(none)" >&2
        fi
    fi
    if (( ${#missing_config_files[@]} != 0 )); then
        echo >&2
        echo "Missing required external interfaces:" >&2
        printf '%s\n' "${missing_config_files[@]}" >&2
    fi
    if (( ${#unexpected_config_files[@]} != 0 )); then
        echo >&2
        echo "Disallowed tracked YAML/KDL files:" >&2
        printf '%s\n' "${unexpected_config_files[@]}" >&2
    fi
    if (( ${#dependency_references[@]} != 0 )); then
        echo >&2
        echo "Forbidden YAML/KDL Cargo dependency references:" >&2
        printf '%s\n' "${dependency_references[@]}" >&2
    fi
    if (( ${#runtime_references[@]} != 0 )); then
        echo >&2
        echo "Forbidden YAML/KDL owned-runtime references:" >&2
        printf '%s\n' "${runtime_references[@]}" >&2
    fi
    if (( ${#documentation_references[@]} != 0 )); then
        echo >&2
        echo "Unreviewed YAML/KDL documentation references:" >&2
        printf '%s\n' "${documentation_references[@]}" >&2
    fi
    exit 1
fi

echo "Configuration format gate is clean: paths, dependencies, runtime sources, and docs."
