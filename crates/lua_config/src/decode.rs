//! Reusable serde encodings for the Cast Lua declaration profile.
//!
//! `nil` is never a value in the profile, so options, patches, and closed
//! variants are expressed as string-tagged records (`{ kind = "some", value =
//! ... }`). These helpers deserialize those records and convert to ordinary
//! Rust `Option`/domain types, so each domain adapter reuses one encoding
//! rather than reinventing it. They pair with the internally-tagged serde enum
//! representation for closed variants (`#[serde(tag = "kind")]`).

use serde::Deserialize;

/// The Lua encoding of `Option<T>`: `{ kind = "none" }` or
/// `{ kind = "some", value = <T> }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LuaOption<T> {
    None,
    Some { value: T },
}

impl<T> From<LuaOption<T>> for Option<T> {
    fn from(option: LuaOption<T>) -> Self {
        match option {
            LuaOption::None => None,
            LuaOption::Some { value } => Some(value),
        }
    }
}

/// The Lua encoding of a total-value patch: `{ kind = "keep" }` or
/// `{ kind = "set", value = <T> }`. Distinct from [`LuaOption`] so an
/// intentional "keep" is never confused with an absent value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LuaPatch<T> {
    Keep,
    Set { value: T },
}

impl<T> LuaPatch<T> {
    /// Fold the patch over a base value: `keep` retains `base`, `set` replaces.
    pub fn apply(self, base: T) -> T {
        match self {
            LuaPatch::Keep => base,
            LuaPatch::Set { value } => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use serde::Deserialize;

    use super::*;
    use crate::LuaEngine;

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum Handler {
        Run { command: String, args: Vec<String> },
        Delete { paths: Vec<String> },
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct Fixture {
        name: String,
        priority: i64,
        after: LuaOption<String>,
        handler: Handler,
        tags: Vec<String>,
    }

    #[test]
    fn decodes_options_variants_and_arrays_from_the_lua_encoding() {
        let engine = LuaEngine::default();
        let evaluated = engine
            .evaluate_as::<Fixture>(&Source::new(
                "root.lua",
                r#"
                    return {
                        name = "depmod",
                        priority = 100,
                        after = { kind = "some", value = "udev" },
                        handler = { kind = "run", command = "/usr/bin/depmod", args = { "-a" } },
                        tags = { "kernel", "modules" },
                    }
                "#,
            ))
            .unwrap();

        assert_eq!(
            evaluated.value,
            Fixture {
                name: "depmod".to_owned(),
                priority: 100,
                after: LuaOption::Some { value: "udev".to_owned() },
                handler: Handler::Run {
                    command: "/usr/bin/depmod".to_owned(),
                    args: vec!["-a".to_owned()],
                },
                tags: vec!["kernel".to_owned(), "modules".to_owned()],
            }
        );
        assert_eq!(Option::<String>::from(evaluated.value.after), Some("udev".to_owned()));
    }

    #[test]
    fn the_none_option_encoding_decodes_to_none() {
        let engine = LuaEngine::default();
        let evaluated = engine
            .evaluate_as::<LuaOption<String>>(&Source::new(
                "root.lua",
                r#"return { kind = "none" }"#,
            ))
            .unwrap();
        assert_eq!(Option::<String>::from(evaluated.value), None);
    }
}
