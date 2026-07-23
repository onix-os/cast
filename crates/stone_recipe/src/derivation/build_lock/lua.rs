//! Lua adapter for the canonical build-lock (Phase L5).
//!
//! The build-lock domain types are engine-neutral and carry their own semantic
//! `validate`, so the Lua adapter decodes an authored manifest straight into
//! [`BuildLock`] via serde (using the tagged Lua encoding for the input-origin
//! and role variants) and runs the identical validation the Gluon codec runs.
//! Equivalent Gluon and Lua sources normalize to equal locks with intentionally
//! distinct evaluation identities.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::LuaEngine;

use super::{BuildLock, BuildLockValidationError};

/// Stateful Lua adapter for the canonical build-lock declaration.
#[derive(Debug, Clone, Default)]
pub struct LuaBuildLockCodec {
    engine: LuaEngine,
}

impl DeclarationEvaluator<BuildLock> for LuaBuildLockCodec {
    type Identity = EvaluationIdentity;
    type Error = BuildLockValidationError;

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
    ) -> Result<
        DeclarationEvaluation<BuildLock, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<BuildLock>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let lock = evaluation.value;
        lock.validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: lock,
            identity: evaluation.identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::super::sample_lock;
    use super::*;

    // The Lua declaration profile forbids helper functions and locals, so the
    // platform and identity records are written inline.
    const LUA_LOCK: &str = r#"
return {
    schema_version = 6,
    request_fingerprint = "request-fingerprint",
    repositories = {
        {
            id = "volatile",
            index_uri = "https://example.invalid/stone.index",
            snapshot = "repository-snapshot",
        },
    },
    requests = {
        {
            request = "binary(hello)",
            package_id = "hello-id",
            output = "out",
            origins = {
                {
                    kind = "builder_tool",
                    selection = { kind = "package" },
                    index = 0,
                },
            },
        },
    },
    packages = {
        {
            package_id = "cmake-id",
            name = "cmake",
            version = "3.31.0-1",
            architecture = "x86_64",
            repository = "volatile",
            outputs = { { name = "out" } },
            dependencies = {},
        },
        {
            package_id = "hello-id",
            name = "hello",
            version = "1.0.0-1",
            architecture = "x86_64",
            repository = "volatile",
            outputs = { { name = "out" } },
            dependencies = { { package_id = "cmake-id", output = "out" } },
        },
    },
    build_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    host_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    target_platform = { architecture = "x86_64", vendor = "unknown", operating_system = "linux", abi = "gnu" },
    policy = { name = "aerynos", fingerprint = "policy-fingerprint" },
    target = { name = "x86_64", fingerprint = "target-fingerprint" },
    profile = { name = "default-x86_64", fingerprint = "profile-fingerprint" },
    toolchain = { name = "llvm", fingerprint = "toolchain-fingerprint" },
    builder = { name = "cmake", fingerprint = "builder-fingerprint" },
}
"#;

    #[test]
    fn a_lua_build_lock_normalizes_to_the_same_value_as_the_domain_sample() {
        let decoded = LuaBuildLockCodec::default()
            .evaluate(&Source::new("build.lock.lua", LUA_LOCK))
            .expect("lua build lock evaluates")
            .value;

        assert_eq!(decoded, sample_lock());
    }

    #[test]
    fn the_lua_build_lock_identity_is_the_lua_engine() {
        let identity = LuaBuildLockCodec::default()
            .evaluate(&Source::new("build.lock.lua", LUA_LOCK))
            .expect("lua build lock evaluates")
            .identity;
        assert_eq!(identity.engine.implementation(), "lua");
    }
}
