local function make(deps)
    return {
        meta = {
            pname = "override-client",
            version = "8.2.0",
            release = 1,
            homepage = "https://example.invalid/override-client",
            license = { "MPL-2.0" },
        },
        builder = { kind = "cmake", flags = { "-DUSE_SYSTEM_LIBRARIES=ON" } },
        sources = {
            {
                kind = "archive",
                url = "https://example.invalid/override-client-8.2.0.tar.xz",
                hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        build_inputs = { deps.compression, deps.tls },
        architectures = {},
    }
end

return { make = make }
