//! Lua declaration adapter for the profile domain.
//!
//! Decodes authored Lua profile fragments into the shared `Map`, reusing the
//! shared `decode_specs` validation. Options use the Lua tagged encoding, and
//! the conversion into the shared wire types lives in the neutral layer, so
//! any adapter reaches equal domain values with an intentionally distinct
//! evaluation identity.

use std::fmt::Write as _;

use config::declaration::ConfigDeclarationEvaluator;
use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator, Evaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{
    GENERATED_LUA_MARKER, LuaEngine, LuaOption, lua_optional_bool, lua_optional_integer,
    lua_optional_string, lua_string, pretty_lua,
};
use serde::Deserialize;

use super::{
    Map, ProfileConversionError, ProfileSpec, RepositorySourceSpec, RepositorySpec, decode_specs,
    profile_to_spec,
};

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

/// Stateful Lua adapter for the profile declaration boundary. Decodes an
/// authored `.lua` fragment into the shared [`Map`] with an intentionally
/// distinct evaluation identity.
#[derive(Debug, Clone, Default)]
pub struct LuaProfileCodec {
    engine: LuaEngine,
}

impl DeclarationEvaluator<Map> for LuaProfileCodec {
    type Identity = EvaluationIdentity;
    type Error = ProfileConversionError;

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
            .evaluate_within_as::<Vec<LuaProfileSpec>>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let map = decode_lua_specs(evaluated.value).map_err(DeclarationEvaluationError::Conversion)?;
        Ok(Evaluation {
            value: map,
            identity: evaluated.identity,
        })
    }
}

impl ConfigDeclarationEvaluator for LuaProfileCodec {
    type Config = Map;
}

impl DeclarationCodec<Map> for LuaProfileCodec {
    /// Emit the canonical generated-marked Lua source for a profile map — what
    /// a generated profile store is written as.
    fn encode(&self, config: &Map) -> Result<String, Self::Error> {
        encode_lua_specs(config)
    }
}

/// One registered profile declaration language, selected by file extension.
/// Every engine reaches the same [`Map`] and shares the conversion error type.
#[derive(Debug, Clone)]
pub enum ProfileEvaluator {
    Lua(LuaProfileCodec),
}

impl DeclarationEvaluator<Map> for ProfileEvaluator {
    type Identity = EvaluationIdentity;
    type Error = ProfileConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Lua(codec) => <LuaProfileCodec as DeclarationEvaluator<Map>>::language_spec(codec),
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Lua(codec) => <LuaProfileCodec as DeclarationEvaluator<Map>>::limits(codec),
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Lua(codec) => Self::Lua(
                <LuaProfileCodec as DeclarationEvaluator<Map>>::with_source_root(codec, source_root),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<Map, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        match self {
            Self::Lua(codec) => codec.evaluate_within(source, deadline),
        }
    }
}

impl ConfigDeclarationEvaluator for ProfileEvaluator {
    type Config = Map;
}

impl ProfileEvaluator {
    /// The registered profile languages, sharing a limit.
    pub fn registered() -> [Self; 1] {
        [Self::Lua(LuaProfileCodec::default())]
    }
}

/// Emit a profile [`Map`] as canonical, generated-marked Lua source that
/// re-decodes through [`decode_lua_specs`] into the same map. Specs are derived
/// by the shared `profile_to_spec`, so every emitter canonicalizes
/// identical domain values.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_lua_specs(map: &Map) -> Result<String, ProfileConversionError> {
    let mut specs = map.iter().map(profile_to_spec).collect::<Result<Vec<_>, _>>()?;
    specs.sort_by(|left, right| left.id.cmp(&right.id));

    let mut output = String::from(GENERATED_LUA_MARKER);
    output.push_str("return {\n");
    for profile in &specs {
        output.push_str("    {\n");
        writeln!(output, "        id = {},", lua_string(&profile.id)).unwrap();
        output.push_str("        repositories = {\n");
        let mut repositories = profile.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.id.cmp(&right.id));
        for repository in repositories {
            output.push_str("            {\n");
            writeln!(output, "                id = {},", lua_string(&repository.id)).unwrap();
            writeln!(
                output,
                "                description = {},",
                lua_optional_string(repository.description.as_deref())
            )
            .unwrap();
            encode_source(&mut output, &repository.source);
            writeln!(
                output,
                "                priority = {},",
                lua_optional_integer(repository.priority)
            )
            .unwrap();
            writeln!(
                output,
                "                enabled = {},",
                lua_optional_bool(repository.enabled)
            )
            .unwrap();
            output.push_str("            },\n");
        }
        output.push_str("        },\n");
        output.push_str("    },\n");
    }
    output.push_str("}\n");
    Ok(pretty_lua(&output))
}

