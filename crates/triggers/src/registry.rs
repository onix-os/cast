//! Language dispatch for the trigger domain.
//!
//! One [`TriggerEvaluator`] value is one registered language adapter. A set of
//! them lets the shared config layer select `.glu` or `.lua` by extension —
//! there is no content sniffing, fallback, or cross-language import. Both
//! adapters reach the same [`Trigger`] domain value with intentionally distinct
//! evaluation identities.

use std::{error::Error, fmt};

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};

use crate::format::Trigger;
use crate::gluon::{GluonTriggerConversionError, GluonTriggerEvaluator};
use crate::lua::LuaTriggerEvaluator;
use crate::spec::TriggerConversionError;

/// One registered trigger declaration language.
#[derive(Debug, Clone)]
pub enum TriggerEvaluator {
    Gluon(GluonTriggerEvaluator),
    Lua(LuaTriggerEvaluator),
}

/// A conversion failure from either trigger adapter.
#[derive(Debug)]
pub enum TriggerAdapterError {
    Gluon(GluonTriggerConversionError),
    Lua(TriggerConversionError),
}

impl fmt::Display for TriggerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gluon(error) => write!(formatter, "{error}"),
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
            Self::Gluon(evaluator) => {
                <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(evaluator)
            }
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(evaluator) => {
                <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::limits(evaluator)
            }
            Self::Lua(evaluator) => {
                <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::limits(evaluator)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(evaluator) => Self::Gluon(
                <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::with_source_root(
                    evaluator,
                    source_root,
                ),
            ),
            Self::Lua(evaluator) => Self::Lua(
                <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::with_source_root(
                    evaluator,
                    source_root,
                ),
            ),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        Evaluation<Trigger, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        match self {
            Self::Gluon(evaluator) => evaluator
                .evaluate_within(source, deadline)
                .map_err(|error| lift(error, TriggerAdapterError::Gluon)),
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
        DeclarationEvaluationError::Evaluation(diagnostic) => {
            DeclarationEvaluationError::Evaluation(diagnostic)
        }
        DeclarationEvaluationError::Conversion(error) => {
            DeclarationEvaluationError::conversion(wrap(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_reports_its_own_registered_extension() {
        let gluon = TriggerEvaluator::Gluon(GluonTriggerEvaluator::default());
        let lua = TriggerEvaluator::Lua(LuaTriggerEvaluator::default());

        assert_eq!(
            <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(&gluon).extension(),
            "glu"
        );
        assert_eq!(
            <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(&lua).extension(),
            "lua"
        );
        // The two engines are distinct, so their identities will differ.
        assert_ne!(
            <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(&gluon)
                .engine()
                .implementation(),
            <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(&lua)
                .engine()
                .implementation(),
        );
    }
}
