local function make(packages)
    return {
        meta = {
            pname = "observability-workstation",
            version = "1",
            release = 1,
            homepage = "https://example.invalid/observability-workstation",
            license = { "CC0-1.0" },
        },
        builder = {
            kind = "custom",
            spec = {
                required_tools = {},
                environment = {},
                phases = {
                    setup = { steps = {} },
                    build = { steps = {} },
                    install = { steps = {} },
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
                summary = { kind = "some", value = "Explicitly extended userspace package set" },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = {
                    packages.shell,
                    packages.core,
                    packages.metrics,
                    packages.logs,
                },
                runtime_exclude = {},
                paths = {},
                conflicts = {},
            },
        },
        architectures = { "native" },
    }
end

return { make = make }
