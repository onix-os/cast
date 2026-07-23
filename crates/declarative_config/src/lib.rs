//! Language-neutral contracts and policy for declarative configuration.

mod deadline;
mod diagnostic;
mod limits;
mod source;

#[doc(hidden)]
pub mod source_access;

pub use deadline::EvaluationDeadline;
pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use limits::Limits;
pub use source::{Source, SourceRoot};
