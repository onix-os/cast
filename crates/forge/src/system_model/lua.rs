//! Lua declaration adapter for the system-model domain (Phase L5).
//!
//! Decodes an authored Lua system declaration into the same shared
//! [`SystemParts`](spec) the Gluon adapter produces, reusing the neutral
//! `spec::into_domain` conversion and `SystemModel::from_generated`. The
//! repository records use the shared Lua repository encoding; equivalent Gluon
//! and Lua sources normalize to equal semantic values with intentionally
//! distinct evaluation identities.

use std::fmt::Write as _;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::gluon::SystemSnapshotCodec;
use super::{SystemModel, spec};
use crate::declaration_migration::DeclarationMigrationRequest;
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

/// Emit a decoded [`SystemModel`] as canonical generated-marked Lua source that
/// re-decodes through [`LuaSystemEvaluator`] to the same semantic value. This is
/// the system-model write path — what a Gluon→Lua declaration migration emits
/// for the `etc/cast/system.glu` slot. Repository records reuse the shared
/// repository encoding, so a system model and a standalone repositories
/// document canonicalize their repositories identically.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_lua_system(model: &SystemModel) -> Result<String, spec::ConversionError> {
    let system = spec::SystemSpec::try_from(model)?;

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
    Ok(output)
}

/// Failure building a system-model migration request. Every path fails closed —
/// no request is produced unless the converted Lua is proven to normalize to the
/// original value.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SystemMigrationError {
    #[error("decode the original generated system declaration")]
    DecodeOriginal(#[source] DeclarationEvaluationError<spec::ConversionError>),
    #[error("re-encode the system declaration as Lua")]
    Encode(#[source] spec::ConversionError),
    #[error("decode the re-encoded Lua system declaration")]
    DecodeConverted(#[source] DeclarationEvaluationError<spec::ConversionError>),
    #[error("the re-encoded Lua system declaration does not normalize to the original value")]
    ConversionDiverged,
}

/// Build a fail-closed Gluon→Lua migration request for a state's generated
/// system-model snapshot slot (`usr/lib/system-model.glu`).
///
/// The original generated Gluon is decoded, re-emitted as Lua, and the Lua is
/// decoded again; only if it normalizes to the *same* [`spec::SystemSpec`] as
/// the original is a [`DeclarationMigrationRequest`] produced. A conversion that
/// would not round-trip is rejected rather than committed. This is pure: the
/// caller supplies the original bytes and the live bindings the bridge later
/// revalidates against (the state id, logical slot, and the state's retained
/// `/usr` tree marker) — it reads no filesystem and commits nothing.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn convert_generated_system_declaration(
    state_id: i32,
    logical_slot: &str,
    original_gluon: &str,
    state_tree_marker: &[u8],
) -> Result<DeclarationMigrationRequest, SystemMigrationError> {
    let original = <SystemSnapshotCodec as DeclarationEvaluator<SystemModel>>::evaluate(
        &SystemSnapshotCodec::default(),
        &Source::new(logical_slot, original_gluon),
    )
    .map_err(SystemMigrationError::DecodeOriginal)?
    .value;

    let converted = encode_lua_system(&original).map_err(SystemMigrationError::Encode)?;

    let reevaluated = LuaSystemEvaluator::default()
        .evaluate(&Source::new(logical_slot, &converted))
        .map_err(SystemMigrationError::DecodeConverted)?;

    // Fail closed: the migrated artifact must normalize to the original value.
    let original_spec = spec::SystemSpec::try_from(&original).map_err(SystemMigrationError::Encode)?;
    let converted_spec =
        spec::SystemSpec::try_from(&reevaluated.value).map_err(SystemMigrationError::Encode)?;
    if original_spec != converted_spec {
        return Err(SystemMigrationError::ConversionDiverged);
    }

    Ok(DeclarationMigrationRequest {
        state_id,
        logical_slot: logical_slot.to_owned(),
        state_tree_marker: state_tree_marker.to_vec(),
        original_language: "gluon".to_owned(),
        original_logical_path: logical_slot.to_owned(),
        original_sha256: Sha256::digest(original_gluon.as_bytes()).to_vec(),
        migrated_language: "lua".to_owned(),
        converted_bytes: converted.into_bytes(),
        evaluation_identity: reevaluated.identity.sha256.clone().into_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::*;

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

    #[test]
    fn an_emitted_system_model_re_decodes_to_the_same_value() {
        // The migration write path: a Gluon-decoded system model emits Lua that
        // re-decodes to the same semantic value (repositories, packages, flag).
        let gluon = gluon_model(GLUON_SYSTEM);
        let emitted = encode_lua_system(&gluon).expect("system model emits to lua");
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let redecoded = lua_model(&emitted);
        assert_eq!(redecoded.disable_warning, gluon.disable_warning);
        assert_eq!(
            format!("{:?}", redecoded.repositories),
            format!("{:?}", gluon.repositories)
        );
        assert_eq!(redecoded.packages, gluon.packages);
    }

    #[test]
    fn converting_the_generated_system_declaration_builds_a_verified_request() {
        let marker = vec![9u8; 32];
        let request = convert_generated_system_declaration(
            7,
            "usr/lib/system-model.glu",
            GLUON_SYSTEM,
            &marker,
        )
        .expect("conversion succeeds");

        assert_eq!(request.state_id, 7);
        assert_eq!(request.logical_slot, "usr/lib/system-model.glu");
        assert_eq!(request.original_logical_path, "usr/lib/system-model.glu");
        assert_eq!(request.original_language, "gluon");
        assert_eq!(request.migrated_language, "lua");
        assert_eq!(request.state_tree_marker, marker);
        assert_eq!(
            request.original_sha256,
            Sha256::digest(GLUON_SYSTEM.as_bytes()).to_vec()
        );
        assert!(!request.evaluation_identity.is_empty());

        // The converted bytes are generated-marked Lua that re-decodes to the
        // same normalized value the original Gluon produced.
        let converted = String::from_utf8(request.converted_bytes.clone()).expect("utf-8 lua");
        assert!(converted.starts_with(GENERATED_LUA_MARKER));
        let redecoded = spec::SystemSpec::try_from(&lua_model(&converted)).unwrap();
        let original = spec::SystemSpec::try_from(&gluon_model(GLUON_SYSTEM)).unwrap();
        assert_eq!(redecoded, original);
    }
}
