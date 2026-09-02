-- Roles are closed values, not ambient modules. Selecting one role therefore
-- selects one complete userspace closure without merging hidden defaults.
local roles = {
    workstation = {
        pname = "userspace-workstation",
        summary = "Declarative graphical workstation role",
        packages = {
            { kind = "package", value = { name = "foot" } },
            { kind = "package", value = { name = "helix" } },
            { kind = "package", value = { name = "wayland" } },
        },
    },
    server = {
        pname = "userspace-server",
        summary = "Declarative service host role",
        packages = {
            { kind = "package", value = { name = "openssh" } },
            { kind = "package", value = { name = "podman" } },
            { kind = "package", value = { name = "systemd" } },
        },
    },
    builder = {
        pname = "userspace-builder",
        summary = "Declarative package builder role",
        packages = {
            { kind = "package", value = { name = "clang" } },
            { kind = "package", value = { name = "cmake" } },
            { kind = "package", value = { name = "ninja" } },
        },
    },
}

local function select(role)
    return roles[role]
end

return {
    workstation = "workstation",
    server = "server",
    builder = "builder",
    select = select,
}
