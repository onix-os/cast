-- Application and dependency closure are separate authored identities. Each
-- record retains the URL, digest, extraction destination, and resulting source
-- lock as independent values, so either archive can be updated without deriving
-- its identity from the other.
local function archive_lock(identity)
    return {
        url = identity.url,
        digest = identity.digest,
        unpack_dir = identity.unpack_dir,
        lock = {
            kind = "archive",
            url = identity.url,
            hash = identity.digest,
            rename = { kind = "some", value = identity.archive_name },
            strip_dirs = { kind = "some", value = 1 },
            unpack = true,
            unpack_dir = { kind = "some", value = identity.unpack_dir },
        },
    }
end

local application = archive_lock({
    url = "https://example.invalid/vendor-note-3.2.1.tar.zst",
    digest = "111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000",
    archive_name = "vendor-note-3.2.1.tar.zst",
    unpack_dir = "application",
})

local vendor = archive_lock({
    url = "https://example.invalid/vendor-note-cargo-vendor-2026-07-21.tar.zst",
    digest = "0000ffffeeeeddddccccbbbbaaaa999988887777666655554444333322221111",
    archive_name = "vendor-note-cargo-vendor-2026-07-21.tar.zst",
    unpack_dir = "vendor",
})

return {
    archive_lock = archive_lock,
    application = application,
    vendor = vendor,
}
