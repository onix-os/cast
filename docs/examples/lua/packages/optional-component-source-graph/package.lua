local PRIMARY_SOURCE = {
    kind = "archive",
    url = "https://example.invalid/optional-source-graph-3.0.0.tar.xz",
    hash = "1111111111111111111111111111111111111111111111111111111111111111",
    rename = { kind = "some", value = "optional-source-graph.tar.xz" },
    strip_dirs = { kind = "some", value = 1 },
    unpack = true,
    unpack_dir = { kind = "some", value = "application" },
}

local COMPONENT_SOURCE = {
    kind = "archive",
    url = "https://example.invalid/optional-source-graph-components-3.0.0.tar.xz",
    hash = "2222222222222222222222222222222222222222222222222222222222222222",
    rename = { kind = "some", value = "optional-source-graph-components.tar.xz" },
    strip_dirs = { kind = "some", value = 1 },
    unpack = true,
    unpack_dir = { kind = "some", value = "components" },
}

local ROOT_OUTPUT = {
    name = "out",
    include_in_manifest = true,
    summary = { kind = "none" },
    description = { kind = "none" },
    provides_exclude = {},
    runtime_inputs = {},
    runtime_exclude = {},
    paths = { { kind = "any", path = "*" } },
    conflicts = {},
}

local COMPONENT_OUTPUT = {
    name = "components",
    include_in_manifest = true,
    summary = { kind = "some", value = "Optional source-graph components" },
    description = { kind = "none" },
    provides_exclude = {},
    runtime_inputs = {},
    runtime_exclude = {},
    paths = { { kind = "any", path = "/usr/share/optional-source-graph/components" } },
    conflicts = {},
}

local COMPONENT_PREPARER = {
    path = "/usr/bin/component-preparer",
    requirement = { kind = "binary", value = "component-preparer" },
}

-- One typed choice drives every list below, so an enabled component adds its
-- source, tool, setup step, and output together or not at all.
local function when(condition, values)
    if condition then
        return values
    end
    return {}
end

local function append(left, right)
    local joined = {}
    for index = 1, #left do
        joined[#joined + 1] = left[index]
    end
    for index = 1, #right do
        joined[#joined + 1] = right[index]
    end
    return joined
end

local function make(features)
    return {
        meta = {
            pname = "optional-source-graph",
            version = "3.0.0",
            release = 1,
            homepage = "https://example.invalid/optional-source-graph",
            license = { "Apache-2.0" },
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
        sources = append({ PRIMARY_SOURCE }, when(features.component, { COMPONENT_SOURCE })),
        native_build_inputs = when(features.component, {
            { kind = "binary", value = "component-preparer" },
        }),
        outputs = append({ ROOT_OUTPUT }, when(features.component, { COMPONENT_OUTPUT })),
        hooks = {
            pre_setup = when(features.component, {
                {
                    kind = "run",
                    program = COMPONENT_PREPARER,
                    args = { "--source", "../components" },
                },
            }),
            post_setup = {},
            pre_build = {},
            post_build = {},
            pre_check = {},
            post_check = {},
            pre_install = {},
            post_install = {},
            pre_workload = {},
            post_workload = {},
        },
        architectures = { "native" },
    }
end

return { make = make }
