//! Lua adapter for generated source-lock declarations.
//!
//! The source-lock domain types are engine-neutral and carry their own semantic
//! `validate`, so this adapter decodes a generated lock straight into
//! [`SourceLock`] via serde using the `kind`-tagged encoding for the resolution
//! variants, then runs the same validation any other adapter runs.

use std::{cmp::Ordering, fmt::Write as _};

use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string, pretty_lua};

use super::{ArchiveResolution, GitResolution, SourceLock, SourceResolution, ValidationError};

/// Typed adapter owning the Lua runtime values for a generated source lock.
#[derive(Debug, Clone, Default)]
pub struct LuaSourceLockCodec {
    engine: LuaEngine,
}

impl DeclarationEvaluator<SourceLock> for LuaSourceLockCodec {
    type Identity = EvaluationIdentity;
    type Error = ValidationError;

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
    ) -> Result<DeclarationEvaluation<SourceLock, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        let evaluation = self
            .engine
            .evaluate_within_as::<SourceLock>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let lock = evaluation.value;
        lock.validate().map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: lock,
            identity: evaluation.identity,
        })
    }
}

impl DeclarationCodec<SourceLock> for LuaSourceLockCodec {
    fn encode(&self, lock: &SourceLock) -> Result<String, Self::Error> {
        Ok(encode_lua_source_lock(lock))
    }
}

/// Emit a source lock as canonical, generated-marked Lua that re-decodes
/// through this adapter into the same [`SourceLock`].
pub(crate) fn encode_lua_source_lock(lock: &SourceLock) -> String {
    let mut sources = lock.sources.iter().collect::<Vec<_>>();
    sources.sort_by(|left, right| canonical_source_cmp(left, right));

    let mut output = String::from(GENERATED_LUA_MARKER);
    let _ = write!(output, "return {{\nschema_version = {},\nsources = {{\n", lock.schema_version);
    for source in sources {
        match source {
            SourceResolution::Archive(source) => {
                let _ = write!(output, "{},\n", archive(source));
            }
            SourceResolution::Git(source) => {
                let _ = write!(output, "{},\n", git(source));
            }
        }
    }
    output.push_str("},\n}\n");
    pretty_lua(&output)
}

fn archive(source: &ArchiveResolution) -> String {
    format!(
        "{{ kind = \"archive\", order = {}, url = {}, sha256 = {} }}",
        source.order,
        lua_string(&source.url),
        lua_string(&source.sha256),
    )
}

fn git(source: &GitResolution) -> String {
    format!(
        "{{ kind = \"git\", order = {}, url = {}, requested_ref = {}, commit = {}, materialization_sha256 = {} }}",
        source.order,
        lua_string(&source.url),
        lua_string(&source.requested_ref),
        lua_string(&source.commit),
        lua_string(&source.materialization_sha256),
    )
}

/// Order sources so an unchanged lock re-encodes byte-for-byte.
fn canonical_source_cmp(left: &SourceResolution, right: &SourceResolution) -> Ordering {
    left.order()
        .cmp(&right.order())
        .then_with(|| source_kind_order(left).cmp(&source_kind_order(right)))
        .then_with(|| match (left, right) {
            (SourceResolution::Archive(left), SourceResolution::Archive(right)) => {
                left.url.cmp(&right.url).then_with(|| left.sha256.cmp(&right.sha256))
            }
            (SourceResolution::Git(left), SourceResolution::Git(right)) => left
                .url
                .cmp(&right.url)
                .then_with(|| left.requested_ref.cmp(&right.requested_ref))
                .then_with(|| left.commit.cmp(&right.commit))
                .then_with(|| left.materialization_sha256.cmp(&right.materialization_sha256)),
            _ => Ordering::Equal,
        })
}

fn source_kind_order(source: &SourceResolution) -> u8 {
    match source {
        SourceResolution::Archive(_) => 0,
        SourceResolution::Git(_) => 1,
    }
}
