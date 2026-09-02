-- An interpreter is one coherent value: the executable used during checks,
-- the relation retained by the installed output, and the absolute shebang
-- written into that output all come from the same explicit target.
local function interpreter(target, token)
    return {
        program = {
            path = "/usr/bin/" .. target,
            requirement = { kind = "binary", value = target },
        },
        runtime = { kind = "binary", value = target },
        shebang = "/usr/bin/" .. target,
        token = token,
    }
end

return {
    bash = interpreter("bash", "@BASH@"),
    python = interpreter("python3", "@PYTHON@"),
}
