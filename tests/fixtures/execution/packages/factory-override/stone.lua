-- The recipe injects its typed values into a locally imported factory, so the
-- frozen provenance must bind the exact factory module it evaluated.
local make = cast.import("factory.lua")

return make({
    meta = {
        pname = "cast-factory-override-fixture",
        version = "1.0.0",
        release = 1,
        homepage = "https://fixtures.invalid/cast-factory-override-fixture",
        license = { "MPL-2.0" },
    },
    builder = {
        kind = "cmake",
        flags = { "-DCAST_FACTORY_VARIANT=stone-override" },
    },
    sources = {
        {
            kind = "archive",
            url = "https://fixtures.invalid/sources/cast-factory-override-fixture-1.0.0.tar",
            hash = "cc10c246cfaeeb644e2a458794262fd68ade2c0d76c0874a932a787ad088e001",
            rename = { kind = "some", value = "cast-factory-override-fixture.tar" },
            strip_dirs = { kind = "some", value = 1 },
            unpack = true,
            unpack_dir = { kind = "some", value = "cast-factory-override-fixture" },
        },
    },
    architectures = { "x86_64" },
})
