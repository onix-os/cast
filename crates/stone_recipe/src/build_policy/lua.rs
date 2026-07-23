//! Lua declaration DTOs for the build-policy domain (Phase L5, in progress).
//!
//! The build policy is pervaded by tuple/newtype enum variants (`TextSpec`,
//! `BuildToolSpec`, `ValuePatch`, …). Serde's internally-tagged `#[serde(tag =
//! "kind")]` encoding — the uniform Lua encoding every other domain uses — does
//! not support tuple variants, so those enums cannot be decoded by deriving
//! `Deserialize` on the domain type the way the build lock's struct/unit enums
//! were. This module holds the struct-variant Lua DTOs plus `From` conversions
//! that bridge that gap. It is the foundation of the full build-policy adapter;
//! the remaining spec tree is layered on top in later slices.

// The full build-policy adapter is assembled over several slices; these
// foundation DTOs are exercised by the tests below until the top-level
// evaluator that consumes them lands.
#![cfg_attr(not(test), allow(dead_code))]

use serde::Deserialize;

use super::{BuildToolSpec, ContextValue, TextSpec};

/// The Lua encoding of a [`TextSpec`]. The domain enum's tuple variants become
/// struct variants so the uniform `{ kind = … }` tag applies; `Context` reuses
/// the all-unit [`ContextValue`], which decodes directly from its snake_case
/// name.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaTextSpec {
    Literal { value: String },
    Context { value: ContextValue },
    Concat { values: Vec<LuaTextSpec> },
}

impl From<LuaTextSpec> for TextSpec {
    fn from(text: LuaTextSpec) -> Self {
        match text {
            LuaTextSpec::Literal { value } => Self::Literal(value),
            LuaTextSpec::Context { value } => Self::Context(value),
            LuaTextSpec::Concat { values } => {
                Self::Concat(values.into_iter().map(Into::into).collect())
            }
        }
    }
}

/// The Lua encoding of a [`BuildToolSpec`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaBuildToolSpec {
    Package { value: String },
    Binary { value: String },
    SystemBinary { value: String },
}

impl From<LuaBuildToolSpec> for BuildToolSpec {
    fn from(tool: LuaBuildToolSpec) -> Self {
        match tool {
            LuaBuildToolSpec::Package { value } => Self::Package(value),
            LuaBuildToolSpec::Binary { value } => Self::Binary(value),
            LuaBuildToolSpec::SystemBinary { value } => Self::SystemBinary(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::*;

    fn decode<T: serde::de::DeserializeOwned>(source: &str) -> T {
        LuaEngine::default()
            .evaluate_as::<T>(&Source::new("build-policy.lua", source))
            .expect("lua value decodes")
            .value
    }

    #[test]
    fn a_literal_text_spec_decodes() {
        let text: TextSpec = decode::<LuaTextSpec>(r#"return { kind = "literal", value = "cc" }"#).into();
        assert_eq!(text, TextSpec::Literal("cc".to_owned()));
    }

    #[test]
    fn a_context_text_spec_decodes_the_unit_context_value() {
        let text: TextSpec =
            decode::<LuaTextSpec>(r#"return { kind = "context", value = "package_name" }"#).into();
        assert_eq!(text, TextSpec::Context(ContextValue::PackageName));
    }

    #[test]
    fn a_nested_concat_text_spec_decodes_recursively() {
        let source = r#"
return {
    kind = "concat",
    values = {
        { kind = "literal", value = "lib" },
        { kind = "context", value = "lib_suffix" },
    },
}
"#;
        let text: TextSpec = decode::<LuaTextSpec>(source).into();
        assert_eq!(
            text,
            TextSpec::Concat(vec![
                TextSpec::Literal("lib".to_owned()),
                TextSpec::Context(ContextValue::LibSuffix),
            ])
        );
    }

    #[test]
    fn build_tool_variants_decode() {
        let package: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "package", value = "cmake" }"#).into();
        assert_eq!(package, BuildToolSpec::Package("cmake".to_owned()));

        let system: BuildToolSpec =
            decode::<LuaBuildToolSpec>(r#"return { kind = "system_binary", value = "/bin/sh" }"#)
                .into();
        assert_eq!(system, BuildToolSpec::SystemBinary("/bin/sh".to_owned()));
    }
}
