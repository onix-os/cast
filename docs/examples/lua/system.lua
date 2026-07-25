-- Paired Lua form of docs/examples/gluon/system.glu (Phase L7).
return {
    disable_warning = false,
    repositories = {
        {
            id = "local",
            description = { kind = "none" },
            source = { kind = "direct_index", uri = "file:///var/cache/cast/local.index" },
            priority = { kind = "none" },
            enabled = { kind = "none" },
        },
    },
    packages = { "system-base", "editor" },
}
