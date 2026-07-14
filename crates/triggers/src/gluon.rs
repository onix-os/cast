//! Versioned restricted Gluon boundary for triggers.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};
use thiserror::Error;

use crate::{
    HandlerSpec, InhibitorsSpec, KeyValueSpec, PathDefinitionSpec, PathKindSpec, TriggerConversionError, TriggerSpec,
    format::Trigger,
};

pub const TRIGGER_ABI_VERSION: u32 = 1;
pub const GLUON_TRIGGER_ABI: &str = include_str!("../gluon/trigger.glu");

#[derive(Debug)]
pub struct EvaluatedTrigger {
    pub trigger: Trigger,
    pub fingerprint: EvaluationFingerprint,
}

#[derive(Debug, Error)]
pub enum TriggerEvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] TriggerConversionError),
    #[error("trigger source must explicitly import `cast.trigger.v1`")]
    MissingAbiImport,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    Unset,
    Set(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTriggerSpec {
    name: String,
    description: String,
    before: GluonOptional<String>,
    after: GluonOptional<String>,
    inhibitors: GluonOptional<GluonInhibitorsSpec>,
    paths: Vec<GluonKeyValueSpec<GluonPathDefinitionSpec>>,
    handlers: Vec<GluonKeyValueSpec<GluonHandlerSpec>>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonInhibitorsSpec {
    paths: Vec<String>,
    environment: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPathDefinitionSpec {
    handlers: Vec<String>,
    kind: GluonOptional<GluonPathKindSpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonPathKindSpec {
    Directory,
    Symlink,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonHandlerSpec {
    Run { command: String, args: Vec<String> },
    Delete { paths: Vec<String> },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonKeyValueSpec<T> {
    key: String,
    value: T,
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::Unset => None,
            GluonOptional::Set(value) => Some(value),
        }
    }
}

impl From<GluonTriggerSpec> for TriggerSpec {
    fn from(spec: GluonTriggerSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            before: spec.before.into(),
            after: spec.after.into(),
            inhibitors: Option::<GluonInhibitorsSpec>::from(spec.inhibitors).map(Into::into),
            paths: spec.paths.into_iter().map(Into::into).collect(),
            handlers: spec.handlers.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonInhibitorsSpec> for InhibitorsSpec {
    fn from(spec: GluonInhibitorsSpec) -> Self {
        Self {
            paths: spec.paths,
            environment: spec.environment,
        }
    }
}

impl From<GluonPathDefinitionSpec> for PathDefinitionSpec {
    fn from(spec: GluonPathDefinitionSpec) -> Self {
        Self {
            handlers: spec.handlers,
            kind: Option::<GluonPathKindSpec>::from(spec.kind).map(Into::into),
        }
    }
}

impl From<GluonPathKindSpec> for PathKindSpec {
    fn from(spec: GluonPathKindSpec) -> Self {
        match spec {
            GluonPathKindSpec::Directory => Self::Directory,
            GluonPathKindSpec::Symlink => Self::Symlink,
        }
    }
}

impl From<GluonHandlerSpec> for HandlerSpec {
    fn from(spec: GluonHandlerSpec) -> Self {
        match spec {
            GluonHandlerSpec::Run { command, args } => Self::Run { command, args },
            GluonHandlerSpec::Delete { paths } => Self::Delete { paths },
        }
    }
}

impl<T, U> From<GluonKeyValueSpec<T>> for KeyValueSpec<U>
where
    U: From<T>,
{
    fn from(spec: GluonKeyValueSpec<T>) -> Self {
        Self {
            key: spec.key,
            value: spec.value.into(),
        }
    }
}

pub fn evaluate_gluon(source: &Source) -> Result<EvaluatedTrigger, TriggerEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

pub fn evaluate_gluon_with(evaluator: &Evaluator, source: &Source) -> Result<EvaluatedTrigger, TriggerEvaluationError> {
    evaluate_gluon_with_inputs(evaluator, source, &[])
}

pub fn evaluate_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedTrigger, TriggerEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.insert_embedded_module("cast.trigger.v1", GLUON_TRIGGER_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonTriggerSpec>(source, explicit_inputs)?;
    if !evaluation
        .fingerprint
        .imported_modules
        .iter()
        .any(|module| module.logical_name == "cast.trigger.v1")
    {
        return Err(TriggerEvaluationError::MissingAbiImport);
    }
    let trigger = Trigger::try_from(TriggerSpec::from(evaluation.value))?;

    Ok(EvaluatedTrigger {
        trigger,
        fingerprint: evaluation.fingerprint,
    })
}
