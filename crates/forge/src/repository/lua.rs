//! Lua declaration adapter for the repository domain (Phase L4).
//!
//! Decodes authored Lua repository fragments into the same shared `Map` the
//! Gluon adapter produces, reusing the shared `decode_specs` validation. Options
//! use the Lua tagged encoding; the conversion into the shared wire types
//! mirrors the Gluon conversion, so equivalent sources normalize to equal domain
//! values with intentionally distinct evaluation identities. Registration and
//! the canonical Lua emitter are added in a later slice.

use lua_config::LuaOption;
use serde::Deserialize;

use super::gluon::decode_specs;
use super::Map;
use crate::repository::RepositoryConversionError;
use crate::system_model::spec::{RepositorySourceSpec, RepositorySpec};

#[derive(Debug, Clone, Deserialize)]
struct LuaRepositorySpec {
    id: String,
    description: LuaOption<String>,
    source: LuaRepositorySourceSpec,
    priority: LuaOption<i64>,
    enabled: LuaOption<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LuaRepositorySourceSpec {
    DirectIndex {
        uri: String,
    },
    RootIndex {
        base_uri: String,
        channel: LuaOption<String>,
        version: String,
        arch: LuaOption<String>,
    },
}

impl From<LuaRepositorySpec> for RepositorySpec {
    fn from(spec: LuaRepositorySpec) -> Self {
        Self {
            id: spec.id,
            description: spec.description.into(),
            source: spec.source.into(),
            priority: spec.priority.into(),
            enabled: spec.enabled.into(),
        }
    }
}

impl From<LuaRepositorySourceSpec> for RepositorySourceSpec {
    fn from(spec: LuaRepositorySourceSpec) -> Self {
        match spec {
            LuaRepositorySourceSpec::DirectIndex { uri } => Self::DirectIndex { uri },
            LuaRepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => Self::RootIndex {
                base_uri,
                channel: channel.into(),
                version,
                arch: arch.into(),
            },
        }
    }
}

/// Convert decoded Lua repository fragments into the shared, validated map.
fn decode_lua_specs(specs: Vec<LuaRepositorySpec>) -> Result<Map, RepositoryConversionError> {
    decode_specs(specs.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};
    use lua_config::LuaEngine;

    use super::*;
    use crate::repository::gluon::RepositoryCodec;

    const GLUON_REPOSITORY: &str = r#"
let cast = import! cast.repository.v1
[
    cast.repository.direct "main" "https://packages.example/stone.index",
]
"#;

    const LUA_REPOSITORY: &str = r#"
return {
    {
        id = "main",
        description = { kind = "none" },
        source = {
            kind = "direct_index",
            uri = "https://packages.example/stone.index",
        },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
}
"#;

    fn lua_map(source: &str) -> Map {
        let specs = LuaEngine::default()
            .evaluate_as::<Vec<LuaRepositorySpec>>(&Source::new("repository.lua", source))
            .expect("lua repository evaluates")
            .value;
        decode_lua_specs(specs).expect("lua repository is valid")
    }

    fn gluon_map(source: &str) -> Map {
        <RepositoryCodec as DeclarationEvaluator<Map>>::evaluate(
            &RepositoryCodec::default(),
            &Source::new("repository.glu", source),
        )
        .expect("gluon repository evaluates")
        .value
    }

    #[test]
    fn a_lua_repository_normalizes_to_the_same_map_as_gluon() {
        assert_eq!(
            format!("{:?}", lua_map(LUA_REPOSITORY)),
            format!("{:?}", gluon_map(GLUON_REPOSITORY)),
        );
    }
}
