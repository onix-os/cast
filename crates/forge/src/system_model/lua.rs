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

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::gluon::SystemSnapshotCodec;
use super::{SYSTEM_SNAPSHOT_PATH, SystemModel, spec};
use crate::db::state::{Database, DeclarationMigrationCommit};
use crate::declaration_migration::{
    BridgeError, DeclarationMigrationBlobStore, DeclarationMigrationRequest, migrate_declaration,
    resolve_migrated_blob_revalidated,
};
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

/// Failure migrating a state's generated system-model snapshot.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SystemDeclarationMigrationError {
    #[error("read the generated system snapshot {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the generated system snapshot {path:?} is not UTF-8")]
    Utf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("convert the system snapshot to Lua")]
    Convert(#[source] SystemMigrationError),
    #[error("commit the system-model migration")]
    Bridge(#[source] BridgeError),
}

/// Operator command: migrate one state's generated system-model snapshot slot
/// (`usr/lib/system-model.glu`) from Gluon to Lua.
///
/// Reads the snapshot beneath the state's root, builds a fail-closed conversion
/// request bound to the supplied retained-tree marker, and commits it through
/// the bridge — the durable content-addressed blob first, then the atomic
/// catalog row. It is deliberately *additive and inert*: it records a migration
/// but changes nothing about how the state boots or resolves declarations,
/// because no resolve hook consults the catalog yet. The caller obtains the
/// state's live `/usr` tree marker (`RetainedTreeMarker::token`) and passes it
/// so a future resolver revalidates the row against the exact tree it owns.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn migrate_state_system_declaration(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    state_id: i32,
    state_root: &Path,
    state_tree_marker: &[u8],
) -> Result<DeclarationMigrationCommit, SystemDeclarationMigrationError> {
    let path = state_root.join(SYSTEM_SNAPSHOT_PATH);
    let bytes = std::fs::read(&path).map_err(|source| SystemDeclarationMigrationError::Read {
        path: path.clone(),
        source,
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|source| SystemDeclarationMigrationError::Utf8 {
        path: path.clone(),
        source,
    })?;

    let request = convert_generated_system_declaration(
        state_id,
        SYSTEM_SNAPSHOT_PATH,
        text,
        state_tree_marker,
    )
    .map_err(SystemDeclarationMigrationError::Convert)?;

    migrate_declaration(database, blobs, request).map_err(SystemDeclarationMigrationError::Bridge)
}

/// Failure resolving a state's system-model slot through the migration catalog.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SystemSnapshotResolutionError {
    #[error("resolve the migrated system snapshot blob")]
    Bridge(#[source] BridgeError),
    #[error("the migrated system snapshot blob is not UTF-8")]
    Utf8(#[source] std::str::Utf8Error),
    #[error("evaluate the migrated Lua system snapshot")]
    Evaluate(#[source] DeclarationEvaluationError<spec::ConversionError>),
}

/// Bridge-era reader resolution for a state's system-model slot: return the
/// migrated Lua [`SystemModel`] when — and only when — a committed catalog row
/// exists *and* revalidates against the live state's `/usr` tree marker and the
/// exact original snapshot bytes.
///
/// This is the fail-closed core of the reader hook, kept independent of the live
/// read paths: `Ok(None)` means no migration is committed, so the caller reads
/// the legacy `.glu` unchanged; `Ok(Some(model))` is the verified Lua snapshot;
/// an `Err` means a committed row drifted from the tree marker or original
/// source — the caller must fail, never silently fall back. `original_snapshot`
/// is the legacy `.glu` text the reader would otherwise load; its SHA-256 is
/// compared to the catalog row so a swapped-out original is rejected.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_migrated_system_snapshot(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    state_id: i32,
    logical_slot: &str,
    original_snapshot: &str,
    state_tree_marker: &[u8],
) -> Result<Option<SystemModel>, SystemSnapshotResolutionError> {
    let expected_original_sha256 = Sha256::digest(original_snapshot.as_bytes()).to_vec();

    let Some(blob) = resolve_migrated_blob_revalidated(
        database,
        blobs,
        state_id,
        logical_slot,
        state_tree_marker,
        &expected_original_sha256,
    )
    .map_err(SystemSnapshotResolutionError::Bridge)?
    else {
        return Ok(None);
    };

    let text = std::str::from_utf8(&blob).map_err(SystemSnapshotResolutionError::Utf8)?;
    let model = LuaSystemEvaluator::default()
        .evaluate(&Source::new(logical_slot, text))
        .map_err(SystemSnapshotResolutionError::Evaluate)?
        .value;
    Ok(Some(model))
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

    #[test]
    fn the_operator_command_migrates_a_state_system_snapshot() {
        use crate::declaration_migration::resolve_migrated_blob;

        let state_root = tempfile::tempdir().unwrap();
        let blobs_dir = tempfile::tempdir().unwrap();
        let blobs = DeclarationMigrationBlobStore::new(blobs_dir.path());
        let database = Database::new(":memory:").unwrap();
        let state_id = i32::from(database.add(&[], None, None).unwrap().id);

        // Lay down the generated snapshot beneath the state root.
        let snapshot = state_root.path().join(SYSTEM_SNAPSHOT_PATH);
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        std::fs::write(&snapshot, GLUON_SYSTEM).unwrap();

        let marker = vec![3u8; 32];
        let commit = migrate_state_system_declaration(
            &database,
            &blobs,
            state_id,
            state_root.path(),
            &marker,
        )
        .expect("system snapshot migrates");
        assert_eq!(commit, DeclarationMigrationCommit::Committed);

        // The committed Lua blob resolves and re-decodes to the original value.
        let resolved = resolve_migrated_blob(&database, &blobs, state_id, SYSTEM_SNAPSHOT_PATH)
            .unwrap()
            .expect("a committed row selects the blob");
        let redecoded =
            spec::SystemSpec::try_from(&lua_model(std::str::from_utf8(&resolved).unwrap())).unwrap();
        let original = spec::SystemSpec::try_from(&gluon_model(GLUON_SYSTEM)).unwrap();
        assert_eq!(redecoded, original);

        // Idempotent: re-running commits nothing new.
        let again = migrate_state_system_declaration(
            &database,
            &blobs,
            state_id,
            state_root.path(),
            &marker,
        )
        .expect("re-migration is idempotent");
        assert_eq!(again, DeclarationMigrationCommit::AlreadyPresent);
    }

    #[test]
    fn the_reader_hook_resolves_a_migrated_snapshot_and_fails_closed_on_drift() {
        let state_root = tempfile::tempdir().unwrap();
        let blobs_dir = tempfile::tempdir().unwrap();
        let blobs = DeclarationMigrationBlobStore::new(blobs_dir.path());
        let database = Database::new(":memory:").unwrap();
        let state_id = i32::from(database.add(&[], None, None).unwrap().id);

        let snapshot = state_root.path().join(SYSTEM_SNAPSHOT_PATH);
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        std::fs::write(&snapshot, GLUON_SYSTEM).unwrap();

        let marker = vec![5u8; 32];
        migrate_state_system_declaration(&database, &blobs, state_id, state_root.path(), &marker)
            .expect("snapshot migrates");

        // A committed row that revalidates resolves to the migrated model.
        let resolved = resolve_migrated_system_snapshot(
            &database,
            &blobs,
            state_id,
            SYSTEM_SNAPSHOT_PATH,
            GLUON_SYSTEM,
            &marker,
        )
        .expect("resolution succeeds")
        .expect("a committed row resolves");
        assert_eq!(
            spec::SystemSpec::try_from(&resolved).unwrap(),
            spec::SystemSpec::try_from(&gluon_model(GLUON_SYSTEM)).unwrap(),
        );

        // A drifted tree marker fails closed rather than selecting the blob.
        let wrong_marker = vec![6u8; 32];
        assert!(resolve_migrated_system_snapshot(
            &database,
            &blobs,
            state_id,
            SYSTEM_SNAPSHOT_PATH,
            GLUON_SYSTEM,
            &wrong_marker,
        )
        .is_err());

        // A swapped-out original source fails closed.
        assert!(resolve_migrated_system_snapshot(
            &database,
            &blobs,
            state_id,
            SYSTEM_SNAPSHOT_PATH,
            "return { disable_warning = true }\n",
            &marker,
        )
        .is_err());

        // A state with no committed row yields None — the caller reads legacy.
        let other_state = i32::from(database.add(&[], None, None).unwrap().id);
        assert!(resolve_migrated_system_snapshot(
            &database,
            &blobs,
            other_state,
            SYSTEM_SNAPSHOT_PATH,
            GLUON_SYSTEM,
            &marker,
        )
        .expect("resolution succeeds")
        .is_none());
    }
}
