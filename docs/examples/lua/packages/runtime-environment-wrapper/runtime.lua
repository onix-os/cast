local function output_resource(package_name, output_name, path)
    return {
        path = path,
        runtime_input = {
            kind = "output",
            value = { package = { name = package_name }, output = output_name },
        },
    }
end

-- Every path is paired with the exact typed provider that supplies it. The
-- package-specific wrapper consumes this closed record; it never discovers a
-- target or resource through the build host, PATH, or an inherited variable.
return {
    target = {
        program = {
            path = "/usr/libexec/graphite-renderer/graphite-renderer",
            requirement = {
                kind = "output",
                value = { package = { name = "graphite-renderer" }, output = "runtime" },
            },
        },
        runtime_input = {
            kind = "output",
            value = { package = { name = "graphite-renderer" }, output = "runtime" },
        },
    },
    plugins = output_resource("graphite-codecs", "plugins", "/usr/lib/graphite-renderer/plugins"),
    data = output_resource("graphite-assets", "data", "/usr/share/graphite-renderer"),
    certificates = output_resource(
        "system-trust",
        "certificates",
        "/usr/share/system-trust/ca-bundle.pem"
    ),
    schemas = output_resource(
        "desktop-schema-registry",
        "schemas",
        "/usr/share/glib-2.0/schemas"
    ),
    interpreter = {
        program = {
            path = "/usr/bin/bash",
            requirement = { kind = "binary", value = "bash" },
        },
        runtime_input = { kind = "binary", value = "bash" },
    },
}
