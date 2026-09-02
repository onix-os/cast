-- Every checkout has its own authored URL, revision, and materialization name.
-- No recursive fetch or implicit Git-submodule state is part of the recipe.
return {
    application = {
        kind = "git",
        url = "https://example.invalid/orbit-console.git",
        git_ref = "1111111111111111111111111111111111111111",
        clone_dir = { kind = "some", value = "application" },
    },
    syntax = {
        kind = "git",
        url = "https://example.invalid/orbit-syntax.git",
        git_ref = "2222222222222222222222222222222222222222",
        clone_dir = { kind = "some", value = "syntax-engine" },
    },
    protocol = {
        kind = "git",
        url = "https://example.invalid/orbit-wire.git",
        git_ref = "3333333333333333333333333333333333333333",
        clone_dir = { kind = "some", value = "wire-format" },
    },
}
