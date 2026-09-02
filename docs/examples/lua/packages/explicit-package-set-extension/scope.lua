local base = {
    shell = { kind = "package", value = { name = "bash" } },
    core = { kind = "package", value = { name = "coreutils" } },
}

-- Extension is explicit, non-recursive, and receives the complete base set.
-- There is no global package universe or reflection-based callPackage layer.
local function with_observability(packages)
    return {
        shell = packages.shell,
        core = packages.core,
        metrics = { kind = "package", value = { name = "prometheus-node-exporter" } },
        logs = { kind = "package", value = { name = "vector" } },
    }
end

return {
    base = base,
    with_observability = with_observability,
}
