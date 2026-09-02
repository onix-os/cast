-- One prebuilt ELF release is a closed value: target, archive identity,
-- extraction layout, executable path, and runtime ABI requirements travel
-- together. The package factory never consults uname, PATH, or the host ELF.
return {
    architecture = "x86_64",
    executable = "bin/quartz-inspector",
    installed_path = "/usr/bin/quartz-inspector",
    install_script = [[/usr/bin/install -Dm755 bin/quartz-inspector "${CAST_INSTALL_ROOT}${CAST_BINDIR}/quartz-inspector"]],
    runtime_inputs = {
        { kind = "interpreter", value = "/usr/lib/ld-linux-x86-64.so.2(x86_64)" },
        { kind = "soname", value = "libc.so.6(x86_64)" },
    },
    source = {
        kind = "archive",
        url = "https://example.invalid/quartz-inspector/releases/3.4.1/quartz-inspector-3.4.1-x86_64.tar.xz",
        hash = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        rename = { kind = "some", value = "quartz-inspector-3.4.1-x86_64.tar.xz" },
        strip_dirs = { kind = "some", value = 1 },
        unpack = true,
        unpack_dir = { kind = "some", value = "quartz-inspector" },
    },
}
