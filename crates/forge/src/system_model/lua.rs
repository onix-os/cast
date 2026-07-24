//! Lua declaration adapter for the system-model domain (Phase L5).
//!
//! Decodes an authored Lua system declaration into the same shared
//! [`SystemParts`](spec) the Gluon adapter produces, reusing the neutral
//! `spec::into_domain` conversion and `SystemModel::from_generated`. The
//! repository records use the shared Lua repository encoding; equivalent Gluon
//! and Lua sources normalize to equal semantic values with intentionally
//! distinct evaluation identities.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::LuaEngine;
use serde::Deserialize;

use super::{SystemModel, spec};
use crate::repository::lua::LuaRepositorySpec;

#[derive(Debug, Clone, Deserialize)]
struct LuaSystemSpec {
    disable_warning: bool,
    repositories: Vec<LuaRepositorySpec>,
    packages: Vec<String>,
}

impl From<LuaSystemSpec> for spec::SystemSpec {
    fn from(value: LuaSystemSpec) -> Self {
        Self {
            disable_warning: value.disable_warning,
            repositories: value.repositories.into_iter().map(Into::into).collect(),
            packages: value.packages,
        }
    }
}

/// Stateful read-only Lua adapter for authored system declarations.
#[derive(Debug, Clone, Default)]
pub(crate) struct LuaSystemEvaluator {
    engine: LuaEngine,
}

impl DeclarationEvaluator<SystemModel> for LuaSystemEvaluator {
    type Identity = EvaluationIdentity;
    type Error = spec::ConversionError;

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
    ) -> Result<Evaluation<SystemModel, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        let source_text = source.text().to_owned();
        let evaluated = self
            .engine
            .evaluate_within_as::<LuaSystemSpec>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let parts = spec::into_domain(spec::SystemSpec::from(evaluated.value))
            .map_err(DeclarationEvaluationError::Conversion)?;
        let identity = evaluated.identity;
        let model = SystemModel::from_generated(parts, source_text, identity.clone());
        Ok(Evaluation {
            value: model,
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::*;
    use crate::system_model::gluon::SystemSnapshotCodec;

    const GLUON_SYSTEM: &str = r#"
let cast = import! cast.system.v1
{
    disable_warning = cast.boolean.true,
    repositories = [
        cast.repository.direct_with {
            id = "local",
            description = cast.optional.some "local packages",
            uri = "file:///var/cache/local.index",
            priority = cast.optional.some 5,
            enabled = cast.optional.some cast.boolean.false,
        },
        cast.repository.root "volatile" "https://packages.example.test" "stream/volatile",
    ],
    packages = ["cast", "soname(libc.so.6)"],
}
"#;

    const LUA_SYSTEM: &str = r#"
return {
    disable_warning = true,
    repositories = {
        {
            id = "local",
            description = { kind = "some", value = "local packages" },
            source = { kind = "direct_index", uri = "file:///var/cache/local.index" },
            priority = { kind = "some", value = 5 },
            enabled = { kind = "some", value = false },
        },
        {
            id = "volatile",
            description = { kind = "none" },
            source = {
                kind = "root_index",
                base_uri = "https://packages.example.test",
                channel = { kind = "none" },
                version = "stream/volatile",
                arch = { kind = "none" },
            },
            priority = { kind = "none" },
            enabled = { kind = "none" },
        },
    },
    packages = { "cast", "soname(libc.so.6)" },
}
"#;

    fn lua_model(source: &str) -> SystemModel {
        LuaSystemEvaluator::default()
            .evaluate(&Source::new("system.lua", source))
            .expect("lua system evaluates")
            .value
    }

    fn gluon_model(source: &str) -> SystemModel {
        <SystemSnapshotCodec as DeclarationEvaluator<SystemModel>>::evaluate(
            &SystemSnapshotCodec::default(),
            &Source::new("system.glu", source),
        )
        .expect("gluon system evaluates")
        .value
    }

    #[test]
    fn a_lua_system_normalizes_to_the_same_semantic_value_as_gluon() {
        let lua = lua_model(LUA_SYSTEM);
        let gluon = gluon_model(GLUON_SYSTEM);

        assert_eq!(lua.disable_warning, gluon.disable_warning);
        assert_eq!(
            format!("{:?}", lua.repositories),
            format!("{:?}", gluon.repositories)
        );
        assert_eq!(lua.packages, gluon.packages);
    }

    #[test]
    fn the_paired_system_documentation_example_normalizes_equally() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let gluon = std::fs::read_to_string(format!("{root}/docs/examples/gluon/system.glu"))
            .expect("gluon system example");
        let lua = std::fs::read_to_string(format!("{root}/docs/examples/lua/system.lua"))
            .expect("lua system example");
        let gluon = gluon_model(&gluon);
        let lua = lua_model(&lua);
        assert_eq!(lua.disable_warning, gluon.disable_warning);
        assert_eq!(format!("{:?}", lua.repositories), format!("{:?}", gluon.repositories));
        assert_eq!(lua.packages, gluon.packages);
    }

    #[test]
    fn the_lua_and_gluon_system_identities_differ_by_engine() {
        let lua = lua_model(LUA_SYSTEM);
        let gluon = gluon_model(GLUON_SYSTEM);

        assert_ne!(
            lua.fingerprint().engine.implementation(),
            gluon.fingerprint().engine.implementation(),
        );
    }
}
