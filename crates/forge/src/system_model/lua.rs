//! Lua declaration adapter for the system-model domain (Phase L5).
//!
//! Decodes an authored Lua system declaration into the same shared
//! [`SystemParts`](spec) the Gluon adapter produces, reusing the neutral
//! `spec::into_domain` conversion and `SystemModel::from_generated`. The
//! repository records use the shared Lua repository encoding; equivalent Gluon
//! and Lua sources normalize to equal semantic values with intentionally
//! distinct evaluation identities.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use config::declaration::GeneratedDeclarationAuthority;
use declarative_config::{
    DeclarationCodec,
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline, EvaluationIdentity, LanguageSpec,
    Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string, pretty_lua};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{SYSTEM_SNAPSHOT_PATH, SystemIntentDeclaration, SystemModel, spec};
use crate::db::state::{Database, DeclarationMigrationCommit};
use crate::repository::lua::{LuaRepositorySpec, encode_repository_record};

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

/// Stateful Lua adapter for authored system intent.
///
/// Decodes the same authored table [`LuaSystemEvaluator`] reads, but normalizes
/// it through [`SystemModel::regenerate`] so the result carries the canonical
/// generated snapshot alongside the source that produced it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LuaSystemIntentEvaluator {
    engine: LuaEngine,
}

impl DeclarationEvaluator<SystemIntentDeclaration> for LuaSystemIntentEvaluator {
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
    ) -> Result<Evaluation<SystemIntentDeclaration, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        let authored_source = source.text().to_owned();
        let evaluated = self
            .engine
            .evaluate_within_as::<LuaSystemSpec>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let parts = spec::into_domain(spec::SystemSpec::from(evaluated.value))
            .map_err(DeclarationEvaluationError::Conversion)?;
        let model = SystemModel::regenerate(parts)?;

        Ok(Evaluation {
            value: SystemIntentDeclaration { authored_source, model },
            identity: evaluated.identity,
        })
    }
}

/// Records which authored source produced a generated snapshot, written using
/// Lua comment syntax.
const SOURCE_FINGERPRINT_PREFIX: &str = "-- Authored source fingerprint: ";

pub(super) fn is_generated_snapshot(source: &str) -> bool {
    source.starts_with(GENERATED_LUA_MARKER)
}

pub(super) fn generated_source_fingerprint(source: &str) -> Option<String> {
    source
        .lines()
        .find_map(|line| line.strip_prefix(SOURCE_FINGERPRINT_PREFIX))
        .filter(|fingerprint| fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(ToOwned::to_owned)
}

pub(super) fn with_source_fingerprint(generated: &str, source_fingerprint: &str) -> String {
    let generated = generated
        .strip_prefix(GENERATED_LUA_MARKER)
        .expect("Cast-generated system snapshots always carry the generated marker");
    format!("{GENERATED_LUA_MARKER}{SOURCE_FINGERPRINT_PREFIX}{source_fingerprint}\n{generated}")
}

/// Stateful read-only Lua adapter for authored system declarations.
#[derive(Debug, Clone, Default)]
pub(crate) struct LuaSystemEvaluator {
    engine: LuaEngine,
}

impl LuaSystemEvaluator {
    /// Neutral ownership descriptor for the fixed generated snapshot slot.
    ///
    /// The language descriptor selects the public extension while the marker
    /// proves the bytes belong to Cast's system snapshot slot rather than to an
    /// authored declaration.
    pub(crate) fn generated_authority(&self) -> GeneratedDeclarationAuthority {
        GeneratedDeclarationAuthority::new(self.language_spec().clone(), GENERATED_LUA_MARKER)
            .expect("the generated system snapshot authority is valid")
    }
}

impl DeclarationCodec<SystemModel> for LuaSystemEvaluator {
    fn encode(&self, model: &SystemModel) -> Result<String, Self::Error> {
        encode_lua_system(model)
    }
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
        Ok(Evaluation { value: model, identity })
    }
}

/// Emit a decoded [`SystemModel`] as canonical generated-marked Lua source that
/// re-decodes through [`LuaSystemEvaluator`] to the same semantic value. This is
/// the system-model write path — what a Gluon→Lua declaration migration emits
/// for the `etc/cast/system.glu` slot. Repository records reuse the shared
/// repository encoding, so a system model and a standalone repositories
/// document canonicalize their repositories identically.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_lua_system(model: &SystemModel) -> Result<String, spec::ConversionError> {
    Ok(encode_normalized(&spec::SystemSpec::try_from(model)?))
}

/// Encode an already-normalized spec. `regenerate` holds a `SystemSpec` rather
/// than a `SystemModel`, so both entry points share this one body.
pub(super) fn encode_normalized(system: &spec::SystemSpec) -> String {
    let mut output = String::from(GENERATED_LUA_MARKER);
    output.push_str("return {\n");
    writeln!(output, "    disable_warning = {},", system.disable_warning).unwrap();
    output.push_str("    repositories = {\n");
    for repository in &system.repositories {
        encode_repository_record(&mut output, repository);
    }
    output.push_str("    },\n");
    output.push_str("    packages = {");
    for (index, package) in system.packages.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        output.push_str(&lua_string(package));
    }
    output.push_str("},\n");
    output.push_str("}\n");
    pretty_lua(&output)
}


#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::*;

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

    /// The snapshot write path: an emitted model re-decodes to the same
    /// semantic value, so a generated snapshot round-trips through its own
    /// adapter.
    #[test]
    fn an_emitted_system_model_re_decodes_to_the_same_value() {
        let original = lua_model(LUA_SYSTEM);
        let emitted = encode_lua_system(&original).expect("system model emits to lua");
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let redecoded = lua_model(&emitted);
        assert_eq!(redecoded.disable_warning, original.disable_warning);
        assert_eq!(
            format!("{:?}", redecoded.repositories),
            format!("{:?}", original.repositories)
        );
        assert_eq!(redecoded.packages, original.packages);
    }
}
