local family = cast.import("family.lua")

local function make(release, member)
    local selected = family.select(member)

    return {
        meta = {
            pname = selected.pname,
            version = release.version,
            release = release.package_release,
            homepage = "https://example.invalid/relay-family",
            license = { "Apache-2.0" },
        },
        builder = { kind = "cmake", flags = { selected.flag } },
        sources = {
            {
                kind = "archive",
                url = release.source_url,
                hash = release.source_sha256,
                rename = { kind = "none" },
                strip_dirs = { kind = "none" },
                unpack = true,
                unpack_dir = { kind = "none" },
            },
        },
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = { kind = "some", value = selected.summary },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = selected.runtime_inputs,
                runtime_exclude = {},
                paths = selected.paths,
                conflicts = {},
            },
        },
        architectures = { "x86_64", "aarch64" },
    }
end

return { make = make }
