local roles = cast.import("roles.lua")

local function make(role)
    local selected = roles.select(role)

    return {
        meta = {
            pname = selected.pname,
            version = "1",
            release = 1,
            homepage = "https://example.invalid/userspace-roles",
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
                summary = { kind = "some", value = selected.summary },
                description = {
                    kind = "some",
                    value = "One explicit, typed userspace role with no imperative activation phase.",
                },
                provides_exclude = {},
                runtime_inputs = selected.packages,
                runtime_exclude = {},
                paths = {},
                conflicts = {},
            },
        },
        architectures = { "native" },
    }
end

return { make = make }
