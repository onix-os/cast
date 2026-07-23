//! Language-neutral contracts and policy for declarative configuration.

mod content_hash;
mod deadline;
mod diagnostic;
mod limits;
mod module_graph;
mod source;

pub use deadline::EvaluationDeadline;
pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use limits::Limits;
pub use module_graph::{
    AbiCatalog, ImportRequest, ModuleClass, ModuleView, NormalizedRelative, PreparedDependency,
    PreparedGraph, PreparedModule, PreparedModuleFingerprint, prepare_module_graph,
};
pub use source::{Source, SourceRoot};
