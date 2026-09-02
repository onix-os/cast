local SHELL_SCRIPT = "cast-interpreter-shell"
local PYTHON_SCRIPT = "cast-interpreter-python"

-- These complete source-less payloads start with inert authored tokens. No
-- host file, recipe-directory input, interpreter discovery, or recursive
-- shebang scan participates.
local SETUP_SCRIPT = [[printf '%s\n' \
    '#!@BASH@' \
    'set -eu' \
    'test "${1-}" = "--self-test"' \
    'printf "%s\n" "explicit interpreter suite: bash"' \
    > cast-interpreter-shell
printf '%s\n' \
    '#!@PYTHON@' \
    'import sys' \
    'if sys.argv[1:] != ["--self-test"]:' \
    '    raise SystemExit(2)' \
    'print("explicit interpreter suite: python")' \
    > cast-interpreter-python]]

local INSTALL_SCRIPT = [[install -Dm755 cast-interpreter-shell "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-interpreter-shell"
install -Dm755 cast-interpreter-python "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-interpreter-python"]]

local function shebang_substitution(interpreter)
    return "s|^#!" .. interpreter.token .. "$|#!" .. interpreter.shebang .. "|"
end

local function make(interpreters)
    return {
        meta = {
            pname = "explicit-interpreter-suite",
            version = "1.0.0",
            release = 1,
            homepage = "https://example.invalid/explicit-interpreter-suite",
            license = { "MPL-2.0" },
        },
        builder = {
            kind = "custom",
            spec = {
                required_tools = {
                    interpreters.bash.runtime,
                    interpreters.python.runtime,
                    { kind = "binary", value = "sed" },
                    { kind = "binary", value = "grep" },
                    { kind = "binary", value = "install" },
                },
                environment = {},
                phases = {
                    setup = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = interpreters.bash.program,
                                declared_programs = {},
                                script = SETUP_SCRIPT,
                            },
                            {
                                kind = "run",
                                program = {
                                    path = "/usr/bin/sed",
                                    requirement = { kind = "binary", value = "sed" },
                                },
                                args = {
                                    "-i",
                                    "-e",
                                    shebang_substitution(interpreters.bash),
                                    SHELL_SCRIPT,
                                },
                            },
                            {
                                kind = "run",
                                program = {
                                    path = "/usr/bin/sed",
                                    requirement = { kind = "binary", value = "sed" },
                                },
                                args = {
                                    "-i",
                                    "-e",
                                    shebang_substitution(interpreters.python),
                                    PYTHON_SCRIPT,
                                },
                            },
                        },
                    },
                    build = { steps = {} },
                    install = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = interpreters.bash.program,
                                declared_programs = {
                                    {
                                        path = "/usr/bin/install",
                                        requirement = { kind = "binary", value = "install" },
                                    },
                                },
                                script = INSTALL_SCRIPT,
                            },
                        },
                    },
                    check = {
                        steps = {
                            {
                                kind = "run",
                                program = {
                                    path = "/usr/bin/grep",
                                    requirement = { kind = "binary", value = "grep" },
                                },
                                args = { "-Fqx", "#!" .. interpreters.bash.shebang, SHELL_SCRIPT },
                            },
                            {
                                kind = "run",
                                program = {
                                    path = "/usr/bin/grep",
                                    requirement = { kind = "binary", value = "grep" },
                                },
                                args = { "-Fqx", "#!" .. interpreters.python.shebang, PYTHON_SCRIPT },
                            },
                            -- A source-tree script is never passed to RunBuilt. Each check
                            -- invokes it through the exact typed interpreter capability.
                            {
                                kind = "run",
                                program = interpreters.bash.program,
                                args = { SHELL_SCRIPT, "--self-test" },
                            },
                            {
                                kind = "run",
                                program = interpreters.python.program,
                                args = { PYTHON_SCRIPT, "--self-test" },
                            },
                        },
                    },
                    workload = { steps = {} },
                },
                supported_hooks = {
                    setup = true,
                    build = true,
                    check = true,
                    install = true,
                    workload = true,
                },
            },
        },
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = { kind = "some", value = "Explicitly bound Bash and Python commands" },
                description = {
                    kind = "some",
                    value = "Two source-less scripts whose exact interpreters are authored, checked, and retained as runtime relations.",
                },
                provides_exclude = {},
                runtime_inputs = {
                    interpreters.bash.runtime,
                    interpreters.python.runtime,
                },
                runtime_exclude = {},
                paths = {
                    { kind = "exe", path = "/usr/bin/cast-interpreter-shell" },
                    { kind = "exe", path = "/usr/bin/cast-interpreter-python" },
                },
                conflicts = {},
            },
        },
        architectures = { "native" },
    }
end

return { make = make }
