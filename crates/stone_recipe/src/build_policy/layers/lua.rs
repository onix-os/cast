//! Lua adapter for the ordered build-policy composition manifest (Phase L5).
//!
//! Decodes an authored Lua manifest into the same shared [`BuildPolicyRootSpec`]
//! the Gluon adapter produces and runs the identical `validate` pass. The
//! operation variants use the Lua tagged encoding; equivalent Gluon and Lua
//! sources normalize to equal specs with intentionally distinct evaluation
//! identities. This domain admits explicit inputs, so the adapter implements
//! [`DeclarationInputEvaluator`] as well and binds them into the identity.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, DeclarationInputEvaluator,
    Evaluation as DeclarationEvaluation, EvaluationDeadline, EvaluationIdentity, LanguageSpec,
    Limits, Source, SourceRoot,
};
use lua_config::LuaEngine;
use serde::Deserialize;

use super::{
    BuildPolicyLayerEntrySpec, BuildPolicyLayerSpec, BuildPolicyOperation,
    BuildPolicyRootConversionError, BuildPolicyRootSpec,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LuaBuildPolicyOperation {
    Add,
    Replace,
    Modify,
}

#[derive(Debug, Clone, Deserialize)]
struct LuaBuildPolicyLayerEntrySpec {
    operation: LuaBuildPolicyOperation,
    origin: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LuaBuildPolicyLayerSpec {
    name: String,
    entries: Vec<LuaBuildPolicyLayerEntrySpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct LuaBuildPolicyRootSpec {
    name: String,
    layers: Vec<LuaBuildPolicyLayerSpec>,
}

impl From<LuaBuildPolicyOperation> for BuildPolicyOperation {
    fn from(value: LuaBuildPolicyOperation) -> Self {
        match value {
            LuaBuildPolicyOperation::Add => Self::Add,
            LuaBuildPolicyOperation::Replace => Self::Replace,
            LuaBuildPolicyOperation::Modify => Self::Modify,
        }
    }
}

impl From<LuaBuildPolicyLayerEntrySpec> for BuildPolicyLayerEntrySpec {
    fn from(value: LuaBuildPolicyLayerEntrySpec) -> Self {
        Self {
            operation: value.operation.into(),
            origin: value.origin,
        }
    }
}

impl From<LuaBuildPolicyLayerSpec> for BuildPolicyLayerSpec {
    fn from(value: LuaBuildPolicyLayerSpec) -> Self {
        Self {
            name: value.name,
            entries: value.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<LuaBuildPolicyRootSpec> for BuildPolicyRootSpec {
    fn from(value: LuaBuildPolicyRootSpec) -> Self {
        Self {
            name: value.name,
            layers: value.layers.into_iter().map(Into::into).collect(),
        }
    }
}

/// Stateful Lua adapter for the ordered build-policy root manifest.
#[derive(Debug, Clone, Default)]
pub struct LuaBuildPolicyRootEvaluator {
    engine: LuaEngine,
}

impl LuaBuildPolicyRootEvaluator {
    fn evaluate_root(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<BuildPolicyRootSpec, EvaluationIdentity>,
        DeclarationEvaluationError<BuildPolicyRootConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs_within_as::<LuaBuildPolicyRootSpec>(
                source,
                explicit_inputs,
                deadline,
            )
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let root = BuildPolicyRootSpec::from(evaluation.value);
        root.validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: root,
            identity: evaluation.identity,
        })
    }
}

impl DeclarationEvaluator<BuildPolicyRootSpec> for LuaBuildPolicyRootEvaluator {
    type Identity = EvaluationIdentity;
    type Error = BuildPolicyRootConversionError;

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
        DeclarationEvaluation<BuildPolicyRootSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_root(source, &[], deadline)
    }
}

impl DeclarationInputEvaluator<BuildPolicyRootSpec> for LuaBuildPolicyRootEvaluator {
    fn evaluate_with_inputs_within(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<BuildPolicyRootSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_root(source, explicit_inputs, deadline)
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};

    use super::super::GluonBuildPolicyRootEvaluator;
    use super::*;

    const GLUON_MANIFEST: &str = r#"
let cast = import! cast.build_policy.layers.v1
cast.policy "base" [
    cast.layer "core" [
        cast.add "core/base.glu",
        cast.modify "core/tuning.glu",
    ],
    cast.layer "arch" [
        cast.replace "arch/x86_64.glu",
    ],
]
"#;

    const LUA_MANIFEST: &str = r#"
return {
    name = "base",
    layers = {
        {
            name = "core",
            entries = {
                { operation = { kind = "add" }, origin = "core/base.glu" },
                { operation = { kind = "modify" }, origin = "core/tuning.glu" },
            },
        },
        {
            name = "arch",
            entries = {
                { operation = { kind = "replace" }, origin = "arch/x86_64.glu" },
            },
        },
    },
}
"#;

    fn lua_spec(source: &str) -> BuildPolicyRootSpec {
        LuaBuildPolicyRootEvaluator::default()
            .evaluate(&Source::new("build-policy.lua", source))
            .expect("lua manifest evaluates")
            .value
    }

    fn gluon_spec(source: &str) -> BuildPolicyRootSpec {
        GluonBuildPolicyRootEvaluator::default()
            .evaluate(&Source::new("build-policy.glu", source))
            .expect("gluon manifest evaluates")
            .value
    }

    #[test]
    fn a_lua_manifest_normalizes_to_the_same_spec_as_gluon() {
        assert_eq!(lua_spec(LUA_MANIFEST), gluon_spec(GLUON_MANIFEST));
    }

    #[test]
    fn the_lua_and_gluon_manifest_identities_differ_by_engine() {
        let lua = LuaBuildPolicyRootEvaluator::default()
            .evaluate(&Source::new("build-policy.lua", LUA_MANIFEST))
            .unwrap()
            .identity;
        let gluon = GluonBuildPolicyRootEvaluator::default()
            .evaluate(&Source::new("build-policy.glu", GLUON_MANIFEST))
            .unwrap()
            .identity;
        assert_ne!(lua.engine.implementation(), gluon.engine.implementation());
    }
}
