//! Restricted Gluon evaluation for repository-owned declarative configuration.
//!
//! This crate intentionally constructs an empty [`gluon::RootedThread`]. It
//! avoids convenience VM builders and ambient importers because those expose
//! host I/O primitives and process-wide import paths.

mod decoder;
mod diagnostic;
mod engine;
mod evaluator;
mod fingerprint;
mod import;
mod runtime;

pub use declarative_config::{Diagnostic, DiagnosticCategory, LimitKind, Limits, Source, SourceRoot, SourceSpan};
pub use engine::{Evaluation, GLUON_GENERATED_MARKER, GluonEngine};
pub use evaluator::Evaluator;
pub use fingerprint::{EvaluationFingerprint, EvaluationFingerprintValidationError, ModuleFingerprint};
pub use import::ImportPolicy;

/// The exact Gluon release which defines this evaluator's language behavior.
pub const GLUON_VERSION: &str = "0.18.3";

/// Version of the Rust/Gluon configuration boundary.
pub const CONFIGURATION_ABI_VERSION: u32 = 1;

/// Version of the evaluator's security and determinism policy.
pub const EVALUATOR_POLICY_VERSION: u32 = 1;
