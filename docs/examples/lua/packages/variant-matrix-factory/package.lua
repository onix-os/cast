local matrix = cast.import("matrix.lua")

local function make(storage, telemetry)
    local selected = matrix.select(storage, telemetry)

    return {
        meta = {
            pname = "matrix-service",
            version = "2.0.0",
            release = 1,
            homepage = "https://example.invalid/matrix-service",
            license = { "MPL-2.0" },
        },
        builder = { kind = "cmake", flags = selected.flags },
        sources = {
            {
                kind = "archive",
                url = "https://example.invalid/matrix-service-2.0.0.tar.xz",
                hash = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        build_inputs = selected.build_inputs,
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = { kind = "none" },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = selected.runtime_inputs,
                runtime_exclude = {},
                paths = { { kind = "any", path = "*" } },
                conflicts = {},
            },
        },
        architectures = { "native" },
    }
end

return { make = make }
