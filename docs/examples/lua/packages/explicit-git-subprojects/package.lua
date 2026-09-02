-- The factory receives the closed source graph and links the independently
-- locked trees into the layout the bundled CMake build expects.
local function make(repositories)
    return {
        meta = {
            pname = "orbit-console",
            version = "2.4.0",
            release = 1,
            homepage = "https://example.invalid/orbit-console",
            license = { "BSD-2-Clause" },
        },
        builder = {
            kind = "cmake",
            flags = {
                "-Sapplication",
                "-DFETCHCONTENT_FULLY_DISCONNECTED=ON",
                "-DUSE_BUNDLED_SUBPROJECTS=ON",
            },
        },
        sources = {
            repositories.application,
            repositories.syntax,
            repositories.protocol,
        },
        hooks = {
            pre_setup = {
                {
                    kind = "run",
                    program = {
                        path = "/usr/bin/mkdir",
                        requirement = { kind = "binary", value = "mkdir" },
                    },
                    args = { "-p", "application/subprojects" },
                },
                {
                    kind = "run",
                    program = {
                        path = "/usr/bin/ln",
                        requirement = { kind = "binary", value = "ln" },
                    },
                    args = { "-s", "../../syntax-engine", "application/subprojects/syntax-engine" },
                },
                {
                    kind = "run",
                    program = {
                        path = "/usr/bin/ln",
                        requirement = { kind = "binary", value = "ln" },
                    },
                    args = { "-s", "../../wire-format", "application/subprojects/wire-format" },
                },
            },
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
