local function client(scope)
    return {
        meta = {
            pname = "scoped-client",
            version = "1.2.0",
            release = 1,
            homepage = "https://example.invalid/scoped-client",
            license = { "BSD-2-Clause" },
        },
        builder = { kind = "cmake", flags = { "-DBUILD_SERVER=OFF" } },
        sources = {
            {
                kind = "archive",
                url = "https://example.invalid/scoped-client-1.2.0.tar.xz",
                hash = "5555555555555555555555555555555555555555555555555555555555555555",
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        build_inputs = { scope.compression, scope.crypto },
        architectures = { "native" },
    }
end

local function server(scope)
    return {
        meta = {
            pname = "scoped-server",
            version = "1.2.0",
            release = 1,
            homepage = "https://example.invalid/scoped-server",
            license = { "BSD-2-Clause" },
        },
        builder = { kind = "cmake", flags = { "-DBUILD_SERVER=ON" } },
        sources = {
            {
                kind = "archive",
                url = "https://example.invalid/scoped-server-1.2.0.tar.xz",
                hash = "6666666666666666666666666666666666666666666666666666666666666666",
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        native_build_inputs = { scope.documentation },
        build_inputs = { scope.database },
        architectures = { "native" },
    }
end

return {
    client = client,
    server = server,
}
