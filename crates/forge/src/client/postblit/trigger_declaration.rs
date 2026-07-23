//! Read-only typed declaration adapters for packaged trigger scopes.

use config::declaration::{
    ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
};
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation,
    LanguageSpec, Limits, Source, SourceRoot,
};
use gluon_config::EvaluationIdentity;
use triggers::{
    GluonTriggerConversionError, GluonTriggerEvaluator,
    format::Trigger,
};

/// Transaction triggers loaded from `tx.glu` and `tx.d/*.glu`.
#[derive(Debug)]
pub(super) struct TransactionTrigger(pub(super) Trigger);

impl config::Config for TransactionTrigger {
    fn domain() -> String {
        "tx".into()
    }
}

impl From<Trigger> for TransactionTrigger {
    fn from(trigger: Trigger) -> Self {
        Self(trigger)
    }
}

/// System triggers loaded from `sys.glu` and `sys.d/*.glu`.
#[derive(Debug)]
pub(super) struct SystemTrigger(pub(super) Trigger);

impl config::Config for SystemTrigger {
    fn domain() -> String {
        "sys".into()
    }
}

impl From<Trigger> for SystemTrigger {
    fn from(trigger: Trigger) -> Self {
        Self(trigger)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct TransactionTriggerEvaluator {
    trigger: GluonTriggerEvaluator,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SystemTriggerEvaluator {
    trigger: GluonTriggerEvaluator,
}

macro_rules! trigger_evaluator {
    ($evaluator:ty, $config:ty) => {
        impl DeclarationEvaluator<$config> for $evaluator {
            type Identity = EvaluationIdentity;
            type Error = GluonTriggerConversionError;

            fn language_spec(&self) -> &LanguageSpec {
                <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(
                    &self.trigger,
                )
            }

            fn limits(&self) -> Limits {
                <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::limits(
                    &self.trigger,
                )
            }

            fn with_source_root(&self, source_root: SourceRoot) -> Self {
                Self {
                    trigger: <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::with_source_root(
                        &self.trigger,
                        source_root,
                    ),
                }
            }

            fn evaluate(
                &self,
                source: &Source,
            ) -> Result<
                Evaluation<$config, Self::Identity>,
                DeclarationEvaluationError<Self::Error>,
            > {
                let evaluation =
                    <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
                        &self.trigger,
                        source,
                    )?;
                Ok(Evaluation {
                    value: <$config>::from(evaluation.value),
                    identity: evaluation.identity,
                })
            }
        }

        impl ConfigDeclarationEvaluator for $evaluator {
            type Config = $config;
        }
    };
}

trigger_evaluator!(TransactionTriggerEvaluator, TransactionTrigger);
trigger_evaluator!(SystemTriggerEvaluator, SystemTrigger);

pub(super) fn transaction_evaluators(
) -> DeclarationEvaluatorSet<TransactionTriggerEvaluator> {
    DeclarationEvaluatorSet::new([TransactionTriggerEvaluator::default()])
        .expect("one canonical transaction-trigger language is registered")
}

pub(super) fn system_evaluators(
) -> DeclarationEvaluatorSet<SystemTriggerEvaluator> {
    DeclarationEvaluatorSet::new([SystemTriggerEvaluator::default()])
        .expect("one canonical system-trigger language is registered")
}
