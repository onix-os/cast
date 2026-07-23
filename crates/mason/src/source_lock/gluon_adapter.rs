//! Stateful Gluon adapter for generated source-lock declarations.

use std::str;

use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source,
    SourceRoot,
};
use gluon_config::{EvaluationFingerprint, GluonEngine};

use super::{
    DecodeError, GluonSourceLock, SourceLock, ValidationError,
    encode_source_lock,
};

/// Gluon source-lock codec with its language and evaluator policy fixed at
/// construction.
#[derive(Debug, Clone)]
pub struct GluonSourceLockCodec {
    engine: GluonEngine,
}

impl Default for GluonSourceLockCodec {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl GluonSourceLockCodec {
    pub fn new(limits: Limits) -> Self {
        Self {
            engine: GluonEngine::new(limits),
        }
    }
}

impl DeclarationEvaluator<SourceLock> for GluonSourceLockCodec {
    type Identity = EvaluationFingerprint;
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

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<SourceLock, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate::<GluonSourceLock>(source)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let lock = SourceLock::try_from(evaluation.value)
            .map_err(DeclarationEvaluationError::Conversion)?;
        lock.validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: lock,
            identity: evaluation.fingerprint,
        })
    }
}

impl DeclarationCodec<SourceLock> for GluonSourceLockCodec {
    fn encode(&self, lock: &SourceLock) -> Result<String, Self::Error> {
        Ok(encode_source_lock(lock))
    }
}

/// Compatibility entry point while in-tree callers move to the typed codec.
pub fn decode_source_lock(
    logical_name: &str,
    bytes: &[u8],
) -> Result<SourceLock, DecodeError> {
    let source = str::from_utf8(bytes)?;
    let evaluation = GluonSourceLockCodec::default()
        .evaluate(&Source::new(logical_name, source))
        .map_err(|error| match error {
            DeclarationEvaluationError::Evaluation(error) => {
                DecodeError::Evaluation(Box::new(error))
            }
            DeclarationEvaluationError::Conversion(error) => {
                DecodeError::Validation(error)
            }
        })?;
    Ok(evaluation.value)
}