fn encode_source(output: &mut String, source: &RepositorySourceSpec) {
    match source {
        RepositorySourceSpec::DirectIndex { uri } => {
            output.push_str("                source = {\n");
            output.push_str("                    kind = \"direct_index\",\n");
            writeln!(output, "                    uri = {},", lua_string(uri)).unwrap();
            output.push_str("                },\n");
        }
        RepositorySourceSpec::RootIndex {
            base_uri,
            channel,
            version,
            arch,
        } => {
            output.push_str("                source = {\n");
            output.push_str("                    kind = \"root_index\",\n");
            writeln!(output, "                    base_uri = {},", lua_string(base_uri)).unwrap();
            writeln!(
                output,
                "                    channel = {},",
                lua_optional_string(channel.as_deref())
            )
            .unwrap();
            writeln!(output, "                    version = {},", lua_string(version)).unwrap();
            writeln!(
                output,
                "                    arch = {},",
                lua_optional_string(arch.as_deref())
            )
            .unwrap();
            output.push_str("                },\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;
    use lua_config::LuaEngine;

    use super::*;
    use declarative_config::DeclarationEvaluator;

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

    /// Both repository source kinds in one profile: a root index and a direct
    /// index.
    const LUA_PROFILE_ROOT_INDEX: &str = r#"
return {
    {
        id = "server",
        repositories = {
            {
                id = "core",
                description = { kind = "none" },
                source = {
                    kind = "root_index",
                    base_uri = "https://packages.example/core",
                    channel = { kind = "none" },
                    version = "stream/volatile",
                    arch = { kind = "none" },
                },
                priority = { kind = "none" },
                enabled = { kind = "none" },
            },
            {
                id = "extra",
                description = { kind = "none" },
                source = {
                    kind = "direct_index",
                    uri = "https://packages.example/extra.index",
                },
                priority = { kind = "none" },
                enabled = { kind = "none" },
            },
        },
    },
}
"#;

    #[test]
    fn emitted_lua_profile_re_decodes_to_the_same_map() {
        let original = lua_map(LUA_PROFILE_ROOT_INDEX);
        let emitted = encode_lua_specs(&original).expect("map emits to lua");
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let round_tripped = lua_map(&emitted);
        assert_eq!(format!("{original:?}"), format!("{round_tripped:?}"));
    }


    const SHIPPED_PROFILE_LUA: &str = r#"
return {
    {
        id = "default-x86_64",
        repositories = {
            {
                id = "volatile",
                description = { kind = "some", value = "AerynOS volatile stream (CDN)" },
                source = {
                    kind = "root_index",
                    base_uri = "https://cdn.aerynos.dev/",
                    channel = { kind = "some", value = "main" },
                    version = "stream/volatile",
                    arch = { kind = "some", value = "x86_64" },
                },
                priority = { kind = "some", value = 0 },
                enabled = { kind = "some", value = true },
            },
        },
    },
}
"#;

    /// The shipped default profile must decode to the profile it names.
    #[test]
    fn the_shipped_default_profile_decodes() {
        let map = lua_map(SHIPPED_PROFILE_LUA);
        assert!(map.iter().count() > 0);
    }

    #[test]
    fn the_lua_codec_encodes_a_valid_generated_slot_authority() {
        use declarative_config::DeclarationCodec;

        let map = lua_map(LUA_PROFILE_ROOT_INDEX);
        let encoded = LuaProfileCodec::default().encode(&map).expect("codec emits lua");
        assert!(encoded.starts_with(GENERATED_LUA_MARKER));
        assert_eq!(format!("{:?}", lua_map(&encoded)), format!("{map:?}"));
    }
}
