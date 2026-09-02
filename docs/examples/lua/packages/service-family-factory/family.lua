local members = {
    daemon = {
        pname = "relay-daemon",
        flag = "-DBUILD_MEMBER=daemon",
        summary = "Relay background service",
        runtime_inputs = {
            { kind = "package", value = { name = "systemd" } },
            { kind = "soname", value = "libssl.so.3" },
        },
        paths = {
            { kind = "exe", path = "/usr/sbin/relay-daemon" },
            { kind = "any", path = "/usr/lib/systemd/system/relay-daemon.service" },
            { kind = "any", path = "/usr/share/defaults/relay-daemon" },
        },
    },
    client = {
        pname = "relay-client",
        flag = "-DBUILD_MEMBER=client",
        summary = "Relay command-line client",
        runtime_inputs = {
            { kind = "soname", value = "libssl.so.3" },
            { kind = "package", value = { name = "ca-certificates" } },
        },
        paths = {
            { kind = "exe", path = "/usr/bin/relayctl" },
            { kind = "any", path = "/usr/share/man/man1/relayctl.1" },
        },
    },
    integration = {
        pname = "relay-integration",
        flag = "-DBUILD_MEMBER=integration",
        summary = "Relay declarative integration assets",
        runtime_inputs = {
            { kind = "package", value = { name = "relay-daemon" } },
            { kind = "package", value = { name = "systemd" } },
        },
        paths = {
            { kind = "any", path = "/usr/lib/sysusers.d/relay.conf" },
            { kind = "any", path = "/usr/lib/tmpfiles.d/relay.conf" },
        },
    },
}

local function select(member)
    return members[member]
end

return {
    daemon = "daemon",
    client = "client",
    integration = "integration",
    select = select,
}
