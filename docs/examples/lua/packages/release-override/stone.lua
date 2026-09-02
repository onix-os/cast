-- Metadata and the complete source list move together: the recipe restates the
-- fields it replaces and takes the rest from the base package by name. Nothing
-- is merged implicitly, so no stale source can survive the release bump.
local base = cast.import("package.lua")

return {
    meta = {
        pname = "override-release",
        version = "2.1.0",
        release = 4,
        homepage = "https://example.invalid/override-release",
        license = { "MIT" },
    },
    sources = {
        {
            kind = "archive",
            url = "https://example.invalid/override-release-2.1.0.tar.xz",
            hash = "4444444444444444444444444444444444444444444444444444444444444444",
            rename = { kind = "some", value = "override-release-2.1.0.tar.xz" },
            strip_dirs = { kind = "some", value = 1 },
            unpack = true,
            unpack_dir = { kind = "some", value = "override-release-2.1.0" },
        },
    },
    builder = base.builder,
    architectures = base.architectures,
}
