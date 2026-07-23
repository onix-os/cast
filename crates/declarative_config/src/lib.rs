//! Language-neutral contracts and policy for declarative configuration.

mod diagnostic;
mod source;

#[doc(hidden)]
pub mod source_access;

pub use diagnostic::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
pub use source::{Source, SourceRoot};
