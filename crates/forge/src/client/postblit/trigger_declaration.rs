//! Read-only typed declaration adapters for packaged trigger scopes.

use config::declaration::{
    ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
};
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, EvaluationDeadline,
    Evaluation, EvaluationIdentity,
    LanguageSpec, Limits, Source, SourceRoot,
};
use triggers::{
    GluonTriggerEvaluator,
    format::Trigger,
    lua::LuaTriggerEvaluator,
    registry::{TriggerAdapterError, TriggerEvaluator},
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

/// One registered transaction-trigger language (`.glu` or `.lua`), selected by
/// file extension. Both engines reach the same [`Trigger`] value.
#[derive(Debug, Clone)]
pub(super) struct TransactionTriggerEvaluator {
    trigger: TriggerEvaluator,
}

/// One registered system-trigger language (`.glu` or `.lua`), selected by file
/// extension. Both engines reach the same [`Trigger`] value.
#[derive(Debug, Clone)]
pub(super) struct SystemTriggerEvaluator {
    trigger: TriggerEvaluator,
}

macro_rules! trigger_evaluator {
    ($evaluator:ty, $config:ty) => {
        impl $evaluator {
            fn wrap(trigger: TriggerEvaluator) -> Self {
                Self { trigger }
            }
        }

        impl DeclarationEvaluator<$config> for $evaluator {
            type Identity = EvaluationIdentity;
            type Error = TriggerAdapterError;

            fn language_spec(&self) -> &LanguageSpec {
                <TriggerEvaluator as DeclarationEvaluator<Trigger>>::language_spec(
                    &self.trigger,
                )
            }

            fn limits(&self) -> Limits {
                <TriggerEvaluator as DeclarationEvaluator<Trigger>>::limits(
                    &self.trigger,
                )
            }

            fn with_source_root(&self, source_root: SourceRoot) -> Self {
                Self::wrap(
                    <TriggerEvaluator as DeclarationEvaluator<Trigger>>::with_source_root(
                        &self.trigger,
                        source_root,
                    ),
                )
            }

            fn evaluate_within(
                &self,
                source: &Source,
                deadline: EvaluationDeadline,
            ) -> Result<
                Evaluation<$config, Self::Identity>,
                DeclarationEvaluationError<Self::Error>,
            > {
                let evaluation =
                    <TriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate_within(
                        &self.trigger,
                        source,
                        deadline,
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

fn registered_engines() -> [TriggerEvaluator; 2] {
    [
        TriggerEvaluator::Gluon(GluonTriggerEvaluator::default()),
        TriggerEvaluator::Lua(LuaTriggerEvaluator::default()),
    ]
}

pub(super) fn transaction_evaluators(
) -> DeclarationEvaluatorSet<TransactionTriggerEvaluator> {
    DeclarationEvaluatorSet::new(
        registered_engines().map(TransactionTriggerEvaluator::wrap),
    )
    .expect("the transaction-trigger languages register distinct extensions")
}

pub(super) fn system_evaluators(
) -> DeclarationEvaluatorSet<SystemTriggerEvaluator> {
    DeclarationEvaluatorSet::new(
        registered_engines().map(SystemTriggerEvaluator::wrap),
    )
    .expect("the system-trigger languages register distinct extensions")
}
