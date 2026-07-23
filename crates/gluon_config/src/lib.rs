//! Restricted Gluon evaluation for repository-owned declarative configuration.
//!
//! This crate intentionally constructs an empty [`gluon::RootedThread`]. It
//! avoids convenience VM builders and ambient importers because those expose
//! host I/O primitives and process-wide import paths.

mod decoder;
mod diagnostic;
mod engine;
mod import;
mod runtime;

use declarative_config::{AbiId, EvaluatorPolicyId};
pub use declarative_config::{
    Diagnostic, DiagnosticCategory, EvaluationIdentity,
    EvaluationIdentityValidationError, IdentityDependency, IdentityModule,
    LimitKind, Limits, ModuleClass, Source, SourceRoot, SourceSpan,
};
pub use engine::{Evaluation, GLUON_GENERATED_MARKER, GluonEngine};
pub use import::ImportPolicy;

/// The exact Gluon release which defines this evaluator's language behavior.
pub const GLUON_VERSION: &str = "0.18.3";

/// Version of the Rust/Gluon configuration boundary.
pub const CONFIGURATION_ABI_VERSION: u32 = 1;

/// Version of the evaluator's security and determinism policy.
pub const EVALUATOR_POLICY_VERSION: u32 = 1;

/// The neutral configuration-ABI descriptor this adapter commits to in every
/// evaluation identity. Its semantic version is independent of the Gluon engine
/// version and does not change merely because the engine changes.
pub(crate) fn gluon_configuration_abi() -> AbiId {
    AbiId::new("cast.configuration", CONFIGURATION_ABI_VERSION.to_string())
        .expect("the configuration ABI descriptor is canonical")
}

/// The neutral evaluator-policy descriptor for this adapter's security and
/// determinism rules.
pub(crate) fn gluon_evaluator_policy() -> EvaluatorPolicyId {
    EvaluatorPolicyId::new(EVALUATOR_POLICY_VERSION.to_string())
        .expect("the evaluator policy descriptor is canonical")
}
