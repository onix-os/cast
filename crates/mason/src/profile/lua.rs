//! Lua declaration adapter for the profile domain (Phase L4).
//!
//! Decodes authored Lua profile fragments into the same shared `Map` the Gluon
//! adapter produces, reusing the shared `decode_specs` validation. Options use
//! the Lua tagged encoding; the conversion into the shared wire types mirrors
//! the Gluon conversion, so equivalent sources normalize to equal domain
//! values with intentionally distinct evaluation identities. Registration and
//! the canonical Lua emitter are added in a later slice.

use lua_config::LuaOption;
use serde::Deserialize;

use super::{Map, ProfileConversionError, ProfileSpec, RepositorySourceSpec, RepositorySpec, decode_specs};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaProfileSpec {
    id: String,
    repositories: Vec<LuaRepositorySpec>,
}

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

impl From<LuaProfileSpec> for ProfileSpec {
    fn from(spec: LuaProfileSpec) -> Self {
        Self {
            id: spec.id,
            repositories: spec.repositories.into_iter().map(Into::into).collect(),
        }
    }
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

/// Convert decoded Lua profile fragments into the shared, validated domain map.
pub(crate) fn decode_lua_specs(
    specs: Vec<LuaProfileSpec>,
) -> Result<Map, ProfileConversionError> {
    decode_specs(specs.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::*;
    use crate::profile::gluon::ProfileCodec;
    use declarative_config::DeclarationEvaluator;

    const GLUON_PROFILE: &str = r#"
let cast = import! cast.profile.v1
[
    cast.profile "desktop" [
        cast.repository.direct "main" "https://packages.example/stone.index",
    ],
]
"#;

    const LUA_PROFILE: &str = r#"
return {
    {
        id = "desktop",
        repositories = {
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
        },
    },
}
"#;

    fn lua_map(source: &str) -> Map {
        let specs = LuaEngine::default()
            .evaluate_as::<Vec<LuaProfileSpec>>(&Source::new("profile.lua", source))
            .expect("lua profile evaluates")
            .value;
        decode_lua_specs(specs).expect("lua profile is valid")
    }

    fn gluon_map(source: &str) -> Map {
        <ProfileCodec as DeclarationEvaluator<Map>>::evaluate(
            &ProfileCodec::default(),
            &Source::new("profile.glu", source),
        )
        .expect("gluon profile evaluates")
        .value
    }

    #[test]
    fn a_lua_profile_normalizes_to_the_same_map_as_gluon() {
        assert_eq!(
            format!("{:?}", lua_map(LUA_PROFILE)),
            format!("{:?}", gluon_map(GLUON_PROFILE)),
        );
    }
}
