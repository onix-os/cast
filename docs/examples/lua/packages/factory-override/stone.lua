-- This is analogous to overriding a Nix function argument before evaluating
-- the package: the factory receives an alternate TLS capability.
local factory = cast.import("package.lua")
local inputs = cast.import("inputs.lua")

local package = factory.make({
    compression = inputs.compression,
    tls = { kind = "pkg_config", value = "libressl" },
})

local function local_output(output)
    return {
        kind = "output",
        value = { package = { name = "override-client" }, output = output },
    }
end

-- The shared Rust default output set, reproduced here because appending the
-- "tools" output requires an explicit output list: the authored ABI offers
-- either `outputs` omitted (opaque, Rust-filled) or a complete authored list,
-- not a way to extend the default from a recipe.
local outputs = {
    {
        name = "out",
        include_in_manifest = true,
        summary = { kind = "none" },
        description = { kind = "none" },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = { { kind = "any", path = "*" } },
        conflicts = {},
    },
    {
        name = "docs",
        include_in_manifest = true,
        summary = { kind = "some", value = "Documentation for override-client" },
        description = {
            kind = "some",
            value = "Documentation files for the override-client package",
        },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = { { kind = "any", path = "/usr/share/gtk-doc" } },
        conflicts = {},
    },
    {
        name = "devel",
        include_in_manifest = true,
        summary = { kind = "some", value = "Development files for override-client" },
        description = {
            kind = "some",
            value = "Install this package if you intend to build software against\nthe override-client package.",
        },
        provides_exclude = {},
        runtime_inputs = { local_output("out") },
        runtime_exclude = {},
        paths = {
            { kind = "any", path = "/usr/include" },
            { kind = "any", path = "/usr/lib/*.a" },
            { kind = "any", path = "/usr/lib/cmake" },
            { kind = "any", path = "/usr/lib/lib*.so" },
            { kind = "any", path = "/usr/lib/pkgconfig" },
            { kind = "any", path = "/usr/share/aclocal" },
            { kind = "any", path = "/usr/share/cmake" },
            { kind = "any", path = "/usr/share/man/man2" },
            { kind = "any", path = "/usr/share/man/man3" },
            { kind = "any", path = "/usr/share/man/man9" },
            { kind = "any", path = "/usr/share/pkgconfig" },
            { kind = "any", path = "/usr/share/gir-1.0/*.gir" },
            { kind = "any", path = "/usr/share/vala/vapi/*.deps" },
            { kind = "any", path = "/usr/share/vala/vapi/*.vapi" },
            { kind = "any", path = "/usr/lib/*.prl" },
            { kind = "any", path = "/usr/lib/metatypes" },
            { kind = "any", path = "/usr/lib/qt*/metatypes/qt*.json" },
            { kind = "any", path = "/usr/lib/qt*/mkspecs" },
            { kind = "any", path = "/usr/lib/qt*/modules/*.json" },
            { kind = "any", path = "/usr/lib/qt*/sbom" },
            { kind = "any", path = "/usr/lib/qt*/plugins/designer/*.so" },
            { kind = "any", path = "/usr/share/doc/qt5/*.qch" },
            { kind = "any", path = "/usr/share/doc/qt5/*.tags" },
            { kind = "any", path = "/usr/share/doc/qt6/*.qch" },
            { kind = "any", path = "/usr/share/doc/qt6/*.tags" },
        },
        conflicts = {},
    },
    {
        name = "dbginfo",
        include_in_manifest = false,
        summary = { kind = "some", value = "Debugging symbols for override-client" },
        description = {
            kind = "some",
            value = "Install this package if you need debugging information + symbols\nfor the override-client package.",
        },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = { { kind = "any", path = "/usr/lib/debug" } },
        conflicts = {},
    },
    {
        name = "libs",
        include_in_manifest = true,
        summary = { kind = "some", value = "Library files for override-client" },
        description = {
            kind = "some",
            value = "Library files for override-client, typically pulled in as a dependency of another package.",
        },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = {},
        conflicts = {},
    },
    {
        name = "32bit",
        include_in_manifest = true,
        summary = {
            kind = "some",
            value = "Provides 32-bit runtime libraries for override-client",
        },
        description = {
            kind = "some",
            value = "Install this package if you need the 32-bit versions of the\noverride-client package libraries.",
        },
        provides_exclude = {},
        runtime_inputs = { local_output("out") },
        runtime_exclude = {},
        paths = {
            { kind = "any", path = "/usr/lib32" },
            { kind = "any", path = "/usr/lib32/lib*.so.*" },
        },
        conflicts = {},
    },
    {
        name = "32bit-devel",
        include_in_manifest = true,
        summary = {
            kind = "some",
            value = "Provides development files for override-client-32bit",
        },
        description = {
            kind = "some",
            value = "Install this package if you need to build software against\nthe 32-bit version of override-client, override-client-32bit.",
        },
        provides_exclude = {},
        runtime_inputs = { local_output("32bit"), local_output("devel") },
        runtime_exclude = {},
        paths = {
            { kind = "any", path = "/usr/lib32/*.a" },
            { kind = "any", path = "/usr/lib32/cmake" },
            { kind = "any", path = "/usr/lib32/lib*.so" },
            { kind = "any", path = "/usr/lib32/pkgconfig" },
        },
        conflicts = {},
    },
    {
        name = "32bit-dbginfo",
        include_in_manifest = false,
        summary = { kind = "some", value = "Debugging symbols for override-client-32bit" },
        description = {
            kind = "some",
            value = "Install this package if you need debugging information + symbols\nfor the override-client-32bit package.",
        },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = { { kind = "any", path = "/usr/lib32/debug" } },
        conflicts = {},
    },
    {
        name = "demos",
        include_in_manifest = true,
        summary = { kind = "some", value = "Example files for override-client" },
        description = { kind = "some", value = "Example files for the override-client package" },
        provides_exclude = {},
        runtime_inputs = {},
        runtime_exclude = {},
        paths = { { kind = "any", path = "/usr/lib/qt*/examples" } },
        conflicts = {},
    },
    {
        name = "tools",
        include_in_manifest = true,
        summary = { kind = "some", value = "Optional diagnostics" },
        description = { kind = "none" },
        provides_exclude = {},
        runtime_inputs = { local_output("out") },
        runtime_exclude = {},
        paths = { { kind = "exe", path = "/usr/bin/override-client-diagnose" } },
        conflicts = {},
    },
}

-- Keep/replace/append semantics stay explicit: the recipe restates the fields
-- it replaces and takes the rest from the evaluated base package by name.
return {
    meta = package.meta,
    builder = package.builder,
    sources = package.sources,
    build_inputs = package.build_inputs,
    outputs = outputs,
    architectures = { "x86_64" },
}
