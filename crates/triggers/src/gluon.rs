//! Versioned restricted Gluon boundary for triggers.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation as DeclarationEvaluation,
    LanguageSpec, Limits, SourceRoot,
};
use gluon_config::{
    Diagnostic, Evaluation as GluonEvaluation, EvaluationIdentity,
    GluonEngine, Source,
};
use thiserror::Error;

use crate::{
    HandlerSpec, InhibitorsSpec, KeyValueSpec, PathDefinitionSpec, PathKindSpec, TriggerConversionError, TriggerSpec,
    format::Trigger,
};

pub const TRIGGER_ABI_VERSION: u32 = 1;
pub const GLUON_TRIGGER_ABI: &str = include_str!("../gluon/trigger.glu");

/// Owned trigger conversion failures after Gluon has evaluated successfully.
///
/// Keeping these failures separate lets the language-neutral declaration
/// loader distinguish engine diagnostics from an invalid owned trigger.
#[derive(Debug, Error)]
pub enum GluonTriggerConversionError {
    #[error(transparent)]
    Trigger(#[from] TriggerConversionError),
    #[error("trigger source must explicitly import `cast.trigger.v1`")]
    MissingAbiImport,
}

/// Stateful read-only adapter for the Gluon trigger declaration boundary.
///
/// The embedded ABI is installed once when the adapter is created. Rooting a
/// clone for fragment evaluation therefore preserves the exact ABI catalog,
/// limits, and language descriptor.
#[derive(Debug, Clone)]
pub struct GluonTriggerEvaluator {
    engine: GluonEngine,
}

impl GluonTriggerEvaluator {
    pub fn new(engine: GluonEngine) -> Result<Self, Diagnostic> {
        let mut import_policy = engine.import_policy().clone();
        import_policy.insert_embedded_module("cast.trigger.v1", GLUON_TRIGGER_ABI)?;
        Ok(Self {
            engine: engine.with_import_policy(import_policy),
        })
    }
}

impl Default for GluonTriggerEvaluator {
    fn default() -> Self {
        Self::new(GluonEngine::default())
            .expect("the embedded cast.trigger.v1 ABI is a valid Gluon module")
    }
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

impl DeclarationEvaluator<Trigger> for GluonTriggerEvaluator {
    type Identity = EvaluationIdentity;
    type Error = GluonTriggerConversionError;

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
        DeclarationEvaluation<Trigger, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        <Self as DeclarationInputEvaluator<Trigger>>::evaluate_with_inputs(
            self,
            source,
            &[],
        )
    }
}

impl DeclarationInputEvaluator<Trigger> for GluonTriggerEvaluator {
    fn evaluate_with_inputs(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<Trigger, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs::<GluonTriggerSpec>(source, explicit_inputs)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        convert_evaluation(evaluation)
            .map_err(DeclarationEvaluationError::Conversion)
    }
}

fn convert_evaluation(
    evaluation: GluonEvaluation<GluonTriggerSpec>,
) -> Result<
    DeclarationEvaluation<Trigger, EvaluationIdentity>,
    GluonTriggerConversionError,
> {
    if !evaluation
        .identity
        .modules
        .iter()
        .any(|module| module.logical_name == "cast.trigger.v1")
    {
        return Err(GluonTriggerConversionError::MissingAbiImport);
    }
    let trigger = Trigger::try_from(TriggerSpec::from(evaluation.value))?;

    Ok(DeclarationEvaluation {
        value: trigger,
        identity: evaluation.identity,
    })
}
