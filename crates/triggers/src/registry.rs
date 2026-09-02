//! Language dispatch for the trigger domain.
//!
//! One [`TriggerEvaluator`] value is one registered language adapter. A set of
//! them lets the shared config layer select a language by extension — there is
//! no content sniffing, fallback, or cross-language import. Every adapter
//! reaches the same [`Trigger`] domain value with its own evaluation identity.

use std::{error::Error, fmt};

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline, EvaluationIdentity, LanguageSpec,
    Limits, Source, SourceRoot,
};

use crate::format::Trigger;
use crate::lua::LuaTriggerEvaluator;
use crate::spec::TriggerConversionError;

/// One registered trigger declaration language.
#[derive(Debug, Clone)]
pub enum TriggerEvaluator {
    Lua(LuaTriggerEvaluator),
}

/// A conversion failure from a trigger adapter.
#[derive(Debug)]
pub enum TriggerAdapterError {
    Lua(TriggerConversionError),
}

impl fmt::Display for TriggerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lua(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for TriggerAdapterError {}

impl DeclarationEvaluator<Trigger> for TriggerEvaluator {
    type Identity = EvaluationIdentity;
    type Error = TriggerAdapterError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Lua(evaluator) => <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(evaluator),
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Lua(evaluator) => <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::limits(evaluator),
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Lua(evaluator) => Self::Lua(
                <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::with_source_root(evaluator, source_root),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<Trigger, Self::Identity>, DeclarationEvaluationError<Self::Error>> {
        match self {
            Self::Lua(evaluator) => evaluator
                .evaluate_within(source, deadline)
                .map_err(|error| lift(error, TriggerAdapterError::Lua)),
        }
    }
}

/// Lift a per-adapter evaluation error into the unified dispatch error.
fn lift<E>(
    error: DeclarationEvaluationError<E>,
    wrap: impl FnOnce(E) -> TriggerAdapterError,
) -> DeclarationEvaluationError<TriggerAdapterError> {
    match error {
        DeclarationEvaluationError::Evaluation(diagnostic) => DeclarationEvaluationError::Evaluation(diagnostic),
        DeclarationEvaluationError::Conversion(error) => DeclarationEvaluationError::conversion(wrap(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_reports_its_own_registered_extension() {
        let lua = TriggerEvaluator::Lua(LuaTriggerEvaluator::default());

        assert_eq!(
            <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(&lua).extension(),
            "lua"
        );
    }
}
