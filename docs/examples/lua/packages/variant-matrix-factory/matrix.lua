local storages = {
    sqlite = {
        flag = "-DSTORAGE=sqlite",
        build_inputs = { { kind = "pkg_config", value = "sqlite3" } },
        runtime_inputs = { { kind = "soname", value = "libsqlite3.so.0" } },
    },
    postgresql = {
        flag = "-DSTORAGE=postgresql",
        build_inputs = { { kind = "pkg_config", value = "libpq" } },
        runtime_inputs = { { kind = "soname", value = "libpq.so.5" } },
    },
}

local telemetries = {
    telemetry_off = {
        flag = "-DTELEMETRY=off",
        build_inputs = {},
        runtime_inputs = {},
    },
    opentelemetry = {
        flag = "-DTELEMETRY=opentelemetry",
        build_inputs = { { kind = "pkg_config", value = "opentelemetry-c" } },
        runtime_inputs = { { kind = "soname", value = "libopentelemetry.so.1" } },
    },
}

local function append(left, right)
    local joined = {}
    for index = 1, #left do
        joined[#joined + 1] = left[index]
    end
    for index = 1, #right do
        joined[#joined + 1] = right[index]
    end
    return joined
end

-- Both axes are closed, so one selection yields one exhaustive cell.
local function select(storage, telemetry)
    local selected_storage = storages[storage]
    local selected_telemetry = telemetries[telemetry]
    return {
        flags = { selected_storage.flag, selected_telemetry.flag },
        build_inputs = append(selected_storage.build_inputs, selected_telemetry.build_inputs),
        runtime_inputs = append(selected_storage.runtime_inputs, selected_telemetry.runtime_inputs),
    }
end

return {
    sqlite = "sqlite",
    postgresql = "postgresql",
    telemetry_off = "telemetry_off",
    opentelemetry = "opentelemetry",
    select = select,
}
