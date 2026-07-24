//! Lua declaration adapter for the repository domain (Phase L4).
//!
//! Decodes authored Lua repository fragments into the same shared `Map` the
//! Gluon adapter produces, reusing the shared `decode_specs` validation. Options
//! use the Lua tagged encoding; the conversion into the shared wire types
//! mirrors the Gluon conversion, so equivalent sources normalize to equal domain
//! values with intentionally distinct evaluation identities. Registration and
//! the canonical Lua emitter are added in a later slice.

use std::fmt::Write as _;

use config::declaration::ConfigDeclarationEvaluator;
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{
    GENERATED_LUA_MARKER, LuaEngine, LuaOption, lua_optional_bool, lua_optional_integer,
    lua_optional_string, lua_string,
};
use serde::Deserialize;

use super::gluon::{RepositoryCodec, decode_specs, repository_to_spec};
use super::Map;
use crate::repository::RepositoryConversionError;
use crate::system_model::spec::{RepositorySourceSpec, RepositorySpec};

/// The Lua encoding of a [`RepositorySpec`]. Shared with the system-model
/// adapter, which embeds the same repository records.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaRepositorySpec {
    id: String,
    description: LuaOption<String>,
    source: LuaRepositorySourceSpec,
    priority: LuaOption<i64>,
    enabled: LuaOption<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LuaRepositorySourceSpec {
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

/// Stateful Lua adapter for the repository declaration boundary. Decodes an
/// authored `.lua` fragment into the same shared [`Map`] the Gluon codec
/// produces, with an intentionally distinct evaluation identity.
#[derive(Debug, Clone, Default)]
pub struct LuaRepositoryCodec {
    engine: LuaEngine,
}

impl DeclarationEvaluator<Map> for LuaRepositoryCodec {
    type Identity = EvaluationIdentity;
    type Error = RepositoryConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<Map, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        let evaluated = self
            .engine
            .evaluate_within_as::<Vec<LuaRepositorySpec>>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let map = decode_lua_specs(evaluated.value).map_err(DeclarationEvaluationError::Conversion)?;
        Ok(Evaluation {
            value: map,
            identity: evaluated.identity,
        })
    }
}

impl ConfigDeclarationEvaluator for LuaRepositoryCodec {
    type Config = Map;
}

/// One registered repository declaration language (`.glu` or `.lua`), selected
/// by file extension. Both engines reach the same [`Map`]; the conversion error
/// type is shared, so no error unification is needed.
#[derive(Debug, Clone)]
pub enum RepositoryEvaluator {
    Gluon(RepositoryCodec),
    Lua(LuaRepositoryCodec),
}

impl DeclarationEvaluator<Map> for RepositoryEvaluator {
    type Identity = EvaluationIdentity;
    type Error = RepositoryConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Gluon(codec) => {
                <RepositoryCodec as DeclarationEvaluator<Map>>::language_spec(codec)
            }
            Self::Lua(codec) => <LuaRepositoryCodec as DeclarationEvaluator<Map>>::language_spec(codec),
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(codec) => <RepositoryCodec as DeclarationEvaluator<Map>>::limits(codec),
            Self::Lua(codec) => <LuaRepositoryCodec as DeclarationEvaluator<Map>>::limits(codec),
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(codec) => Self::Gluon(
                <RepositoryCodec as DeclarationEvaluator<Map>>::with_source_root(codec, source_root),
            ),
            Self::Lua(codec) => Self::Lua(
                <LuaRepositoryCodec as DeclarationEvaluator<Map>>::with_source_root(codec, source_root),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<Map, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        match self {
            Self::Gluon(codec) => codec.evaluate_within(source, deadline),
            Self::Lua(codec) => codec.evaluate_within(source, deadline),
        }
    }
}

impl ConfigDeclarationEvaluator for RepositoryEvaluator {
    type Config = Map;
}

impl RepositoryEvaluator {
    /// The registered repository languages, `.glu` first, sharing a limit.
    pub fn registered() -> [Self; 2] {
        [
            Self::Gluon(RepositoryCodec::default()),
            Self::Lua(LuaRepositoryCodec::default()),
        ]
    }
}

/// Emit a repository [`Map`] as canonical, generated-marked Lua source that
/// re-decodes through [`decode_lua_specs`] into the same map. The specs are
/// derived by the shared `repository_to_spec`, so the Lua and Gluon emitters
/// canonicalize identical domain values.
#[cfg_attr(not(test), allow(dead_code))]
fn encode_lua_specs(map: &Map) -> Result<String, RepositoryConversionError> {
    let mut specs = map.iter().map(repository_to_spec).collect::<Result<Vec<_>, _>>()?;
    specs.sort_by(|left, right| left.id.cmp(&right.id));

    let mut output = String::from(GENERATED_LUA_MARKER);
    output.push_str("return {\n");
    for spec in &specs {
        output.push_str("    {\n");
        writeln!(output, "        id = {},", lua_string(&spec.id)).unwrap();
        writeln!(
            output,
            "        description = {},",
            lua_optional_string(spec.description.as_deref())
        )
        .unwrap();
        encode_source(&mut output, &spec.source);
        writeln!(output, "        priority = {},", lua_optional_integer(spec.priority)).unwrap();
        writeln!(output, "        enabled = {},", lua_optional_bool(spec.enabled)).unwrap();
        output.push_str("    },\n");
    }
    output.push_str("}\n");
    Ok(output)
}

fn encode_source(output: &mut String, source: &RepositorySourceSpec) {
    match source {
        RepositorySourceSpec::DirectIndex { uri } => {
            output.push_str("        source = {\n");
            output.push_str("            kind = \"direct_index\",\n");
            writeln!(output, "            uri = {},", lua_string(uri)).unwrap();
            output.push_str("        },\n");
        }
        RepositorySourceSpec::RootIndex {
            base_uri,
            channel,
            version,
            arch,
        } => {
            output.push_str("        source = {\n");
            output.push_str("            kind = \"root_index\",\n");
            writeln!(output, "            base_uri = {},", lua_string(base_uri)).unwrap();
            writeln!(
                output,
                "            channel = {},",
                lua_optional_string(channel.as_deref())
            )
            .unwrap();
            writeln!(output, "            version = {},", lua_string(version)).unwrap();
            writeln!(output, "            arch = {},", lua_optional_string(arch.as_deref())).unwrap();
            output.push_str("        },\n");
        }
    }
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

    #[test]
    fn the_paired_repository_documentation_example_normalizes_equally() {
        // Prove the shipped documentation example and its paired Lua form
        // (Phase L7) decode to the same repository map.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let gluon = std::fs::read_to_string(format!("{root}/docs/examples/gluon/repositories.glu"))
            .expect("gluon repository example");
        let lua = std::fs::read_to_string(format!("{root}/docs/examples/lua/repositories.lua"))
            .expect("lua repository example");
        assert_eq!(format!("{:?}", lua_map(&lua)), format!("{:?}", gluon_map(&gluon)));
    }

    const GLUON_ROOT_INDEX: &str = r#"
let cast = import! cast.repository.v1
[
    cast.repository.root "core" "https://packages.example/core" "stream/volatile",
    cast.repository.direct "extra" "https://packages.example/extra.index",
]
"#;

    #[test]
    fn emitted_lua_re_decodes_to_the_same_map() {
        let original = gluon_map(GLUON_ROOT_INDEX);
        let emitted = encode_lua_specs(&original).expect("map emits to lua");
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let round_tripped = lua_map(&emitted);
        assert_eq!(format!("{original:?}"), format!("{round_tripped:?}"));
    }
}
