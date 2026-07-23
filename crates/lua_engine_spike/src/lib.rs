//! Phase L0 Lua engine spike.
//!
//! A throwaway proof that Lua 5.4 (via vendored `mlua`) can satisfy Cast's
//! shared evaluator policy: an empty-by-default sandbox, host-latched resource
//! limits, a deadline-driven interrupt, and deterministic pure evaluation. It
//! contains no production declaration adapter and registers no `.lua`
//! discovery.
//!
//! `mlua`'s `StdLib::NONE` still leaves the Lua base library (`load`, `dofile`,
//! `setmetatable`, `print`, …) reachable through the shared global table, so an
//! empty environment is established per evaluation by running each authored
//! chunk with its own controlled `_ENV` table that has no metatable and no
//! reference to `_G`. This is the manual Lua 5.4 sandbox the runtime survey
//! anticipated.

use std::time::{Duration, Instant};

use mlua::{FromLuaMulti, HookTriggers, Lua, LuaOptions, StdLib, VmState};

pub mod imports;

/// Construct the minimal Lua 5.4 runtime with no standard library loaded.
pub fn empty_runtime() -> mlua::Result<Lua> {
    Lua::new_with(StdLib::NONE, LuaOptions::default())
}

/// Evaluate `chunk` in a fresh empty environment under a monotonic time budget.
///
/// A debug hook fires every N virtual instructions and returns an error once
/// the deadline is exceeded, so an unbounded loop is interrupted by the host
/// rather than running forever. The hook is removed afterward so the latch is
/// scoped to this evaluation. This models the shared core's monotonic deadline
/// driving the engine interrupt.
pub fn eval_with_deadline<T: FromLuaMulti>(
    lua: &Lua,
    chunk: &str,
    budget: Duration,
) -> mlua::Result<T> {
    let start = Instant::now();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1024),
        move |_lua, _debug| {
            if start.elapsed() >= budget {
                Err(mlua::Error::runtime(
                    "cast: evaluation deadline exceeded",
                ))
            } else {
                Ok(VmState::Continue)
            }
        },
    );
    let environment = lua.create_table()?;
    let result = lua.load(chunk).set_environment(environment).eval();
    lua.remove_hook();
    result
}

/// Evaluate `chunk` in a fresh, empty environment table.
///
/// The chunk's `_ENV` upvalue is a table with no entries and no metatable, so
/// no global — including the always-present base library — is reachable from
/// authored code. Only values the caller explicitly inserts into the returned
/// environment would be visible.
pub fn eval_sandboxed<T: FromLuaMulti>(lua: &Lua, chunk: &str) -> mlua::Result<T> {
    let environment = lua.create_table()?;
    lua.load(chunk).set_environment(environment).eval()
}

/// Well-known globals that must never be reachable from a sandboxed chunk.
pub const FORBIDDEN_GLOBALS: &[&str] = &[
    "os",
    "io",
    "package",
    "require",
    "dofile",
    "loadfile",
    "load",
    "loadstring",
    "debug",
    "print",
    "collectgarbage",
    "coroutine",
    "pairs",
    "next",
    "ipairs",
    "setmetatable",
    "getmetatable",
    "rawset",
    "rawget",
    "rawequal",
    "string",
    "table",
    "math",
    "pcall",
    "xpcall",
    "error",
    "assert",
    "select",
    "type",
    "tostring",
    "tonumber",
    // `_G` resolves through the empty environment to nil; `_ENV` itself is the
    // chunk's own empty environment table and is intentionally not listed —
    // access to an empty per-evaluation `_ENV` grants no capability.
    "_G",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxed_chunk_evaluates_a_pure_expression() {
        let lua = empty_runtime().unwrap();
        let value: i64 = eval_sandboxed(&lua, "return 1 + 41").unwrap();
        assert_eq!(value, 42);
    }

    #[test]
    fn sandboxed_chunk_exposes_no_forbidden_globals() {
        let lua = empty_runtime().unwrap();
        for global in FORBIDDEN_GLOBALS {
            let present: bool = eval_sandboxed(&lua, &format!("return ({global}) ~= nil"))
                .unwrap_or_else(|error| {
                    panic!("probe for `{global}` failed to evaluate: {error}")
                });
            assert!(
                !present,
                "forbidden global `{global}` is reachable from a sandboxed chunk"
            );
        }
    }

    #[test]
    fn sandboxed_chunks_cannot_reach_the_real_global_table() {
        let lua = empty_runtime().unwrap();
        // Even reflection helpers are absent; there is no path from `_ENV` back
        // to the base library.
        let leaked: bool = eval_sandboxed(
            &lua,
            "return (getmetatable ~= nil) or (rawget ~= nil) or (load ~= nil)",
        )
        .unwrap();
        assert!(!leaked, "a sandboxed chunk found a path to base-library globals");
    }

    #[test]
    fn deadline_interrupts_an_unbounded_loop_and_the_host_survives() {
        let lua = empty_runtime().unwrap();
        let result: mlua::Result<()> =
            eval_with_deadline(&lua, "while true do end", Duration::from_millis(50));
        assert!(result.is_err(), "an infinite loop must be interrupted by the deadline");

        // The latch is host-side: the runtime remains usable afterward.
        let recovered: i64 = eval_sandboxed(&lua, "return 9").unwrap();
        assert_eq!(recovered, 9);
    }

    #[test]
    fn memory_exhaustion_is_host_limited_and_recoverable() {
        let lua = empty_runtime().unwrap();
        lua.set_memory_limit(4 * 1024 * 1024).unwrap();
        let environment = lua.create_table().unwrap();
        let result: mlua::Result<()> = lua
            .load("local t = {} local i = 1 while true do t[i] = {i, i, i} i = i + 1 end")
            .set_environment(environment)
            .eval();
        assert!(result.is_err(), "unbounded allocation must hit the memory limit");

        let recovered: i64 = eval_sandboxed(&lua, "return 7").unwrap();
        assert_eq!(recovered, 7);
    }

    #[test]
    fn deep_recursion_is_contained_not_a_host_crash() {
        let lua = empty_runtime().unwrap();
        // Adversarial recursion (which the Cast profile forbids) must fault as a
        // caught Lua error, never a host stack overflow. The `1 +` keeps this a
        // non-tail call so the Lua stack actually grows and overflows; a deadline
        // is a backstop so a mis-typed tail call can never hang the host.
        let result: mlua::Result<()> = eval_with_deadline(
            &lua,
            "local function f(n) return 1 + f(n + 1) end return f(0)",
            Duration::from_secs(5),
        );
        assert!(result.is_err(), "unbounded recursion must fault as a caught error");

        let recovered: i64 = eval_sandboxed(&lua, "return 5").unwrap();
        assert_eq!(recovered, 5);
    }
}
