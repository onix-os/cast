-- Paired Lua form of docs/examples/gluon/boot-topology-distinct-xbootldr.glu (Phase L7).
return {
    esp = { partuuid = "11111111-2222-3333-4444-555555555555", mount_point = "/efi" },
    boot = {
        kind = "distinct_xbootldr",
        xbootldr = { partuuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", mount_point = "/boot" },
    },
}
