local BASH = { path = "/usr/bin/bash", requirement = { kind = "binary", value = "bash" } }
local CARGO = { path = "/usr/bin/cargo", requirement = { kind = "binary", value = "cargo" } }
local INSTALL = { path = "/usr/bin/install", requirement = { kind = "binary", value = "install" } }

local SETUP_SCRIPT = [[test -f "${CAST_SOURCE_DIR}/application/Cargo.toml"
test -f "${CAST_SOURCE_DIR}/application/Cargo.lock"
test -d "${CAST_SOURCE_DIR}/vendor"
test ! -e "${CAST_SOURCE_DIR}/application/vendor"]]

local BUILD_SCRIPT = [[export HOME="${CAST_BUILD_ROOT}/home"
export CARGO_HOME="${CAST_BUILD_ROOT}/cargo-home"
export CARGO_NET_OFFLINE=true
export CARGO_INCREMENTAL=0
cargo build \
    --manifest-path "${CAST_SOURCE_DIR}/application/Cargo.toml" \
    --target-dir "${CAST_BUILDER_DIR}/target" \
    --release --frozen --offline \
    --config 'net.offline=true' \
    --config 'source.crates-io.replace-with="declared-vendor"' \
    --config "source.declared-vendor.directory='${CAST_SOURCE_DIR}/vendor'"]]

local CHECK_SCRIPT = [[export HOME="${CAST_BUILD_ROOT}/home"
export CARGO_HOME="${CAST_BUILD_ROOT}/cargo-home"
export CARGO_NET_OFFLINE=true
export CARGO_INCREMENTAL=0
cargo test \
    --manifest-path "${CAST_SOURCE_DIR}/application/Cargo.toml" \
    --target-dir "${CAST_BUILDER_DIR}/target" \
    --frozen --offline \
    --config 'net.offline=true' \
    --config 'source.crates-io.replace-with="declared-vendor"' \
    --config "source.declared-vendor.directory='${CAST_SOURCE_DIR}/vendor'"]]

local INSTALL_SCRIPT = [[install -Dm755 "${CAST_BUILDER_DIR}/target/release/vendor-note" "${CAST_INSTALL_ROOT}${CAST_BINDIR}/vendor-note"
install -Dm644 "${CAST_SOURCE_DIR}/application/README.md" "${CAST_INSTALL_ROOT}${CAST_DATADIR}/doc/vendor-note/README.md"]]

-- This factory consumes only the two explicit source locks. Cargo receives an
-- empty private home, a disabled network policy, and one source replacement
-- pointing at the separately extracted vendor tree.
local function make(sources)
    return {
        meta = {
            pname = "vendor-note",
            version = "3.2.1",
            release = 1,
            homepage = "https://example.invalid/vendor-note",
            license = { "MIT" },
        },
        builder = {
            kind = "custom",
            spec = {
                required_tools = {
                    { kind = "binary", value = "cargo" },
                    { kind = "binary", value = "install" },
                },
                environment = {},
                phases = {
                    setup = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = BASH,
                                declared_programs = {},
                                script = SETUP_SCRIPT,
                            },
                        },
                    },
                    build = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = BASH,
                                declared_programs = { CARGO },
                                script = BUILD_SCRIPT,
                            },
                        },
                    },
                    install = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = BASH,
                                declared_programs = { INSTALL },
                                script = INSTALL_SCRIPT,
                            },
                        },
                    },
                    check = {
                        steps = {
                            {
                                kind = "shell",
                                interpreter = BASH,
                                declared_programs = { CARGO },
                                script = CHECK_SCRIPT,
                            },
                        },
                    },
                    workload = { steps = {} },
                },
                supported_hooks = {
                    setup = true,
                    build = true,
                    check = true,
                    install = true,
                    workload = true,
                },
            },
        },
        sources = { sources.application.lock, sources.vendor.lock },
        outputs = {
            {
                name = "out",
                include_in_manifest = true,
                summary = {
                    kind = "some",
                    value = "Offline-built application with an independent vendor closure",
                },
                description = { kind = "none" },
                provides_exclude = {},
                runtime_inputs = {},
                runtime_exclude = {},
                paths = {
                    { kind = "exe", path = "/usr/bin/vendor-note" },
                    { kind = "any", path = "/usr/share/doc/vendor-note/README.md" },
                },
                conflicts = {},
            },
        },
        options = {
            toolchain = "llvm",
            cspgo = false,
            samplepgo = false,
            debug = true,
            strip = true,
            networking = false,
            compressman = false,
            lastrip = true,
        },
        architectures = { "native" },
    }
end

return { make = make }
