//! Language-neutral contracts and policy for declarative configuration.

mod content_hash;
mod deadline;
mod diagnostic;
mod evaluation;
mod language;
mod limits;
mod module_graph;
mod source;

pub use deadline::EvaluationDeadline;
pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use evaluation::{
    EngineAdapter, Evaluation, IdentityInputs, TypedDecoder, evaluate, evaluate_file,
    evaluate_with_inputs,
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
