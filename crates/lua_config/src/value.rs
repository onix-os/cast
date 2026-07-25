//! Bounded, cycle-safe validation of a final Lua value before schema decoding.
//!
//! Only the authored root's final value crosses into Rust. Per the plan it may
//! contain booleans, UTF-8 strings, integers, contiguous one-based arrays, and
//! string-keyed records (which model tagged variants/options/patches). Floats,
//! `NaN`/infinity, functions, threads, userdata, metatables, cycles, sparse or
//! mixed-key tables, and invalid UTF-8 are rejected here, before any
//! domain-specific decoding sees the value. Depth, node count, table entries,
//! and per-string bytes are bounded so a hostile value cannot exhaust the host.

use declarative_config::{Diagnostic, DiagnosticCategory};
use mlua::{Table, Value};

/// Bounds on the shape of a decoded value tree. These are part of the shared
/// evaluator policy once the production limits are finalized; the defaults here
/// are conservative and deterministic.
#[derive(Debug, Clone, Copy)]
pub struct ValueLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_table_entries: usize,
    pub max_string_bytes: usize,
}

impl Default for ValueLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_nodes: 100_000,
            max_table_entries: 10_000,
            max_string_bytes: 256 * 1024,
        }
    }
}

fn reject(source_name: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCategory::Type,
        None,
        Some(source_name.to_owned()),
        None,
        message,
    )
}

/// Validate `value` against [`ValueLimits`], returning `Ok` only for an
/// acceptable, acyclic, bounded declaration value tree.
pub fn validate_value_tree(
    value: &Value,
    source_name: &str,
    limits: &ValueLimits,
) -> Result<(), Diagnostic> {
    let mut walker = Walker {
        source_name,
        limits,
        nodes: 0,
        active: Vec::new(),
    };
    walker.walk(value, 0)
}

struct Walker<'a> {
    source_name: &'a str,
    limits: &'a ValueLimits,
    nodes: usize,
    active: Vec<*const std::ffi::c_void>,
}

impl Walker<'_> {
    fn walk(&mut self, value: &Value, depth: usize) -> Result<(), Diagnostic> {
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            return Err(reject(self.source_name, "value tree exceeds the node limit"));
        }
        if depth > self.limits.max_depth {
            return Err(reject(self.source_name, "value tree exceeds the depth limit"));
        }
        match value {
            Value::Boolean(_) | Value::Integer(_) => Ok(()),
            Value::String(text) => match text.to_str() {
                Ok(text) if text.as_bytes().len() <= self.limits.max_string_bytes => Ok(()),
                Ok(_) => Err(reject(self.source_name, "string exceeds the byte limit")),
                Err(_) => Err(reject(self.source_name, "string is not valid UTF-8")),
            },
            Value::Number(_) => {
                Err(reject(self.source_name, "float values are not allowed; use integers"))
            }
            Value::Nil => Err(reject(self.source_name, "nil is never a value; use an explicit tag")),
            Value::Table(table) => self.walk_table(table, depth),
            Value::Function(_) => Err(reject(self.source_name, "functions may not appear in the value tree")),
            Value::Thread(_) => Err(reject(self.source_name, "threads may not appear in the value tree")),
            Value::UserData(_) | Value::LightUserData(_) => {
                Err(reject(self.source_name, "userdata may not appear in the value tree"))
            }
            _ => Err(reject(self.source_name, "unsupported value kind in the value tree")),
        }
    }

    fn walk_table(&mut self, table: &Table, depth: usize) -> Result<(), Diagnostic> {
        if table.metatable().is_some() {
            return Err(reject(self.source_name, "tables in the value tree may not have metatables"));
        }
        let pointer = table.to_pointer();
        if self.active.contains(&pointer) {
            return Err(reject(self.source_name, "the value tree contains a cycle"));
        }
        self.active.push(pointer);

        let mut integer_keys = 0usize;
        let mut string_keys = 0usize;
        let mut entries = 0usize;
        for pair in table.clone().pairs::<Value, Value>() {
            let (key, value) = pair.map_err(|error| {
                reject(self.source_name, format!("table iteration failed: {error}"))
            })?;
            entries += 1;
            if entries > self.limits.max_table_entries {
                return Err(reject(self.source_name, "table exceeds the entry limit"));
            }
            match &key {
                Value::Integer(_) => integer_keys += 1,
                Value::String(text) => {
                    if text.to_str().is_err() {
                        return Err(reject(self.source_name, "table key is not valid UTF-8"));
                    }
                    string_keys += 1;
                }
                _ => {
                    return Err(reject(
                        self.source_name,
                        "table keys must be one-based integers or UTF-8 strings",
                    ));
                }
            }
            self.walk(&value, depth + 1)?;
        }

        if integer_keys > 0 && string_keys > 0 {
            return Err(reject(self.source_name, "mixed integer/string table keys are not allowed"));
        }
        if integer_keys > 0 {
            // Require a contiguous one-based array (no sparse holes).
            let length = table.raw_len() as usize;
            if length != integer_keys {
                return Err(reject(self.source_name, "arrays must be contiguous and one-based"));
            }
        }

        self.active.pop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mlua::{Lua, LuaOptions, StdLib};

    use super::*;

    fn eval(lua: &Lua, source: &str) -> Value {
        let environment = lua.create_table().unwrap();
        lua.load(source).set_environment(environment).eval().unwrap()
    }

    fn check(source: &str) -> Result<(), Diagnostic> {
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        let value = eval(&lua, source);
        validate_value_tree(&value, "root.lua", &ValueLimits::default())
    }

    #[test]
    fn accepts_booleans_integers_strings_arrays_and_records() {
        assert!(check("return { name = \"cast\", version = 3, flags = { true, false } }").is_ok());
        assert!(check("return { 1, 2, 3 }").is_ok());
    }

    #[test]
    fn rejects_floats() {
        let error = check("return { ratio = 1.5 }").unwrap_err();
        assert_eq!(error.category, DiagnosticCategory::Type);
        assert!(error.message.contains("float"));
    }

    #[test]
    fn rejects_functions_in_the_tree() {
        assert!(check("return { build = function() return 1 end }").is_err());
    }

    #[test]
    fn rejects_a_sparse_array() {
        // Index 2 is absent, so `#t` (1) disagrees with the two integer keys.
        assert!(check("local t = {} t[1] = \"a\" t[3] = \"c\" return t").is_err());
    }

    #[test]
    fn rejects_mixed_key_tables() {
        assert!(check("return { [1] = \"a\", name = \"b\" }").is_err());
    }

    #[test]
    fn rejects_a_cycle() {
        assert!(check("local t = {} t.self = t return t").is_err());
    }

    #[test]
    fn rejects_a_table_with_a_metatable() {
        // Authored sandbox code cannot reach `setmetatable`; construct the
        // hostile shape through the host API to prove the validator still
        // rejects it defensively.
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        let table = lua.create_table().unwrap();
        table.set_metatable(Some(lua.create_table().unwrap()));
        let error =
            validate_value_tree(&Value::Table(table), "root.lua", &ValueLimits::default())
                .unwrap_err();
        assert!(error.message.contains("metatable"));
    }
}
