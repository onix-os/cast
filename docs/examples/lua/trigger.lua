-- Paired Lua form of docs/examples/gluon/trigger.glu (Phase L7).
return {
    name = "refresh-example",
    description = "Refresh generated example data",
    before = { kind = "none" },
    after = { kind = "none" },
    inhibitors = { kind = "none" },
    paths = {
        {
            key = "/usr/share/example/(name:*)",
            value = { handlers = { "refresh" }, kind = { kind = "none" } },
        },
    },
    handlers = {
        {
            key = "refresh",
            value = { kind = "run", command = "/usr/bin/example-refresh", args = { "$(name)" } },
        },
    },
}
