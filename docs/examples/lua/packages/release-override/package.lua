-- The authored base package. A "base" is just a package table other recipes
-- rebuild field by field; there is no patch ADT or `override_attrs` merge.
return {
    meta = {
        pname = "override-release",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/override-release",
        license = { "MIT" },
    },
    builder = { kind = "cmake", flags = {} },
    sources = {
        {
            kind = "archive",
            url = "https://example.invalid/override-release-1.0.0.tar.xz",
            hash = "3333333333333333333333333333333333333333333333333333333333333333",
            rename = { kind = "some", value = "override-release-1.0.0.tar.xz" },
            strip_dirs = { kind = "some", value = 1 },
            unpack = true,
            unpack_dir = { kind = "some", value = "override-release-1.0.0" },
        },
    },
    architectures = { "native" },
}
