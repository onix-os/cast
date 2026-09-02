local function line(value, rest)
    return value .. "\n" .. rest
end

-- This is intentionally one fixed, package-specific wrapper rather than a
-- generic wrapper abstraction. Its command, variable names, and environment are
-- closed here; the selected runtime record supplies only their exact paths and
-- typed runtime relations.
local function make(runtime)
    local wrapper_payload = line(
        "#!" .. runtime.interpreter.program.path,
        line(
            "export GRAPHITE_PLUGIN_DIR=" .. runtime.plugins.path,
            line(
                "export GRAPHITE_DATA_DIR=" .. runtime.data.path,
                line(
                    "export SSL_CERT_FILE=" .. runtime.certificates.path,
                    line(
                        "export GSETTINGS_SCHEMA_DIR=" .. runtime.schemas.path,
                        "exec " .. runtime.target.program.path .. " \"$@\"\n"
                    )
                )
            )
        )
    )

    -- The payload contains no single quote. Bash's printf builtin therefore
    -- writes the exact authored bytes without cat, env, or interpreter lookup.
    local install_script = "printf '%s' '"
        .. wrapper_payload
        .. "' > graphite-renderer\n"
        .. "/usr/bin/install -Dm755 graphite-renderer \"${CAST_INSTALL_ROOT}${CAST_BINDIR}/graphite-renderer\""

    return {
        meta = {
            pname = "graphite-renderer-wrapper",
            version = "1.0.0",
            release = 1,
            homepage = "https://example.invalid/graphite-renderer",
            license = { "MIT" },
        },
        builder = {
            kind = "custom",
            spec = {
                required_tools = {
                    runtime.interpreter.runtime_input,
                    { kind = "binary", value = "install" },
                },
                environment = {},
                phases = {
                    setup = { steps = {} },
                    build = { steps = {} },
                    install = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = runtime.interpreter.program,
                                declared_programs = {
                                    {
                                        path = "/usr/bin/install",
                                        requirement = { kind = "binary", value = "install" },
                                    },
                                },
                                script = install_script,
                            },
                        },
                    },
                    check = { steps = {} },
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
                summary = {
                    kind = "some",
                    value = "Graphite renderer with a closed runtime environment",
                },
                description = {
                    kind = "some",
                    value = "A source-less wrapper that binds one absolute renderer and every runtime resource without ambient discovery.",
                },
                provides_exclude = {},
                runtime_inputs = {
                    runtime.target.runtime_input,
                    runtime.plugins.runtime_input,
                    runtime.data.runtime_input,
                    runtime.certificates.runtime_input,
                    runtime.schemas.runtime_input,
                    runtime.interpreter.runtime_input,
                },
                runtime_exclude = {},
                paths = { { kind = "exe", path = "/usr/bin/graphite-renderer" } },
                conflicts = {},
            },
        },
        options = {
            toolchain = "llvm",
            cspgo = false,
            samplepgo = false,
            debug = true,
            strip = true,
            networking = false,
            compressman = false,
            lastrip = true,
        },
        architectures = { "native" },
    }
end

return { make = make }
