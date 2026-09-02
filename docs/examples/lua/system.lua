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
