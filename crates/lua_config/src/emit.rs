//! Canonical Lua-value emission primitives shared by the domain emitters.
//!
//! These format Rust values into the same tagged Lua encoding the decoders in
//! [`crate::decode`] accept, so an emitted fragment re-decodes to the value it
//! was produced from. Domain emitters compose these for their records; the
//! primitives here own string escaping and the `LuaOption` tag shape so every
//! domain agrees on it.

use std::fmt::Write as _;

/// Emit a Lua string literal with the escapes the sandboxed parser accepts.
pub fn lua_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

/// Emit an optional string as the `LuaOption` tag the decoder expects.
pub fn lua_optional_string(value: Option<&str>) -> String {
    lua_option(value.map(lua_string))
}

/// Emit an optional integer as the `LuaOption` tag.
pub fn lua_optional_integer(value: Option<i64>) -> String {
    lua_option(value.map(|value| value.to_string()))
}

/// Emit an optional boolean as the `LuaOption` tag.
pub fn lua_optional_bool(value: Option<bool>) -> String {
    lua_option(value.map(|value| value.to_string()))
}

/// Wrap an already-encoded value in the `{ kind = "some", value = ... }` tag,
/// or emit `{ kind = "none" }` when absent.
pub fn lua_option(encoded_value: Option<String>) -> String {
    match encoded_value {
        None => "{ kind = \"none\" }".to_owned(),
        Some(value) => {
            let mut output = String::from("{ kind = \"some\", value = ");
            output.push_str(&value);
            let _ = write!(output, " }}");
            output
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_the_control_and_quote_characters() {
        assert_eq!(lua_string("a\"b\\c\nd\te"), r#""a\"b\\c\nd\te""#);
    }

    #[test]
    fn options_use_the_tagged_encoding() {
        assert_eq!(lua_optional_string(None), r#"{ kind = "none" }"#);
        assert_eq!(
            lua_optional_string(Some("x")),
            r#"{ kind = "some", value = "x" }"#
        );
        assert_eq!(
            lua_optional_integer(Some(5)),
            r#"{ kind = "some", value = 5 }"#
        );
        assert_eq!(
            lua_optional_bool(Some(false)),
            r#"{ kind = "some", value = false }"#
        );
    }
}
