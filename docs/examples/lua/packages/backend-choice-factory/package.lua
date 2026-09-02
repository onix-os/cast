-- A closed set of TLS backends. Each name selects one configure flag, one
-- development dependency and one runtime soname, so the three cannot be mixed.
local backends = {
    openssl = {
        flag = "-DTLS_BACKEND=openssl",
        development = { kind = "pkg_config", value = "openssl" },
        runtime = { kind = "soname", value = "libssl.so.3" },
    },
    libressl = {
        flag = "-DTLS_BACKEND=libressl",
        development = { kind = "pkg_config", value = "libressl" },
        runtime = { kind = "soname", value = "libtls.so.28" },
    },
    rustls = {
        flag = "-DTLS_BACKEND=rustls",
        development = { kind = "pkg_config", value = "rustls-ffi" },
        runtime = { kind = "soname", value = "librustls_ffi.so.0" },
    },
}

local function make(backend)
    local selected = backends[backend]

    return {
        meta = {
            pname = "typed-backend-client",
            version = "4.0.0",
            release = 1,
            homepage = "https://example.invalid/typed-backend-client",
            license = { "MPL-2.0" },
        },
        builder = { kind = "cmake", flags = { selected.flag } },
        sources = {
            {
                kind = "archive",
                url = "https://example.invalid/typed-backend-client-4.0.0.tar.xz",
                hash = "456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123",
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        build_inputs = { selected.development },
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = { kind = "none" },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = { selected.runtime },
                runtime_exclude = {},
                paths = { { kind = "any", path = "*" } },
                conflicts = {},
            },
        },
        architectures = { "native" },
    }
end

return { make = make }
