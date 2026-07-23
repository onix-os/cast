//! Language-neutral contracts and policy for declarative configuration.

mod content_hash;
mod deadline;
mod declaration_error;
mod diagnostic;
mod evaluation;
mod identity;
mod language;
mod limits;
mod module_graph;
mod source;

pub use deadline::EvaluationDeadline;
pub use declaration_error::DeclarationEvaluationError;
pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use evaluation::{
    DeclarationCodec, DeclarationEvaluator, DeclarationInputEvaluator,
    EngineAdapter, Evaluation, IdentityInputs, TypedDecoder, evaluate,
    evaluate_file, evaluate_with_inputs, evaluate_with_inputs_within,
    evaluate_within,
};
pub use identity::{
    EvaluationIdentity, EvaluationIdentityValidationError, IdentityDependency,
    IdentityModule,
};
pub use language::{
    AbiId, DescriptorError, EngineId, EvaluatorPolicyId, LanguageId, LanguageSpec,
};
pub use limits::Limits;
pub use module_graph::{
    AbiCatalog, ImportRequest, ModuleClass, ModuleView, NormalizedRelative, PreparedDependency,
    PreparedGraph, PreparedModule, PreparedModuleFingerprint, prepare_module_graph,
};
pub use source::{Source, SourceRoot};
