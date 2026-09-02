-- Dash is retained by the Shell executable edge; install is the only authored
-- builder tool. This package declares no compile/link step or compiler
-- dependency. Repository policy still supplies its selected frozen toolchain
-- independently of this artifact contract.
local function make(artifact)
    return {
        meta = {
            pname = "quartz-inspector-bin",
            version = "3.4.1",
            release = 1,
            homepage = "https://example.invalid/quartz-inspector",
            license = { "Apache-2.0" },
        },
        builder = {
            kind = "custom",
            spec = {
                required_tools = { { kind = "binary", value = "install" } },
                environment = {},
                phases = {
                    setup = { steps = {} },
                    build = { steps = {} },
                    -- Dash interprets one install-only script. The interpreter
                    -- and the sole external program are typed capabilities.
                    install = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = {
                                    path = "/usr/bin/dash",
                                    requirement = { kind = "binary", value = "dash" },
                                },
                                declared_programs = {
                                    {
                                        path = "/usr/bin/install",
                                        requirement = { kind = "binary", value = "install" },
                                    },
                                },
                                script = artifact.install_script,
                            },
                        },
                    },
                    -- Run the supplied ELF directly from the exact extracted
                    -- archive. It is not rebuilt, wrapped, patched, or
                    -- discovered through PATH.
                    check = {
                        steps = {
                            {
                                kind = "run_built",
                                program = { path = artifact.executable },
                                args = { "--self-test" },
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
        sources = { artifact.source },
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = { kind = "some", value = "Prebuilt Quartz artifact inspector" },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = artifact.runtime_inputs,
                runtime_exclude = {},
                paths = { { kind = "exe", path = artifact.installed_path } },
                conflicts = {},
            },
            {
                name = "dbginfo",
                include_in_manifest = false,
                summary = { kind = "some", value = "Quartz inspector debugging symbols" },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = {},
                runtime_exclude = {},
                paths = { { kind = "any", path = "/usr/lib/debug" } },
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
        architectures = { artifact.architecture },
    }
end

return { make = make }
