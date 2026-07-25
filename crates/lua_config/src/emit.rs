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

/// Re-indent generated Lua produced by the domain emitters into readable,
/// multi-line form. This is a whitespace-only transform over the emitters' own
/// controlled output — not a general Lua formatter — so the re-indented source
/// decodes to exactly the same value. It expands each non-empty `{ … }` table so
/// every element sits on its own indented line, keeps empty tables inline as
/// `{}`, preserves string literals verbatim (including any braces/commas inside
/// them), and passes `--` line comments through unchanged.
pub fn pretty_lua(source: &str) -> String {
    let mut out = String::with_capacity(source.len() * 2);
    let mut chars = source.chars().peekable();
    let mut depth: usize = 0;

    while let Some(character) = chars.next() {
        match character {
            '"' => {
                out.push('"');
                while let Some(inner) = chars.next() {
                    out.push(inner);
                    match inner {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                out.push('-');
                for comment in chars.by_ref() {
                    out.push(comment);
                    if comment == '\n' {
                        break;
                    }
                }
            }
            '{' => {
                skip_inline_whitespace(&mut chars);
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push_str("{}");
                } else {
                    depth += 1;
                    out.push_str("{\n");
                    push_indent(&mut out, depth);
                }
            }
            '}' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                depth = depth.saturating_sub(1);
                out.push('\n');
                push_indent(&mut out, depth);
                out.push('}');
            }
            ',' => {
                skip_inline_whitespace(&mut chars);
                out.push(',');
                if chars.peek() != Some(&'}') {
                    out.push('\n');
                    push_indent(&mut out, depth);
                }
            }
            whitespace if whitespace.is_whitespace() => {
                if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                    out.push(' ');
                }
            }
            other => out.push(other),
        }
    }
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

fn skip_inline_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while matches!(chars.peek(), Some(character) if character.is_whitespace()) {
        chars.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_expands_nested_tables_and_keeps_empty_tables_inline() {
        // Trailing commas are preserved as-authored: the input has none on its
        // last elements, so neither does the output.
        let dense = "return { a = 1, b = { c = 2, d = {  } }, e = {} }";
        let pretty = pretty_lua(dense);
        assert_eq!(
            pretty,
            "return {\n    a = 1,\n    b = {\n        c = 2,\n        d = {}\n    },\n    e = {}\n}"
        );
    }

    #[test]
    fn pretty_preserves_string_literals_with_structural_characters() {
        let dense = r#"return { path = "a,{b}=c", note = "he said \"hi\"" }"#;
        let pretty = pretty_lua(dense);
        // Braces/commas/quotes inside strings are untouched.
        assert!(pretty.contains(r#""a,{b}=c""#), "{pretty}");
        assert!(pretty.contains(r#""he said \"hi\"""#), "{pretty}");
    }

    #[test]
    fn pretty_passes_generated_marker_comment_through() {
        let dense = "-- @generated by cast. DO NOT EDIT.\nreturn { a = 1 }";
        let pretty = pretty_lua(dense);
        assert!(pretty.starts_with("-- @generated by cast. DO NOT EDIT.\n"));
    }

    #[test]
    fn pretty_is_idempotent() {
        let dense = "return { a = { b = 1, c = {  } }, d = { 1, 2, 3 } }";
        let once = pretty_lua(dense);
        assert_eq!(pretty_lua(&once), once);
    }

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
