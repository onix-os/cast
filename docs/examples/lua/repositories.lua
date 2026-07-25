-- Paired Lua form of docs/examples/gluon/repositories.glu (Phase L7).
-- Decodes to the same repository map; the engine identity differs by design.
return {
    {
        id = "local",
        description = { kind = "none" },
        source = { kind = "direct_index", uri = "file:///var/cache/cast/local.index" },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
    {
        id = "volatile",
        description = { kind = "none" },
        source = {
            kind = "root_index",
            base_uri = "https://packages.example.invalid",
            channel = { kind = "none" },
            version = "stream/volatile",
            arch = { kind = "none" },
        },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
}
